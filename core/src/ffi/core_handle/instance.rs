//! Lifetime, cancellation and event delivery for one open core.
//!
//! A handle is an unforgeable-by-accident integer, never a pointer: it names an entry in a
//! process-wide registry and is never reused, so a stale, foreign or double-closed handle is
//! rejected with `invalid_handle` instead of being dereferenced.
//!
//! Each open core owns one worker thread. Background work runs there and its result is
//! delivered to the registered event callback on that same thread, one event at a time and in
//! submission order. Delivery happens while holding the delivery lock, and cancellation,
//! callback replacement and close take that lock too, so once `cancel`, `set_callback` or
//! `close` returns, no callback invocation is running and none will start.
//!
//! A callback holds its core's delivery lock, so a lifecycle call that waits for another
//! core's delivery lock from inside a callback could deadlock (two callbacks cancelling each
//! other). From inside any callback, only `cancel` of that same core is allowed; every other
//! lifecycle call is rejected as `reentrant_call`. A callback must also not block on another
//! thread that cancels, replaces the callback of or closes its own core.

use super::failure::{AbiFailure, OHAND_CORE_STATUS_OK};
use crate::store::schema::{Database, SystemClock};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// Operations that may wait for the worker before `start_*` reports `busy`.
pub const QUEUE_CAPACITY: usize = 1024;

pub type EventCallbackFn = unsafe extern "C" fn(
    context: *mut c_void,
    operation_id: u64,
    status: u32,
    data: *const u8,
    len: usize,
);

pub type JobFn = Box<dyn FnOnce(&Mutex<Database>) -> Result<Vec<u8>, AbiFailure> + Send>;

struct Job {
    operation_id: u64,
    run: JobFn,
}

#[derive(Clone, Copy)]
struct Registration {
    callback: EventCallbackFn,
    context: usize,
}

thread_local! {
    static DELIVERING_FOR: Cell<Option<u64>> = const { Cell::new(None) };
}

struct DeliveryScope;

impl DeliveryScope {
    fn enter(handle: u64) -> DeliveryScope {
        DELIVERING_FOR.with(|slot| slot.set(Some(handle)));
        DeliveryScope
    }
}

impl Drop for DeliveryScope {
    fn drop(&mut self) {
        DELIVERING_FOR.with(|slot| slot.set(None));
    }
}

/// True while this thread is inside any open core's event callback.
fn is_inside_any_callback() -> bool {
    DELIVERING_FOR.with(|slot| slot.get()).is_some()
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared {
    handle: u64,
    database: Mutex<Database>,
    cancelled: AtomicBool,
    delivery: Mutex<Option<Registration>>,
}

impl Shared {
    fn is_delivering_on_this_thread(&self) -> bool {
        DELIVERING_FOR.with(|slot| slot.get()) == Some(self.handle)
    }

    fn deliver(&self, operation_id: u64, outcome: Result<Vec<u8>, AbiFailure>) {
        let registration_guard = lock(&self.delivery);
        if self.cancelled.load(Ordering::SeqCst) {
            return;
        }
        let Some(registration) = *registration_guard else {
            return;
        };
        let (status, payload) = match outcome {
            Ok(payload) => (OHAND_CORE_STATUS_OK, payload),
            Err(failure) => (failure.status(), failure.to_json()),
        };
        let _scope = DeliveryScope::enter(self.handle);
        // SAFETY: the registrant guaranteed callback and context stay valid until the
        // registration is replaced or the core is closed, both of which wait for this lock.
        unsafe {
            (registration.callback)(
                registration.context as *mut c_void,
                operation_id,
                status,
                payload.as_ptr(),
                payload.len(),
            );
        }
    }

    fn run_worker(&self, queue: Receiver<Job>) {
        for job in queue {
            if self.cancelled.load(Ordering::SeqCst) {
                continue;
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| (job.run)(&self.database)))
                .unwrap_or(Err(AbiFailure::INTERNAL));
            self.deliver(job.operation_id, outcome);
        }
    }

    fn cancel(&self) -> Result<(), AbiFailure> {
        if self.is_delivering_on_this_thread() {
            self.cancelled.store(true, Ordering::SeqCst);
        } else if is_inside_any_callback() {
            return Err(AbiFailure::REENTRANT_CALL);
        } else {
            self.cancelled.store(true, Ordering::SeqCst);
            drop(lock(&self.delivery));
        }
        Ok(())
    }
}

