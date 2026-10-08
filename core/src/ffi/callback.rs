// Thread-safe callback delivery and coordination.
// Callbacks are delivered on a documented thread and are prevented after cancellation.

use std::sync::{Arc, Mutex};

/// A callback context that coordinates delivery and prevents post-cancellation callbacks.
pub struct CallbackContext {
    inner: Arc<CallbackState>,
}

struct CallbackState {
    /// Whether callbacks are still permitted (not cancelled/torn down).
    active: Mutex<bool>,
}

impl CallbackContext {
    pub fn new() -> Self {
        CallbackContext {
            inner: Arc::new(CallbackState {
                active: Mutex::new(true),
            }),
        }
    }

    /// Check if callbacks are still permitted.
    pub fn is_active(&self) -> bool {
        *self.inner.active.lock().unwrap()
    }

    /// Deactivate callbacks (after cancellation or teardown).
    /// Returns false if already inactive, true if just deactivated.
    pub fn deactivate(&self) -> bool {
        let mut active = self.inner.active.lock().unwrap();
        if *active {
            *active = false;
            true
        } else {
            false
        }
    }

    /// Get a cloneable handle for delivery.
    pub fn handle(&self) -> CallbackHandle {
        CallbackHandle {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for CallbackContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CallbackContext {
    fn clone(&self) -> Self {
        CallbackContext {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// A handle to the callback context for use in background operations.
pub struct CallbackHandle {
    inner: Arc<CallbackState>,
}

impl CallbackHandle {
    /// Check if callbacks are still permitted (safe to deliver).
    pub fn may_deliver(&self) -> bool {
        *self.inner.active.lock().unwrap()
    }
}

impl Clone for CallbackHandle {
    fn clone(&self) -> Self {
        CallbackHandle {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_callback_context_active_by_default() {
        let ctx = CallbackContext::new();
        assert!(ctx.is_active());
    }

    #[test]
    fn test_callback_context_deactivate() {
        let ctx = CallbackContext::new();
        assert!(ctx.is_active());

        let deactivated = ctx.deactivate();
        assert!(deactivated);
        assert!(!ctx.is_active());

        let deactivated_again = ctx.deactivate();
        assert!(!deactivated_again);
        assert!(!ctx.is_active());
    }

    #[test]
    fn test_callback_handle_reflects_context_state() {
        let ctx = CallbackContext::new();
        let handle = ctx.handle();

        assert!(handle.may_deliver());

        ctx.deactivate();
        assert!(!handle.may_deliver());
    }

    #[test]
    fn test_callback_handle_clone() {
        let ctx = CallbackContext::new();
        let handle1 = ctx.handle();
        let handle2 = handle1.clone();

        assert!(handle1.may_deliver());
        assert!(handle2.may_deliver());

        ctx.deactivate();
        assert!(!handle1.may_deliver());
        assert!(!handle2.may_deliver());
    }

    #[test]
    fn test_callback_coordination_threadsafe() {
        let ctx = Arc::new(CallbackContext::new());

        let ctx1 = Arc::clone(&ctx);
        let t1 = thread::spawn(move || {
            let handle = ctx1.handle();
            for _ in 0..100 {
                let _ = handle.may_deliver();
            }
        });

        let ctx2 = Arc::clone(&ctx);
        let t2 = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(10));
            ctx2.deactivate();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        assert!(!ctx.is_active());
    }

    #[test]
    fn test_callback_prevents_post_cancellation_delivery() {
        let ctx = CallbackContext::new();
        let handle = ctx.handle();

        assert!(handle.may_deliver());

        ctx.deactivate();

        assert!(!handle.may_deliver());
    }
}
