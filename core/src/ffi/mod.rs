//! FFI bindings and native bridge for Swift interop.
//!
//! Provides C-compatible interfaces for capture creation, error handling,
//! and result management. All returned pointers must be freed with the
//! appropriate destructor to prevent memory leaks.

use anyhow::Error;
use std::os::raw::c_char;
use std::ptr;

pub mod capture;
pub mod error;

pub use capture::*;
pub use error::*;

/// Opaque handle to a Rust error for FFI boundary.
/// Created by FFI functions that can fail and must be freed with
/// `ohand_error_free`.
pub struct OhAndError {
    message: String,
}

impl OhAndError {
    fn new(err: Error) -> Self {
        OhAndError {
            message: err.to_string(),
        }
    }
}

/// Free an error returned from FFI.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ohand_error_free(err: *mut OhAndError) {
    if !err.is_null() {
        unsafe {
            let _ = Box::from_raw(err);
        }
    }
}

/// Get the error message from an OhAndError.
/// The returned pointer is valid only for the lifetime of the error.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn ohand_error_message(err: *const OhAndError) -> *const c_char {
    if err.is_null() {
        return ptr::null();
    }
    unsafe { (*err).message.as_ptr() as *const c_char }
}
