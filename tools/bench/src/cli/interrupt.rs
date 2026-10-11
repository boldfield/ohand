//! Maps Ctrl-C to the run's cancel token, so an interrupt ends the in-flight request promptly
//! and the executor records what was and was not sent instead of the process dying mid-write.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use ohand_core::providers::contracts::CancelToken;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Installs the SIGINT handler and a watcher that cancels `token` once it fires. Called once by
/// the binary; library callers and tests pass their own token and never install a handler.
pub fn cancel_on_interrupt(token: CancelToken) {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_interrupt as *const () as libc::sighandler_t,
        );
    }
    thread::spawn(move || {
        while !INTERRUPTED.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(25));
        }
        token.cancel();
    });
}
