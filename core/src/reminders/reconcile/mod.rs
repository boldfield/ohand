// Native notification reconciliation (N03).
//
// Core's durable desired state (N01: `reminders` and `reminder_operations`) is the authority;
// the OS pending set is a cache of it that can be wrong after a crash, a retry, a late
// callback or a relaunch. `reconcile` makes the OS agree with the database and records what it
// confirmed; it never invents reminder state from OS evidence.
//
// Idempotency and crash safety:
// - Every native identifier is `reminder_id#generation` and every operation's effect identity
//   names the operation type and that identifier. Generations never repeat, so an old
//   completion can always be told from a current one and a retry replaces rather than
//   duplicates.
// - Reconciliation is driven by comparing the desired set with the OS pending list, not by
//   replaying operation rows. A schedule whose OS call succeeded but whose acknowledgment was
//   lost (crash, cancelled caller) is found already installed with the right due instant,
//   acknowledged without a second OS call, and never duplicated.
// - Any pending request of a known reminder that is not in the desired set (changed time,
//   cancelled, completed or deleted item, rolled-back install) is cancelled. Requests whose
//   reminder identifier core does not know are left alone: they belong to other features.
//
// Late and duplicate callbacks (`apply_completion`):
// - A completion names the operation by its effect identity and is applied only to an
//   operation that is still current. A success that arrives for a superseded operation (the
//   reminder was cancelled, completed, deleted or moved to a newer generation first) changes
//   no reminder state and reports `Stale { cancel_required: true }` because the OS may now
//   hold a request nothing wants; the caller cancels it, and a later reconcile removes it
//   even if that cancel is lost, since the list shows a request outside the desired set.
// - Acknowledged operations are terminal: duplicates and out-of-order failures report
//   `AlreadyRecorded` and change nothing.
//
// Native-side facts (`schedule_state`, operation states) do not bump the reminder's
// `state_version`, which guards user intent. The one exception is a never-installed reminder
// whose due time has passed: it becomes `Unschedulable(TimeInPast)` with its instant kept and
// no time adjustment. Delivery and acknowledgment facts are never touched here. Callers must
// serialize reconciliations (the `&mut Database` borrow does so within a process); native
// calls are made outside any open transaction.

mod completion;
mod effects;
mod pass;

use thiserror::Error;

use crate::reminders::state::ReminderStateError;

pub use completion::{apply_completion, CompletionOutcome, EffectCompletion, EffectOutcome};
pub use effects::{NotificationEffectError, NotificationEffects, PendingRequest, ScheduleRequest};
pub use pass::{reconcile, EffectFailure, ReconcileReport};

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error(transparent)]
    State(#[from] ReminderStateError),
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Database(String),
    #[error("reminder {0} not found")]
    UnknownReminder(String),
    #[error("reminder {reminder_id} has no operation with effect identity {effect_identity}")]
    UnknownEffect {
        reminder_id: String,
        effect_identity: String,
    },
    #[error("listing pending notifications failed: {0}")]
    ListPending(NotificationEffectError),
}
