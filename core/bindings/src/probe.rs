//! Boundary-independent probe logic: request parsing, bounds, cancellation checkpoints and
//! the call into the real `ohand-core` capture store. Nothing here touches raw pointers.

use ohand_core::store::captures::{get_capture, save_capture_in_tx, Capture};
use ohand_core::store::schema::{Database, SystemClock};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Largest request (and therefore largest capture text) accepted across the boundary.
pub const OHAND_MAX_REQUEST_BYTES: usize = 1_048_576;

/// Normalized error classes from the M1 contract (`transient` and `unauthorized` are reserved
/// for effects that can produce them; the probe only emits the other three).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Transient,
    Permanent,
    Unauthorized,
    Cancelled,
    Unsupported,
}

impl ErrorClass {
    pub fn status(self) -> u32 {
        match self {
            ErrorClass::Transient => 1,
            ErrorClass::Permanent => 2,
            ErrorClass::Unauthorized => 3,
            ErrorClass::Cancelled => 4,
            ErrorClass::Unsupported => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ErrorClass::Transient => "transient",
            ErrorClass::Permanent => "permanent",
            ErrorClass::Unauthorized => "unauthorized",
            ErrorClass::Cancelled => "cancelled",
            ErrorClass::Unsupported => "unsupported",
        }
    }
}

/// A normalized failure. Messages are fixed strings and never echo caller content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub class: ErrorClass,
    pub code: &'static str,
    pub message: &'static str,
}

impl Failure {
    const fn permanent(code: &'static str, message: &'static str) -> Self {
        Failure {
            class: ErrorClass::Permanent,
            code,
            message,
        }
    }

    pub const NULL_ARGUMENT: Failure =
        Failure::permanent("null_argument", "a required argument was null");
    pub const REQUEST_TOO_LARGE: Failure = Failure::permanent(
        "request_too_large",
        "request exceeds the boundary size limit",
    );
    pub const INVALID_UTF8: Failure =
        Failure::permanent("invalid_utf8", "request bytes are not valid UTF-8");
    pub const INVALID_REQUEST: Failure =
        Failure::permanent("invalid_request", "request is not a valid capture");
    pub const CAPTURE_CONFLICT: Failure = Failure::permanent(
        "capture_conflict",
        "a different capture already uses this identifier",
    );
    pub const NOT_FOUND: Failure =
        Failure::permanent("not_found", "no capture has this identifier");
    pub const STORAGE: Failure = Failure::permanent("storage_error", "the capture store failed");
    pub const INTERNAL: Failure = Failure::permanent("internal", "unexpected internal failure");
    pub const CANCELLED: Failure = Failure {
        class: ErrorClass::Cancelled,
        code: "cancelled",
        message: "the call was cancelled before any change was committed",
    };

    /// JSON error payload returned to the native caller.
    pub fn to_json(self) -> Vec<u8> {
        format!(
            "{{\"class\":\"{}\",\"code\":\"{}\",\"message\":\"{}\"}}",
            self.class.name(),
            self.code,
            self.message
        )
        .into_bytes()
    }
}

/// Cooperative cancellation flag shared between the native caller and one or more calls.
#[derive(Default)]
pub struct CancelToken {
    cancelled: AtomicBool,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Points inside a call at which cancellation is observed. After the commit there is no
/// checkpoint: a committed capture is never rolled back by a late cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkpoint {
    AfterValidation,
    BeforeCommit,
}

/// In-memory core capture store owned by the native caller through an opaque handle.
pub struct ProbeStore {
    database: Mutex<Database>,
}

