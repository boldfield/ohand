//! C ABI of the P01 Rust-to-Swift boundary probe.
//!
//! This file holds the P01 probe exports; the C header is generated at build time by
//! `tools/bindings` from this crate and the `ffi` modules of `ohand-core`. Contract:
//!
//! * Requests and responses are UTF-8 JSON byte buffers with explicit lengths, so embedded
//!   NUL bytes never truncate content and no function relies on NUL termination.
//! * Every call returns an `OhandResult`. Status 0 carries the response; any other status is
//!   a normalized error class and the buffer holds a content-free JSON error payload.
//! * The buffer in an `OhandResult` is owned by Rust and must be released exactly once with
//!   `ohand_result_free`. Opaque handles are released with their matching `_free` function.
//! * Calls are synchronous. Cancellation is cooperative through an `OhandCancelToken`; see
//!   `probe::Checkpoint`. Nothing is mutated when a call reports `cancelled`.
//! * Panics never cross the boundary; they are reported as the `internal` failure.

pub mod probe;

use probe::{
    check_request_length, decode_utf8, get_capture_json, save_capture_json, CancelToken, Failure,
    ProbeStore, OHAND_MAX_REQUEST_BYTES,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const OHAND_BINDINGS_ABI_VERSION: u32 = 1;

pub const OHAND_STATUS_OK: u32 = 0;
pub const OHAND_STATUS_TRANSIENT: u32 = 1;
pub const OHAND_STATUS_PERMANENT: u32 = 2;
pub const OHAND_STATUS_UNAUTHORIZED: u32 = 3;
pub const OHAND_STATUS_CANCELLED: u32 = 4;
pub const OHAND_STATUS_UNSUPPORTED: u32 = 5;

static LIVE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

fn track_allocation() {
    LIVE_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
}

fn track_release() {
    LIVE_ALLOCATIONS.fetch_sub(1, Ordering::SeqCst);
}

/// Opaque in-memory capture store backed by the real core database.
pub struct OhandProbeStore {
    store: ProbeStore,
}

/// Opaque cooperative cancellation flag.
pub struct OhandCancelToken {
    token: CancelToken,
}

/// Outcome of a call. `data` points at `len` bytes owned by Rust until `ohand_result_free`.
#[repr(C)]
pub struct OhandResult {
    pub status: u32,
    pub data: *mut u8,
    pub len: usize,
}

fn into_result(status: u32, bytes: Vec<u8>) -> OhandResult {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    let data = Box::into_raw(boxed) as *mut u8;
    track_allocation();
    OhandResult { status, data, len }
}

fn failure_result(failure: Failure) -> OhandResult {
    into_result(failure.class.status(), failure.to_json())
}

fn guarded(operation: impl FnOnce() -> Result<Vec<u8>, Failure>) -> OhandResult {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(bytes)) => into_result(OHAND_STATUS_OK, bytes),
        Ok(Err(failure)) => failure_result(failure),
        Err(_) => failure_result(Failure::INTERNAL),
    }
}

/// Borrows `len` bytes at `data`. The length is checked against the bound first, so an
/// oversized or inconsistent request is rejected without reading any caller memory.
unsafe fn borrow_bytes<'a>(data: *const u8, len: usize) -> Result<&'a [u8], Failure> {
    check_request_length(len)?;
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(Failure::NULL_ARGUMENT);
    }
    Ok(std::slice::from_raw_parts(data, len))
}

unsafe fn borrow_token<'a>(cancel_token: *const OhandCancelToken) -> Option<&'a CancelToken> {
    cancel_token.as_ref().map(|handle| &handle.token)
}

/// Version of this C ABI; bumped on any incompatible change.
#[no_mangle]
pub extern "C" fn ohand_bindings_abi_version() -> u32 {
    OHAND_BINDINGS_ABI_VERSION
}

/// Largest `request_len` the boundary accepts.
#[no_mangle]
pub extern "C" fn ohand_bindings_max_request_bytes() -> usize {
    OHAND_MAX_REQUEST_BYTES
}

/// Number of live Rust-owned objects (result buffers, stores, cancel tokens). Returns to its
/// starting value when every object has been released; tests use it to prove ownership.
#[no_mangle]
pub extern "C" fn ohand_bindings_live_allocations() -> usize {
    LIVE_ALLOCATIONS.load(Ordering::SeqCst)
}

