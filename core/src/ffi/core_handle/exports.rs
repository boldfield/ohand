//! The C ABI of the core handle: open, cancel, close, event callback and the store check.
//!
//! * Handles are `u64` registry keys (see `instance`); `0` is never a valid handle.
//! * Every call returns an `OhandCoreResult`. Status 0 is success; 1-5 are the normalized
//!   classes `transient`, `permanent`, `unauthorized`, `cancelled`, `unsupported`, and the
//!   buffer then holds `{"class","code","message"}` JSON with fixed content-free text.
//! * Result buffers are allocated by Rust and released exactly once with
//!   `ohand_core_result_free`, which clears the struct so a repeated free is harmless. No
//!   other string or buffer is ever handed to the caller.
//! * Event payloads passed to the callback are borrowed for the duration of the call; the
//!   callback copies what it needs. The callback must not unwind.
//! * Threading: events are delivered on the core's worker thread (named `ohand-core-worker`),
//!   one at a time in submission order, never on the caller's thread and never on the main
//!   thread. UI-facing effects must be hopped to the UI thread by the receiver. After
//!   `ohand_core_cancel`, `ohand_core_set_event_callback` or `ohand_core_close` returns, no
//!   callback is running and none will start; `context` may then be released. From inside a
//!   callback, `ohand_core_cancel` is allowed (the running callback is the last), while
//!   `ohand_core_close` and `ohand_core_set_event_callback` are rejected as `reentrant_call`.
//! * Panics never cross the boundary; they become the `internal` failure.

use super::failure::AbiFailure;
use super::instance::{self, EventCallbackFn, JobFn};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Longest store path accepted, in bytes.
pub const OHAND_CORE_MAX_PATH_BYTES: usize = 4096;

pub type OhandCoreHandle = u64;

/// Called on the core's worker thread with the outcome of operation `operation_id`: `status`
/// 0 and a JSON result, or a failure status and JSON error payload. `data` is borrowed for
/// the duration of the call.
pub type OhandCoreEventCallback = Option<
    unsafe extern "C" fn(
        context: *mut c_void,
        operation_id: u64,
        status: u32,
        data: *const u8,
        len: usize,
    ),
>;

/// Outcome of a call. `data` points at `len` bytes owned by Rust until `ohand_core_result_free`;
/// an empty success has a null `data`.
#[repr(C)]
pub struct OhandCoreResult {
    pub status: u32,
    pub data: *mut u8,
    pub len: usize,
}

static LIVE_RESULT_BUFFERS: AtomicUsize = AtomicUsize::new(0);

fn into_result(status: u32, bytes: Vec<u8>) -> OhandCoreResult {
    if bytes.is_empty() {
        return OhandCoreResult {
            status,
            data: std::ptr::null_mut(),
            len: 0,
        };
    }
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let data = Box::into_raw(boxed) as *mut u8;
    LIVE_RESULT_BUFFERS.fetch_add(1, Ordering::SeqCst);
    OhandCoreResult { status, data, len }
}

fn guarded(operation: impl FnOnce() -> Result<(), AbiFailure>) -> OhandCoreResult {
    let outcome = catch_unwind(AssertUnwindSafe(operation)).unwrap_or(Err(AbiFailure::INTERNAL));
    match outcome {
        Ok(()) => into_result(super::failure::OHAND_CORE_STATUS_OK, Vec::new()),
        Err(failure) => into_result(failure.status(), failure.to_json()),
    }
}

