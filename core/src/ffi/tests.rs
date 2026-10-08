// Integration tests for FFI handles and callbacks.
// These tests exercise the real core implementation in simulator contexts.

#[cfg(test)]
mod handle_lifecycle_tests {
    use crate::ffi::handle::HandleState;

    #[test]
    fn test_handle_initialization_and_destruction() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        unsafe {
            assert!(!(*ptr).is_cancelled());
            let _ = Box::from_raw(ptr);
        }
    }

    #[test]
    fn test_handle_cancellation() {
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
    fn test_repeated_cancellation() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        unsafe {
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());
            let _ = Box::from_raw(ptr);
        }
    }
}

#[cfg(test)]
mod callback_delivery_tests {
    use crate::ffi::callback::CallbackContext;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn test_callback_active_by_default() {
        let ctx = CallbackContext::new();
        let handle = ctx.handle();

        assert!(ctx.is_active());
        assert!(handle.may_deliver());
    }

    #[test]
    fn test_callback_delivery_after_deactivation() {
        let ctx = CallbackContext::new();
        let handle = ctx.handle();

        assert!(handle.may_deliver());

        ctx.deactivate();

        assert!(!handle.may_deliver());
    }

    #[test]
    fn test_callback_prevents_post_cancellation() {
        let ctx = CallbackContext::new();
        let handle = ctx.handle();

        let handle2 = handle.clone();
        assert!(handle2.may_deliver());

        ctx.deactivate();

        assert!(!handle.may_deliver());
        assert!(!handle2.may_deliver());
    }

    #[test]
    fn test_callback_threaded_delivery() {
        let ctx = Arc::new(CallbackContext::new());
        let barrier = Arc::new(Barrier::new(3));

        let ctx1 = Arc::clone(&ctx);
        let barrier1 = Arc::clone(&barrier);
        let t1 = thread::spawn(move || {
            barrier1.wait();
            for _ in 0..50 {
                let _ = ctx1.handle().may_deliver();
            }
        });

        let ctx2 = Arc::clone(&ctx);
        let barrier2 = Arc::clone(&barrier);
        let t2 = thread::spawn(move || {
            barrier2.wait();
            for _ in 0..50 {
                let _ = ctx2.handle().may_deliver();
            }
        });

        barrier.wait();
        ctx.deactivate();

        t1.join().unwrap();
        t2.join().unwrap();

        assert!(!ctx.is_active());
    }

    #[test]
    fn test_callback_idempotent_deactivation() {
        let ctx = CallbackContext::new();

        let deactivated_first = ctx.deactivate();
        assert!(deactivated_first);

        let deactivated_second = ctx.deactivate();
        assert!(!deactivated_second);

        let deactivated_third = ctx.deactivate();
        assert!(!deactivated_third);
    }
}

#[cfg(test)]
mod error_normalization_tests {
    use crate::ffi::error::{classify_error, redacted_message, ErrorClass};

    #[test]
    fn test_unauthorized_classification() {
        let error = anyhow::anyhow!("unauthorized: permission denied");
        assert_eq!(classify_error(&error), ErrorClass::Unauthorized);

        let error2 = anyhow::anyhow!("credential not found");
        assert_eq!(classify_error(&error2), ErrorClass::Unauthorized);
    }

    #[test]
    fn test_cancelled_classification() {
        let error = anyhow::anyhow!("operation cancelled by user");
        assert_eq!(classify_error(&error), ErrorClass::Cancelled);
    }

    #[test]
    fn test_transient_classification() {
        let error = anyhow::anyhow!("network timeout");
        assert_eq!(classify_error(&error), ErrorClass::Transient);

        let error2 = anyhow::anyhow!("temporary failure, retry later");
        assert_eq!(classify_error(&error2), ErrorClass::Transient);
    }

    #[test]
    fn test_unsupported_classification() {
        let error = anyhow::anyhow!("feature unsupported on this device");
        assert_eq!(classify_error(&error), ErrorClass::Unsupported);
    }

    #[test]
    fn test_permanent_classification() {
        let error = anyhow::anyhow!("database corrupted");
        assert_eq!(classify_error(&error), ErrorClass::Permanent);

        let error2 = anyhow::anyhow!("unknown error");
        assert_eq!(classify_error(&error2), ErrorClass::Permanent);
    }

