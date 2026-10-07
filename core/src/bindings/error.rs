//! Error handling for FFI boundary.

use super::OhAndError;
use anyhow::Error;
use std::ptr;

/// Result type for FFI functions that can fail.
/// On success, returns a non-null pointer to the result (ownership transferred to caller).
/// On failure, returns null and sets `out_error` to point to an allocated OhAndError
/// that must be freed with `ohand_error_free`.
pub type FFIResult<T> = *mut T;

/// Create an OhAndError from a Rust error.
pub fn create_ffi_error(err: Error) -> *mut OhAndError {
    Box::into_raw(Box::new(OhAndError::new(err)))
}

/// Convert a Rust Result into FFI return values.
pub fn result_to_ffi<T>(result: Result<T, Error>) -> (FFIResult<T>, *mut OhAndError) {
    match result {
        Ok(value) => (Box::into_raw(Box::new(value)), ptr::null_mut()),
        Err(err) => (ptr::null_mut(), create_ffi_error(err)),
    }
}