/// Opens the core store at the UTF-8 `path` (`:memory:` for a private in-memory store) and
/// writes the new handle to `out_handle`; on failure `out_handle` is set to 0.
///
/// # Safety
/// `path` must point at `path_len` readable bytes (it may be null only when `path_len` is
/// 0), and `out_handle` must be null or point at writable memory for one `OhandCoreHandle`.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_open(
    path: *const u8,
    path_len: usize,
    out_handle: *mut OhandCoreHandle,
) -> OhandCoreResult {
    if let Some(out) = out_handle.as_mut() {
        *out = 0;
    }
    guarded(|| {
        let out = out_handle.as_mut().ok_or(AbiFailure::NULL_ARGUMENT)?;
        if path_len > OHAND_CORE_MAX_PATH_BYTES {
            return Err(AbiFailure::REQUEST_TOO_LARGE);
        }
        if path_len == 0 {
            return Err(AbiFailure::INVALID_PATH);
        }
        if path.is_null() {
            return Err(AbiFailure::NULL_ARGUMENT);
        }
        let bytes = std::slice::from_raw_parts(path, path_len);
        let path = std::str::from_utf8(bytes).map_err(|_| AbiFailure::INVALID_UTF8)?;
        *out = instance::open(path)?;
        Ok(())
    })
}

/// Cancels the core: pending work is discarded, later work is refused with `cancelled`, and
/// no callback runs after this returns. Idempotent. The handle stays valid until closed.
#[no_mangle]
pub extern "C" fn ohand_core_cancel(handle: OhandCoreHandle) -> OhandCoreResult {
    guarded(|| {
        instance::lookup(handle)?.cancel();
        Ok(())
    })
}

/// Cancels, waits for the worker to stop and releases the core. A handle that is not open
/// (never issued, already closed) is rejected with `invalid_handle`.
#[no_mangle]
pub extern "C" fn ohand_core_close(handle: OhandCoreHandle) -> OhandCoreResult {
    guarded(|| instance::close(handle))
}

/// Registers `callback` with `context` for operation outcomes, replacing any previous
/// registration; a null `callback` clears it. Waits for a running callback to return, so the
/// previous context is unused once this returns. Outcomes that complete while no callback is
/// registered are discarded.
#[no_mangle]
pub extern "C" fn ohand_core_set_event_callback(
    handle: OhandCoreHandle,
    callback: OhandCoreEventCallback,
    context: *mut c_void,
) -> OhandCoreResult {
    guarded(|| {
        let callback: Option<EventCallbackFn> = callback;
        instance::lookup(handle)?.set_callback(callback, context)
    })
}

/// Queues a read of the schema version and capture count of the core store on the worker
/// thread; the outcome arrives as an event for `operation_id` with JSON
/// `{"operation_id","schema_version","capture_count"}`. This is the smallest real
/// background operation of the lifecycle boundary.
#[no_mangle]
pub extern "C" fn ohand_core_start_store_check(
    handle: OhandCoreHandle,
    operation_id: u64,
) -> OhandCoreResult {
    guarded(|| {
        let job: JobFn = Box::new(move |database| super::store_check::run(database, operation_id));
        instance::lookup(handle)?.submit(operation_id, job)
    })
}

/// Releases the buffer in `result` and clears it. Null is ignored.
///
/// # Safety
/// `result` must be null or point at an `OhandCoreResult` returned by this library whose
/// buffer has not been released through a copy of the struct.
#[no_mangle]
pub unsafe extern "C" fn ohand_core_result_free(result: *mut OhandCoreResult) {
    if let Some(result) = result.as_mut() {
        if !result.data.is_null() {
            let slice = std::ptr::slice_from_raw_parts_mut(result.data, result.len);
            drop(Box::from_raw(slice));
            LIVE_RESULT_BUFFERS.fetch_sub(1, Ordering::SeqCst);
        }
        result.data = std::ptr::null_mut();
        result.len = 0;
    }
}

/// Number of open handles. Returns to its starting value when every handle is closed.
#[no_mangle]
pub extern "C" fn ohand_core_live_handles() -> usize {
    instance::live_handles()
}

/// Number of result buffers not yet released with `ohand_core_result_free`.
#[no_mangle]
pub extern "C" fn ohand_core_live_result_buffers() -> usize {
    LIVE_RESULT_BUFFERS.load(Ordering::SeqCst)
}