    #[test]
    fn test_error_redaction_no_content_leakage() {
        let error = anyhow::anyhow!("Failed to process capture data");
        let msg = redacted_message(&error);

        assert!(!msg.contains("process"));
        assert!(!msg.contains("capture"));
        assert!(msg.contains("failed") || msg.contains("error"));
    }

    #[test]
    fn test_error_redaction_no_user_content() {
        let error = anyhow::anyhow!("User note contains: 'My password is abc123'");
        let msg = redacted_message(&error);

        assert!(!msg.contains("password"));
        assert!(!msg.contains("abc123"));
        assert!(!msg.contains("note"));
    }
}

#[cfg(test)]
mod handle_single_ownership_tests {
    use crate::ffi::handle::HandleState;

    #[test]
    fn test_single_owner_exclusive_access() {
        let state = Box::new(HandleState::new());
        let ptr = Box::into_raw(state);

        unsafe {
            (*ptr).cancel();
            assert!((*ptr).is_cancelled());

            let _ = Box::from_raw(ptr);
        }
    }

    #[test]
    fn test_null_handle_safe_operations() {
        let null_ptr: *mut HandleState = std::ptr::null_mut();
        assert!(null_ptr.is_null());
    }
}

#[cfg(test)]
mod exported_ffi_tests {
    use crate::ffi;

    #[test]
    fn test_exported_create_and_destroy_handle() {
        unsafe {
            let handle = ffi::ohand_create_handle();
            assert!(!handle.is_null());

            ffi::ohand_destroy_handle(handle);
        }
    }

    #[test]
    fn test_exported_destroy_null_handle() {
        unsafe {
            ffi::ohand_destroy_handle(std::ptr::null_mut());
        }
    }

    #[test]
    fn test_exported_cancel_handle() {
        unsafe {
            let handle = ffi::ohand_create_handle();
            let result = ffi::ohand_cancel_handle(handle);
            assert_eq!(result, 0);

            let is_cancelled = ffi::ohand_is_handle_cancelled(handle);
            assert_eq!(is_cancelled, 1);

            ffi::ohand_destroy_handle(handle);
        }
    }

    #[test]
    fn test_exported_invalid_handle_returns_error() {
        unsafe {
            let result = ffi::ohand_is_handle_cancelled(std::ptr::null_mut());
            assert_eq!(result, -1);

            let result = ffi::ohand_cancel_handle(std::ptr::null_mut());
            assert_eq!(result, -1);
        }
    }

    #[test]
    fn test_exported_callback_registration() {
        unsafe {
            let handle = ffi::ohand_create_handle();

            extern "C" fn test_callback(_: *const std::ffi::c_void) {}

            let result = ffi::ohand_register_callback(handle, test_callback);
            assert_eq!(result, 0);

            ffi::ohand_destroy_handle(handle);
        }
    }

    #[test]
    fn test_exported_error_classification() {
        unsafe {
            let unauthorized = "unauthorized access\0";
            let result =
                ffi::ohand_classify_error(unauthorized.as_ptr() as *const std::ffi::c_char);
            assert_eq!(result, 3); // Unauthorized

            let cancelled = "operation cancelled\0";
            let result = ffi::ohand_classify_error(cancelled.as_ptr() as *const std::ffi::c_char);
            assert_eq!(result, 4); // Cancelled

            let transient = "network timeout\0";
            let result = ffi::ohand_classify_error(transient.as_ptr() as *const std::ffi::c_char);
            assert_eq!(result, 1); // Transient

            let permanent = "database error\0";
            let result = ffi::ohand_classify_error(permanent.as_ptr() as *const std::ffi::c_char);
            assert_eq!(result, 2); // Permanent
        }
    }

    #[test]
    fn test_exported_invoke_callback_not_invoked_when_cancelled() {
        unsafe {
            let handle = ffi::ohand_create_handle();

            extern "C" fn test_callback(_: *const std::ffi::c_void) {}

            let _ = ffi::ohand_register_callback(handle, test_callback);
            let _ = ffi::ohand_cancel_handle(handle);

            let result = ffi::ohand_invoke_callback(handle, std::ptr::null());
            assert_eq!(result, 1); // Cancelled

            ffi::ohand_destroy_handle(handle);
        }
    }
}