impl ProbeStore {
    pub fn open_in_memory() -> Result<Self, Failure> {
        let database =
            Database::open(":memory:", Arc::new(SystemClock)).map_err(|_| Failure::STORAGE)?;
        Ok(ProbeStore {
            database: Mutex::new(database),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Database> {
        self.database.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CaptureWire {
    capture_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    audio_reference: Option<String>,
    capture_instant: String,
    timezone_id: String,
    utc_offset_minutes: i32,
    locale: String,
    calendar: String,
    item_scope: String,
    route_id: String,
    entry_locked: bool,
    created_at: String,
    #[serde(default)]
    session_topic: Option<String>,
}

#[derive(Serialize)]
struct SavedWire<'a> {
    capture: &'a CaptureWire,
    idempotent_replay: bool,
}

impl From<Capture> for CaptureWire {
    fn from(capture: Capture) -> Self {
        CaptureWire {
            capture_id: capture.capture_id,
            text: capture.text,
            audio_reference: capture.audio_reference,
            capture_instant: capture.capture_instant,
            timezone_id: capture.timezone_id,
            utc_offset_minutes: capture.utc_offset_minutes,
            locale: capture.locale,
            calendar: capture.calendar,
            item_scope: capture.item_scope,
            route_id: capture.route_id,
            entry_locked: capture.entry_locked,
            created_at: capture.created_at,
            session_topic: capture.session_topic,
        }
    }
}

fn capture_from_wire(wire: CaptureWire) -> Result<Capture, Failure> {
    if wire.capture_id.is_empty() {
        return Err(Failure::INVALID_REQUEST);
    }
    Capture::new(
        wire.capture_id,
        wire.text,
        wire.audio_reference,
        wire.capture_instant,
        wire.timezone_id,
        wire.utc_offset_minutes,
        wire.locale,
        wire.calendar,
        wire.item_scope,
        wire.route_id,
        wire.entry_locked,
        wire.created_at,
        wire.session_topic,
    )
    .map_err(|_| Failure::INVALID_REQUEST)
}

/// Rejects oversized input from its length alone, before any byte is read.
pub fn check_request_length(length: usize) -> Result<(), Failure> {
    if length > OHAND_MAX_REQUEST_BYTES {
        return Err(Failure::REQUEST_TOO_LARGE);
    }
    Ok(())
}

pub fn decode_utf8(bytes: &[u8]) -> Result<&str, Failure> {
    std::str::from_utf8(bytes).map_err(|_| Failure::INVALID_UTF8)
}

fn check_cancelled(cancel_token: Option<&CancelToken>) -> Result<(), Failure> {
    match cancel_token {
        Some(token) if token.is_cancelled() => Err(Failure::CANCELLED),
        _ => Ok(()),
    }
}

/// Saves the capture described by `request_json` into the core store and returns the stored
/// record. `observe` is called at each cancellation checkpoint after the token check passes
/// (tests use it to cancel at an exact point).
pub fn save_capture_json(
    store: &ProbeStore,
    cancel_token: Option<&CancelToken>,
    request_json: &str,
    observe: &dyn Fn(Checkpoint),
) -> Result<Vec<u8>, Failure> {
    check_cancelled(cancel_token)?;
    let wire: CaptureWire =
        serde_json::from_str(request_json).map_err(|_| Failure::INVALID_REQUEST)?;
    let capture = capture_from_wire(wire)?;
    observe(Checkpoint::AfterValidation);
    check_cancelled(cancel_token)?;

    let mut database = store.lock();
    let transaction = database
        .immediate_transaction()
        .map_err(|_| Failure::STORAGE)?;
    let existing = get_capture(&transaction, &capture.capture_id).map_err(|_| Failure::STORAGE)?;
    let (saved, idempotent_replay) = match existing {
        Some(existing) if existing == capture => (existing, true),
        Some(_) => return Err(Failure::CAPTURE_CONFLICT),
        None => {
            observe(Checkpoint::BeforeCommit);
            // Dropping the transaction on this early return rolls it back: no mutation.
            check_cancelled(cancel_token)?;
            let saved = save_capture_in_tx(&transaction, &capture).map_err(|_| Failure::STORAGE)?;
            transaction.commit().map_err(|_| Failure::STORAGE)?;
            (saved, false)
        }
    };
    let saved_wire = CaptureWire::from(saved);
    serde_json::to_vec(&SavedWire {
        capture: &saved_wire,
        idempotent_replay,
    })
    .map_err(|_| Failure::INTERNAL)
}

pub fn get_capture_json(
    store: &ProbeStore,
    cancel_token: Option<&CancelToken>,
    capture_id: &str,
) -> Result<Vec<u8>, Failure> {
    check_cancelled(cancel_token)?;
    let mut database = store.lock();
    let transaction = database.transaction().map_err(|_| Failure::STORAGE)?;
    let found = get_capture(&transaction, capture_id).map_err(|_| Failure::STORAGE)?;
    let capture = found.ok_or(Failure::NOT_FOUND)?;
    serde_json::to_vec(&CaptureWire::from(capture)).map_err(|_| Failure::INTERNAL)
}

/// Test marker: verifies that module-local exports in modules declared in lib.rs
/// are discovered by cbindgen and included in the generated header.
/// Removed once real production exports from later-owned modules appear in the ABI.
#[no_mangle]
pub extern "C" fn ohand_probe_module_export_test_marker() -> u32 {
    42
}