pub struct CoreInstance {
    shared: Arc<Shared>,
    queue: Mutex<Option<SyncSender<Job>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

static REGISTRY: Mutex<BTreeMap<u64, Arc<CoreInstance>>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub fn live_handles() -> usize {
    lock(&REGISTRY).len()
}

pub fn lookup(handle: u64) -> Result<Arc<CoreInstance>, AbiFailure> {
    lock(&REGISTRY)
        .get(&handle)
        .cloned()
        .ok_or(AbiFailure::INVALID_HANDLE)
}

pub fn open(path: &str) -> Result<u64, AbiFailure> {
    let database = Database::open(path, Arc::new(SystemClock))
        .map_err(|error| AbiFailure::from_store(&error, AbiFailure::STORE_UNAVAILABLE))?;
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
    let shared = Arc::new(Shared {
        handle,
        database: Mutex::new(database),
        cancelled: AtomicBool::new(false),
        delivery: Mutex::new(None),
    });
    let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
    let worker_state = Arc::clone(&shared);
    let worker = std::thread::Builder::new()
        .name("ohand-core-worker".to_string())
        .spawn(move || worker_state.run_worker(receiver))
        .map_err(|_| AbiFailure::INTERNAL)?;
    let instance = Arc::new(CoreInstance {
        shared,
        queue: Mutex::new(Some(sender)),
        worker: Mutex::new(Some(worker)),
    });
    lock(&REGISTRY).insert(handle, instance);
    Ok(handle)
}

/// Closes `handle`: no new work is accepted, delivery stops, and the worker has exited when
/// this returns. A second close of the same handle is `invalid_handle`.
pub fn close(handle: u64) -> Result<(), AbiFailure> {
    lookup(handle)?;
    if is_inside_any_callback() {
        return Err(AbiFailure::REENTRANT_CALL);
    }
    let removed = lock(&REGISTRY)
        .remove(&handle)
        .ok_or(AbiFailure::INVALID_HANDLE)?;
    // Cannot be rejected: callers inside a callback were refused above.
    let _ = removed.shared.cancel();
    drop(lock(&removed.queue).take());
    if let Some(worker) = lock(&removed.worker).take() {
        let _ = worker.join();
    }
    *lock(&removed.shared.delivery) = None;
    Ok(())
}

impl CoreInstance {
    /// Stops delivery. When this returns, no callback is running and none will start. It
    /// may be called from inside that same core's callback, where the running callback is the
    /// last one. From inside another core's callback it is rejected as `reentrant_call`,
    /// because waiting for that core's delivery lock while holding our own can deadlock.
    pub fn cancel(&self) -> Result<(), AbiFailure> {
        self.shared.cancel()
    }

    pub fn set_callback(
        &self,
        callback: Option<EventCallbackFn>,
        context: *mut c_void,
    ) -> Result<(), AbiFailure> {
        if is_inside_any_callback() {
            return Err(AbiFailure::REENTRANT_CALL);
        }
        let mut registration = lock(&self.shared.delivery);
        match callback {
            Some(callback) => {
                if self.shared.cancelled.load(Ordering::SeqCst) {
                    return Err(AbiFailure::CANCELLED);
                }
                *registration = Some(Registration {
                    callback,
                    context: context as usize,
                });
            }
            None => *registration = None,
        }
        Ok(())
    }

    pub fn submit(&self, operation_id: u64, run: JobFn) -> Result<(), AbiFailure> {
        if self.shared.cancelled.load(Ordering::SeqCst) {
            return Err(AbiFailure::CANCELLED);
        }
        let queue = lock(&self.queue);
        let Some(sender) = queue.as_ref() else {
            return Err(AbiFailure::INVALID_HANDLE);
        };
        sender
            .try_send(Job { operation_id, run })
            .map_err(|error| match error {
                TrySendError::Full(_) => AbiFailure::QUEUE_FULL,
                TrySendError::Disconnected(_) => AbiFailure::INTERNAL,
            })
    }
}