/// Opens an empty in-memory core capture store. Returns null if the store cannot be opened.
/// Release with `ohand_probe_store_free`.
#[no_mangle]
pub extern "C" fn ohand_probe_store_open_in_memory() -> *mut OhandProbeStore {
    match catch_unwind(ProbeStore::open_in_memory) {
        Ok(Ok(store)) => {
            track_allocation();
            Box::into_raw(Box::new(OhandProbeStore { store }))
        }
        _ => std::ptr::null_mut(),
    }
}

/// Releases a store. Null is ignored.
///
/// # Safety
/// `store` must be null or a pointer from `ohand_probe_store_open_in_memory` that has not
/// been freed, and no call may be using it concurrently or afterwards.
#[no_mangle]
pub unsafe extern "C" fn ohand_probe_store_free(store: *mut OhandProbeStore) {
    if !store.is_null() {
        drop(Box::from_raw(store));
        track_release();
    }
}

/// Creates an uncancelled token. Release with `ohand_cancel_token_free`.
#[no_mangle]
pub extern "C" fn ohand_cancel_token_new() -> *mut OhandCancelToken {
    track_allocation();
    Box::into_raw(Box::new(OhandCancelToken {
        token: CancelToken::default(),
    }))
}

/// Requests cancellation. Safe to call from any thread, repeatedly, and after the call it
/// was meant for has finished. Null is ignored.
///
/// # Safety
/// `cancel_token` must be null or a live pointer from `ohand_cancel_token_new`.
#[no_mangle]
pub unsafe extern "C" fn ohand_cancel_token_cancel(cancel_token: *const OhandCancelToken) {
    if let Some(handle) = cancel_token.as_ref() {
        handle.token.cancel();
    }
}

/// Releases a token. Null is ignored.
///
/// # Safety
/// `cancel_token` must be null or a pointer from `ohand_cancel_token_new` that has not been
/// freed, and no call may be using it concurrently or afterwards.
#[no_mangle]
pub unsafe extern "C" fn ohand_cancel_token_free(cancel_token: *mut OhandCancelToken) {
    if !cancel_token.is_null() {
        drop(Box::from_raw(cancel_token));
        track_release();
    }
}

/// Saves the capture described by the JSON request into the store, idempotently, and returns
/// the stored capture. `cancel_token` may be null.
///
/// # Safety
/// `store` and `cancel_token` (if non-null) must be live handles; `request` must point at
/// `request_len` readable bytes (it may be null only when `request_len` is 0).
#[no_mangle]
pub unsafe extern "C" fn ohand_probe_save_capture(
    store: *const OhandProbeStore,
    cancel_token: *const OhandCancelToken,
    request: *const u8,
    request_len: usize,
) -> OhandResult {
    guarded(|| {
        let bytes = borrow_bytes(request, request_len)?;
        let store = store.as_ref().ok_or(Failure::NULL_ARGUMENT)?;
        let request_json = decode_utf8(bytes)?;
        save_capture_json(
            &store.store,
            borrow_token(cancel_token),
            request_json,
            &|_| {},
        )
    })
}

/// Returns the stored capture with the given UTF-8 identifier, or a `not_found` error.
///
/// # Safety
/// Same requirements as `ohand_probe_save_capture`, with `capture_id` and `capture_id_len`
/// in place of `request` and `request_len`.
#[no_mangle]
pub unsafe extern "C" fn ohand_probe_get_capture(
    store: *const OhandProbeStore,
    cancel_token: *const OhandCancelToken,
    capture_id: *const u8,
    capture_id_len: usize,
) -> OhandResult {
    guarded(|| {
        let bytes = borrow_bytes(capture_id, capture_id_len)?;
        let store = store.as_ref().ok_or(Failure::NULL_ARGUMENT)?;
        let capture_id = decode_utf8(bytes)?;
        get_capture_json(&store.store, borrow_token(cancel_token), capture_id)
    })
}

/// Always panics inside Rust; the call must return the `internal` failure instead of
/// unwinding into the caller. Exists only to prove panic containment.
#[no_mangle]
pub extern "C" fn ohand_probe_trigger_panic() -> OhandResult {
    guarded(|| panic!("intentional probe panic"))
}

/// Releases the buffer in `result` and clears it, so freeing twice is harmless. Null is
/// ignored.
///
/// # Safety
/// `result` must be null or point at an `OhandResult` returned by this library whose buffer
/// has not been released through a copy of the struct.
#[no_mangle]
pub unsafe extern "C" fn ohand_result_free(result: *mut OhandResult) {
    if let Some(result) = result.as_mut() {
        if !result.data.is_null() {
            let slice = std::ptr::slice_from_raw_parts_mut(result.data, result.len);
            drop(Box::from_raw(slice));
            track_release();
        }
        result.data = std::ptr::null_mut();
        result.len = 0;
    }
}
