//! Cold-launch URL delivery for scene-based iOS apps.
//!
//! When the system launches an app because of a custom-scheme URL and the app uses the scene lifecycle, the URL is
//! only handed over in `UISceneConnectionOptions.URLContexts` of `scene:willConnectToSession:options:`. tao 0.37.1's
//! `TaoSceneDelegate` ignores that field (it only forwards `scene:openURLContexts:`, which is not called on a cold
//! launch), so `RunEvent::Opened` never fires for the URL that started the app. This module wraps tao's method,
//! lets tao run unchanged, and then passes the connection options' URLs to the same native receiver.

use std::ffi::{c_char, CStr};
use std::mem;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};

type UrlHandler = dyn Fn(Vec<String>) + Send + Sync;
type WillConnect = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject);

static URL_HANDLER: OnceLock<Box<UrlHandler>> = OnceLock::new();
static ORIGINAL_WILL_CONNECT: OnceLock<Imp> = OnceLock::new();

/// Returns false when tao's scene delegate class or method is not present (not a scene-based launch path).
pub fn install(handler: impl Fn(Vec<String>) + Send + Sync + 'static) -> bool {
    let Some(delegate_class) = AnyClass::get(c"TaoSceneDelegate") else {
        return false;
    };
    let Some(method) = delegate_class.instance_method(sel!(scene:willConnectToSession:options:)) else {
        return false;
    };
    if URL_HANDLER.set(Box::new(handler)).is_err() {
        return false;
    }
    let replacement: WillConnect = will_connect_with_urls;
    // SAFETY: the replacement has the exact signature of the method it wraps and calls the original first.
    let original = unsafe { method.set_implementation(mem::transmute::<WillConnect, Imp>(replacement)) };
    ORIGINAL_WILL_CONNECT.set(original).is_ok()
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
    let urls = urls_in_connection_options(connection_options);
    if urls.is_empty() {
        return;
    }
    if let Some(handler) = URL_HANDLER.get() {
        handler(urls);
    }
}

unsafe fn urls_in_connection_options(connection_options: *mut AnyObject) -> Vec<String> {
    let mut urls = Vec::new();
    if connection_options.is_null() {
        return urls;
    }
    let url_contexts: *mut AnyObject = msg_send![connection_options, URLContexts];
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
