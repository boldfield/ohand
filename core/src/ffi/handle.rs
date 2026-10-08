// Safe handle lifetime management for native-to-core operations.
// Handles are single-owner opaque pointers; no copies allowed after creation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Opaque handle to a core operation (single owner).
/// This is the C ABI type: an owned raw pointer with no Copy.
/// Returned by ohand_create_handle and destroyed by ohand_destroy_handle.
/// The ownership model ensures no dangling pointers after destruction.
pub type OpaqueHandle = *mut HandleState;

/// Callback type: a function pointer that can be invoked from any thread.
pub type CallbackFn = extern "C" fn(*const std::ffi::c_void);

/// Shared state for a core operation (owned by exactly one OpaqueHandle).
pub struct HandleState {
    /// True if the operation has been cancelled.
    cancelled: AtomicBool,
    /// Optional registered callback (stored for later invocation).
    /// Protected by mutex for thread-safe access.
    callback: Mutex<Option<CallbackFn>>,
}

impl HandleState {
    pub fn new() -> Self {
        HandleState {
            cancelled: AtomicBool::new(false),
            callback: Mutex::new(None),
        }
    }

    /// Mark this handle as cancelled.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check if this handle is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Register a callback for this handle.
    /// The callback is stored and can be invoked later if the handle is not cancelled.
    /// Overwrites any previously registered callback.
    pub fn register_callback(&self, callback: CallbackFn) -> Result<(), &'static str> {
        let mut cb = self
            .callback
            .lock()
            .map_err(|_| "callback mutex poisoned")?;
        *cb = Some(callback);
        Ok(())
    }

    /// Invoke the registered callback if one exists and the handle is not cancelled.
    /// Returns true if callback was invoked, false if cancelled or no callback registered.
    pub fn invoke_callback_if_active(
        &self,
        closure: *const std::ffi::c_void,
    ) -> Result<bool, &'static str> {
        if self.is_cancelled() {
            return Ok(false);
        }

        let cb = self
            .callback
            .lock()
            .map_err(|_| "callback mutex poisoned")?;
        if let Some(callback) = *cb {
            callback(closure);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl Default for HandleState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_creation_and_cancellation() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        unsafe {
            assert!(!(*ptr).is_cancelled());
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());
            let _ = Box::from_raw(ptr);
        }
    }

    #[test]
    fn test_handle_cancellation_idempotent() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        unsafe {
            assert!(!(*ptr).is_cancelled());
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());
            let _ = Box::from_raw(ptr);
        }
    }

    #[test]
    fn test_callback_registration() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        extern "C" fn test_callback(_closure: *const std::ffi::c_void) {}

        unsafe {
            let result = (*ptr).register_callback(test_callback);
            assert!(result.is_ok());
            let _ = Box::from_raw(ptr);
        }
    }

    #[test]
    fn test_callback_not_invoked_when_cancelled() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        extern "C" fn test_callback(_closure: *const std::ffi::c_void) {}

        unsafe {
            (*ptr).register_callback(test_callback).unwrap();
            (*ptr).cancel();
            let result = (*ptr).invoke_callback_if_active(std::ptr::null());
            assert_eq!(result, Ok(false));
            let _ = Box::from_raw(ptr);
        }
    }
}
