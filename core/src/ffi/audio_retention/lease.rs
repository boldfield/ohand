//! The store-wide fence around one native audio removal.
//!
//! A removal approval is only safe if nothing can make the audio protected between the approval
//! and the file effect. Every open core handle has its own connection, worker and mutex, and other
//! processes may write the same store, so serializing one handle's worker is not a fence. The
//! fence here is SQLite's own write lock: a lease opens a dedicated connection, starts
//! `BEGIN IMMEDIATE` (which excludes every other writer on the store, from any connection or
//! process, and waits for a current writer to finish) and re-runs the authorization inside it.
//! The lock is held, with nothing written, until native code has carried out the file effect and
//! releases the lease, or until the lease expires. The file effect itself stays native.
//!
//! The lease lasts [`LEASE_DURATION`] unless native code pins it. Pinning ([`pin`]) succeeds only
//! while the fence is still held and, once it has, nothing but [`release`] can end the fence: the
//! expiry waits for the release instead of rolling back. Native code pins immediately before the
//! file effect and releases after it, so expiry and the effect are mutually exclusive rather than
//! merely timed apart, and a stalled effect only delays other writers (which then fail transiently
//! as `store_busy` after the store's busy timeout) instead of letting them commit under it. A pin
//! that comes too late is refused (`lease_expired`) and the effect must not run. Unpinned, the
//! lease is shorter than the store's busy timeout, so a writer that waits behind it is delayed and
//! never fails because of it. A lease that is never pinned or released, because the caller died,
//! the handle was closed or the process stalled before the effect, frees the store when it
//! expires; one that is pinned is released by native code after the effect whatever happens to it.

use super::{store_failure, AbiFailure};
use crate::lifecycle::retention::{authorize_audio_removal, RemovalDecision, RetentionPolicy};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Clock;
use rusqlite::Connection;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// How long an unpinned removal lease holds the store's write lock at most.
pub(super) const LEASE_DURATION: Duration = Duration::from_secs(3);
/// How long beginning a lease waits for another writer before reporting the store busy.
const BEGIN_BUSY_TIMEOUT: Duration = Duration::from_secs(1);
/// Leases that may be held at once, which bounds the threads and connections they use.
const MAXIMUM_LIVE_LEASES: usize = 8;

const LEASE_EXPIRED: AbiFailure = AbiFailure {
    class: ErrorClass::Transient,
    code: "lease_expired",
    message: Cow::Borrowed("the removal lease is not held; the removal must be decided again"),
};

/// The fence itself: the connection that holds the write lock (until it is freed) and whether the
/// file effect has begun under it.
struct Fence {
    connection: Option<Connection>,
    pinned: bool,
}

struct Shared {
    fence: Mutex<Fence>,
    released: Condvar,
}

struct Lease {
    handle: u64,
    shared: Arc<Shared>,
    cancel: Sender<()>,
}

static LEASES: Mutex<BTreeMap<u64, Lease>> = Mutex::new(BTreeMap::new());
static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);
/// Slots reserved by leases being decided or held, until their expiry thread has finished.
static RESERVED_SLOTS: AtomicUsize = AtomicUsize::new(0);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One of the [`MAXIMUM_LIVE_LEASES`] slots, checked and taken in one step and given back on drop.
struct Slot;

impl Slot {
    fn reserve() -> Option<Slot> {
        RESERVED_SLOTS
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |reserved| {
                (reserved < MAXIMUM_LIVE_LEASES).then_some(reserved + 1)
            })
            .ok()
            .map(|_| Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        RESERVED_SLOTS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Rolls back (nothing was written) and closes the leased connection, if it is still held.
fn free(fence: &mut Fence) -> bool {
    fence.pinned = false;
    let Some(held) = fence.connection.take() else {
        return false;
    };
    let _ = held.execute_batch("ROLLBACK");
    true
}

/// The decision made inside the fence, and the lease that keeps the fence up when it approved.
pub(super) struct Begun {
    pub(super) decision: RemovalDecision,
    pub(super) lease_id: Option<u64>,
}

/// Decides the removal while holding the store's write lock and, when approved, keeps holding it
/// until [`release`], or for at most `duration` unless it is pinned first.
pub(super) fn begin(
    store_path: &Path,
    handle: u64,
    clock: &dyn Clock,
    policy: &RetentionPolicy,
    item_id: &str,
    audio_reference: &str,
    duration: Duration,
) -> Result<Begun, AbiFailure> {
    if store_path.as_os_str().is_empty() {
        return Err(AbiFailure::STORE_UNAVAILABLE);
    }
    let slot = Slot::reserve().ok_or(AbiFailure::STORE_BUSY)?;
    let connection = Connection::open(store_path).map_err(|error| store_failure(&error.into()))?;
    connection
        .busy_timeout(BEGIN_BUSY_TIMEOUT)
        .map_err(|error| store_failure(&error.into()))?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| store_failure(&error.into()))?;
    let decision =
        match authorize_audio_removal(&connection, clock, policy, item_id, audio_reference) {
            Ok(decision) => decision,
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(store_failure(&error));
            }
        };
    if !matches!(decision, RemovalDecision::Approved { .. }) {
        let _ = connection.execute_batch("ROLLBACK");
        return Ok(Begun {
            decision,
            lease_id: None,
        });
    }

