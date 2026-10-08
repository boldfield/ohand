//! Raw URL delivery for scene-based iOS apps.
//!
//! tao 0.37.1's `TaoSceneDelegate` handles custom-scheme URLs badly in two ways, and this module wraps both of its
//! scene methods through the Objective-C runtime (tao runs unchanged first):
//!
//! - Cold launch: the launching URL is only in `UISceneConnectionOptions.URLContexts` of
//!   `scene:willConnectToSession:options:`, which tao ignores, so `RunEvent::Opened` never fires for it.
//! - Warm launch: `scene:openURLContexts:` parses each URL into a `url::Url` before `RunEvent::Opened`, and the
//!   `url` crate normalises it (lower-cases the scheme, strips tab and newline). A URL the strict handoff grammar
//!   must reject would arrive looking canonical, and one tao cannot parse is dropped without being counted.
//!
//! Both hooks hand the receiver the unmodified `absoluteString`, so cold and warm delivery apply the same grammar to
//! the same bytes. The iOS shell therefore ignores `RunEvent::Opened` for recording.

use std::ffi::{c_char, CStr};
use std::mem;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};

/// Which scene callback delivered the URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneUrlSource {
    /// `scene:willConnectToSession:options:`, the URL that launched the app.
    Connect,
    /// `scene:openURLContexts:`, a URL opened while the app was already running.
    Open,
}

type UrlHandler = dyn Fn(SceneUrlSource, Vec<String>) + Send + Sync;
type WillConnect = unsafe extern "C-unwind" fn(
    *mut AnyObject,
    Sel,
    *mut AnyObject,
    *mut AnyObject,
    *mut AnyObject,
);
type OpenUrlContexts =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);

static URL_HANDLER: OnceLock<Box<UrlHandler>> = OnceLock::new();
static ORIGINAL_WILL_CONNECT: OnceLock<Imp> = OnceLock::new();
static ORIGINAL_OPEN_URL_CONTEXTS: OnceLock<Imp> = OnceLock::new();

/// Wraps both scene methods. Returns false, changing nothing, when tao's scene delegate class or either method is
/// missing (not a scene-based launch path, or a tao upgrade renamed them).
pub fn install(handler: impl Fn(SceneUrlSource, Vec<String>) + Send + Sync + 'static) -> bool {
    let Some(delegate_class) = AnyClass::get(c"TaoSceneDelegate") else {
        return false;
    };
    let Some(connect_method) =
        delegate_class.instance_method(sel!(scene:willConnectToSession:options:))
    else {
        return false;
    };
    let Some(open_method) = delegate_class.instance_method(sel!(scene:openURLContexts:)) else {
        return false;
    };
    if URL_HANDLER.set(Box::new(handler)).is_err() {
        return false;
    }
    let connect_replacement: WillConnect = will_connect_with_urls;
    let open_replacement: OpenUrlContexts = open_url_contexts_with_urls;
    // SAFETY: each replacement has the exact signature of the method it wraps and calls the original first.
    let (original_connect, original_open) = unsafe {
        (
            connect_method
                .set_implementation(mem::transmute::<WillConnect, Imp>(connect_replacement)),
            open_method
                .set_implementation(mem::transmute::<OpenUrlContexts, Imp>(open_replacement)),
        )
    };
    ORIGINAL_WILL_CONNECT.set(original_connect).is_ok()
        && ORIGINAL_OPEN_URL_CONTEXTS.set(original_open).is_ok()
}

fn deliver(source: SceneUrlSource, urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }
    if let Some(handler) = URL_HANDLER.get() {
        handler(source, urls);
    }
}

unsafe extern "C-unwind" fn will_connect_with_urls(
    this: *mut AnyObject,
    selector: Sel,
    scene: *mut AnyObject,
    session: *mut AnyObject,
    connection_options: *mut AnyObject,
) {
    if let Some(original) = ORIGINAL_WILL_CONNECT.get() {
        let original: WillConnect = mem::transmute::<Imp, WillConnect>(*original);
        original(this, selector, scene, session, connection_options);
    }
    let url_contexts: *mut AnyObject = if connection_options.is_null() {
        std::ptr::null_mut()
    } else {
        msg_send![connection_options, URLContexts]
    };
    deliver(SceneUrlSource::Connect, urls_in_contexts(url_contexts));
}

unsafe extern "C-unwind" fn open_url_contexts_with_urls(
    this: *mut AnyObject,
    selector: Sel,
    scene: *mut AnyObject,
    url_contexts: *mut AnyObject,
) {
    if let Some(original) = ORIGINAL_OPEN_URL_CONTEXTS.get() {
        let original: OpenUrlContexts = mem::transmute::<Imp, OpenUrlContexts>(*original);
        original(this, selector, scene, url_contexts);
    }
    deliver(SceneUrlSource::Open, urls_in_contexts(url_contexts));
}

/// `url_contexts` is an `NSSet<UIOpenURLContext>`; the result is each URL's unmodified `absoluteString`.
unsafe fn urls_in_contexts(url_contexts: *mut AnyObject) -> Vec<String> {
    let mut urls = Vec::new();
    if url_contexts.is_null() {
        return urls;
    }
    let contexts: *mut AnyObject = msg_send![url_contexts, allObjects];
    if contexts.is_null() {
        return urls;
    }
    let count: usize = msg_send![contexts, count];
    for index in 0..count {
        let context: *mut AnyObject = msg_send![contexts, objectAtIndex: index];
        if context.is_null() {
            continue;
        }
        let url: *mut AnyObject = msg_send![context, URL];
        if url.is_null() {
            continue;
        }
        let absolute_string: *mut AnyObject = msg_send![url, absoluteString];
        if absolute_string.is_null() {
            continue;
        }
        let utf8: *const c_char = msg_send![absolute_string, UTF8String];
        if utf8.is_null() {
            continue;
        }
        urls.push(CStr::from_ptr(utf8).to_string_lossy().into_owned());
    }
    urls
}
