//! Native-boundary exports owned by this module tree.
//!
//! To export an operation, add a file here (for example `ffi/capture.rs`) containing
//! `#[no_mangle] pub extern "C" fn ohand_...` functions, plus the single `pub mod capture;`
//! line below. Nothing else is edited: `core/bindings` links this crate, and `tools/bindings`
//! parses it, so the function reaches both the static library and the generated C header.
//! A module that is not declared here is exported nowhere.
//!
//! Rules enforced by `make check` and `make test` (see `docs/validation/core-binding.md`):
//! exported functions are named `ohand_*`, exported constants `OHAND_*` (other public
//! constants of this crate are never exported), types appear in the header only when an
//! exported function uses them, and no export is gated on the build target.

pub mod callback;
pub mod error;
pub mod handle;
#[cfg(test)]
mod tests;

pub use error::ErrorClass;
use handle::{HandleState, OpaqueHandle};

/// Create a new core handle for initialization/teardown testing.
/// Returns an owned opaque handle that must be destroyed with ohand_destroy_handle.
/// The handle is a single-owner pointer; the caller owns it exclusively.
#[no_mangle]
pub extern "C" fn ohand_create_handle() -> OpaqueHandle {
    let state = Box::new(HandleState::new());
    Box::into_raw(state)
}

/// Destroy a handle, releasing its resources.
/// After destruction, the pointer is invalid and must not be used.
/// Destroying a null handle is safe (no-op).
/// # Safety
/// The caller must pass a handle created by ohand_create_handle or null.
#[no_mangle]
pub unsafe extern "C" fn ohand_destroy_handle(handle: OpaqueHandle) {
    if !handle.is_null() {
        let _ = Box::from_raw(handle);
    }
}

/// Check if a handle is cancelled.
/// Returns 1 if cancelled, 0 if active, -1 if handle is invalid.
/// # Safety
/// The caller must pass a valid handle created by ohand_create_handle or null.
#[no_mangle]
pub unsafe extern "C" fn ohand_is_handle_cancelled(handle: OpaqueHandle) -> i32 {
    if handle.is_null() {
        return -1;
    }
    if (*handle).is_cancelled() {
        1
    } else {
        0
    }
}

/// Cancel a handle, preventing future callbacks and operations.
/// Returns 0 on success, -1 if handle is invalid.
/// # Safety
/// The caller must pass a valid handle created by ohand_create_handle or null.
#[no_mangle]
pub unsafe extern "C" fn ohand_cancel_handle(handle: OpaqueHandle) -> i32 {
    if handle.is_null() {
        return -1;
    }
    (*handle).cancel();
    0
}

/// Register a callback with a handle.
/// The callback will be invoked on the documented UI thread if delivery is permitted.
/// The callback is stored and invoked by background operations; it must be thread-safe.
/// Returns 0 on success, -1 if handle is invalid.
/// # Safety
/// The caller must pass a valid handle created by ohand_create_handle or null.
#[no_mangle]
pub unsafe extern "C" fn ohand_register_callback(
    handle: OpaqueHandle,
    callback: handle::CallbackFn,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    match (*handle).register_callback(callback) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
