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

/// Invoke a registered callback if one exists and the handle is not cancelled.
/// Returns 0 if callback was invoked, 1 if cancelled, 2 if no callback registered, -1 if handle is invalid.
/// # Safety
/// The caller must pass a valid handle created by ohand_create_handle or null, and a valid closure pointer.
#[no_mangle]
pub unsafe extern "C" fn ohand_invoke_callback(
    handle: OpaqueHandle,
    closure: *const std::ffi::c_void,
) -> i32 {
    if handle.is_null() {
        return -1;
    }

    match (*handle).invoke_callback_if_active(closure) {
        Ok(true) => 0, // callback was invoked
        Ok(false) => {
            if (*handle).is_cancelled() {
                1 // cancelled
            } else {
                2 // no callback registered
            }
        }
        Err(_) => -1, // error (e.g., mutex poisoned)
    }
}

/// Classify an error into a normalized ErrorClass for cross-ABI delivery.
/// Converts an error description string into a normalized error class.
/// The string is expected to be a simple error message (typically the Display representation).
/// Returns the ErrorClass as an i32 (0=Ok, 1=Transient, 2=Permanent, 3=Unauthorized, 4=Cancelled, 5=Unsupported).
/// # Safety
/// The caller must pass a valid UTF-8 C string, or null (which is treated as an unknown error).
#[no_mangle]
pub unsafe extern "C" fn ohand_classify_error(error_str: *const std::ffi::c_char) -> i32 {
    if error_str.is_null() {
        return ErrorClass::Permanent as i32;
    }

    let c_str = match std::ffi::CStr::from_ptr(error_str).to_str() {
        Ok(s) => s,
        Err(_) => return ErrorClass::Permanent as i32,
    };

    let class = if c_str.contains("unauthorized")
        || c_str.contains("permission")
        || c_str.contains("credential")
    {
        ErrorClass::Unauthorized
    } else if c_str.contains("cancelled") {
        ErrorClass::Cancelled
    } else if c_str.contains("unsupported") {
        ErrorClass::Unsupported
    } else if c_str.contains("temporary")
        || c_str.contains("timeout")
        || c_str.contains("network")
        || c_str.contains("transient")
    {
        ErrorClass::Transient
    } else {
        ErrorClass::Permanent
    };

    class as i32
}
