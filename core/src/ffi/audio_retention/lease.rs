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
//! The lease lasts [`LEASE_DURATION`], which is shorter than the store's busy timeout, so a writer
//! that waits behind a lease is delayed and never fails because of it. Releasing after expiry is
//! refused (`lease_expired`), so native code learns that the fence may have lapsed; native code
//! also does not start the effect late in the lease. A lease that is never released, because the
//! caller died, the handle was closed or the process stalled, frees the store when it expires.

use super::{store_failure, AbiFailure};
use crate::lifecycle::retention::{authorize_audio_removal, RemovalDecision, RetentionPolicy};
use crate::providers::contracts::ErrorClass;
use crate::store::schema::Clock;
use rusqlite::Connection;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// How long a removal lease holds the store's write lock at most.
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

struct Lease {
    handle: u64,
    connection: Arc<Mutex<Option<Connection>>>,
    cancel: Sender<()>,
}

static LEASES: Mutex<BTreeMap<u64, Lease>> = Mutex::new(BTreeMap::new());
static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Rolls back (nothing was written) and closes the leased connection, if it is still held.
fn free(connection: &Mutex<Option<Connection>>) -> bool {
    let Some(held) = lock(connection).take() else {
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
/// for `duration` or until [`release`].
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
    if lock(&LEASES).len() >= MAXIMUM_LIVE_LEASES {
        return Err(AbiFailure::STORE_BUSY);
    }
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
    let held = Arc::new(Mutex::new(Some(connection)));
    let (cancel, cancelled) = channel::<()>();
    let timer_connection = Arc::clone(&held);
    let spawn_failure_connection = Arc::clone(&held);
    // Registered before the timer can run, so an expiry always finds the entry it removes.
    let mut leases = lock(&LEASES);
    leases.insert(
        lease_id,
        Lease {
            handle,
            connection: held,
            cancel,
        },
    );
    let spawned = std::thread::Builder::new()
        .name("ohand-removal-lease".to_string())
        .spawn(move || {
            let _ = cancelled.recv_timeout(duration);
            lock(&LEASES).remove(&lease_id);
            free(&timer_connection);
        });
    if spawned.is_err() {
        leases.remove(&lease_id);
        drop(leases);
        free(&spawn_failure_connection);
        return Err(AbiFailure::INTERNAL);
    }
    Ok(Begun {
        decision,
        lease_id: Some(lease_id),
    })
}

/// Ends the lease early. Succeeds only if the fence was held without a gap until now; an unknown,
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
    let held = free(&lease.connection);
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