    let lease_id = NEXT_LEASE.fetch_add(1, Ordering::SeqCst);
    let shared = Arc::new(Shared {
        fence: Mutex::new(Fence {
            connection: Some(connection),
            pinned: false,
        }),
        released: Condvar::new(),
    });
    let (cancel, cancelled) = channel::<()>();
    let timer_shared = Arc::clone(&shared);
    let spawn_failure_shared = Arc::clone(&shared);
    // Registered before the timer can run, so an expiry always finds the entry it removes.
    let mut leases = lock(&LEASES);
    leases.insert(
        lease_id,
        Lease {
            handle,
            shared,
            cancel,
        },
    );
    let spawned = std::thread::Builder::new()
        .name("ohand-removal-lease".to_string())
        .spawn(move || {
            let _slot = slot;
            let _ = cancelled.recv_timeout(duration);
            {
                // A pinned fence is never rolled back here: the effect may be running under it.
                let mut fence = lock(&timer_shared.fence);
                while fence.pinned {
                    fence = timer_shared
                        .released
                        .wait(fence)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                free(&mut fence);
            }
            lock(&LEASES).remove(&lease_id);
        });
    if spawned.is_err() {
        leases.remove(&lease_id);
        drop(leases);
        free(&mut lock(&spawn_failure_shared.fence));
        return Err(AbiFailure::INTERNAL);
    }
    Ok(Begun {
        decision,
        lease_id: Some(lease_id),
    })
}

fn find(handle: u64, lease_id: u64) -> Result<Arc<Shared>, AbiFailure> {
    match lock(&LEASES).get(&lease_id) {
        None => Err(LEASE_EXPIRED),
        Some(lease) if lease.handle != handle => Err(AbiFailure::INVALID_REQUEST),
        Some(lease) => Ok(Arc::clone(&lease.shared)),
    }
}

/// Keeps the fence up until [`release`], whatever the lease's duration: called immediately before
/// the file effect. Succeeds only if the fence is still held; then the expiry can no longer end it.
/// An expired or unknown lease is `lease_expired` and the effect must not run. Pinning twice is
/// harmless.
pub(super) fn pin(handle: u64, lease_id: u64) -> Result<(), AbiFailure> {
    let shared = find(handle, lease_id)?;
    let mut fence = lock(&shared.fence);
    if fence.connection.is_none() {
        return Err(LEASE_EXPIRED);
    }
    fence.pinned = true;
    Ok(())
}

/// Ends the lease. Succeeds only if the fence was held without a gap until now; an unknown,
/// foreign or expired lease is `lease_expired` (or `invalid_request` for another handle's lease),
/// and an expired one has already freed the store.
pub(super) fn release(handle: u64, lease_id: u64) -> Result<(), AbiFailure> {
    let lease = {
        let mut leases = lock(&LEASES);
        match leases.get(&lease_id) {
            None => return Err(LEASE_EXPIRED),
            Some(lease) if lease.handle != handle => return Err(AbiFailure::INVALID_REQUEST),
            Some(_) => {}
        }
        leases.remove(&lease_id)
    };
    let Some(lease) = lease else {
        return Err(LEASE_EXPIRED);
    };
    let held = free(&mut lock(&lease.shared.fence));
    lease.shared.released.notify_all();
    let _ = lease.cancel.send(());
    if held {
        Ok(())
    } else {
        Err(LEASE_EXPIRED)
    }
}

#[cfg(test)]
pub(super) fn live_leases() -> usize {
    lock(&LEASES).len()
}
