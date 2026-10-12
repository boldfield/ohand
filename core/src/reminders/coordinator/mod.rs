// Capture-to-installed-notification orchestration (N05).
//
// The pieces already exist: the interpretation dispatcher and apply boundary write the desired
// reminder (`reminders::state`), user commands edit or cancel it, and `reconcile_with_capacity`
// makes the OS agree with the database. The coordinator is the one place that composes them so
// no caller has to remember to settle the OS after a write.
//
// Rules:
// - The durable write comes first and is never rolled back by the OS. Every edit commits in its
//   own transaction; only then is the OS settled. An edit whose settlement failed (OS listing
//   unavailable, for example) is still a committed edit: `ReminderEdit::settled` carries the
//   error, the returned record says `pending_schedule`, and the next settlement (after another
//   write, on launch or on foreground activation) finishes it.
// - Status is read back from the database after settlement. `schedule_state` becomes
//   `scheduled` only inside `reconcile` after the OS accepted the request (or was found already
//   holding it with the right due instant); the coordinator never sets it, so a transient OS
//   failure, a withheld rejection or an ambiguous answer is reported as it is.
// - Nothing here decides a time. Offline commands resolve through the fast path and approved
//   asynchronous results through the same apply boundary, both against the capture's stored
//   time context; a result that arrives after its requested instant is stored as
//   `unschedulable(time_in_past)` and never installed, backdated or replaced by a new deadline.
// - Settlement is idempotent and driven by comparing desired state with the OS pending set, so
//   a retried command, a repeated drain, a lost acknowledgment or an app restart converges on
//   one pending request per desired reminder and makes no OS call when nothing changed.

use rusqlite::Transaction;
use thiserror::Error;

use crate::jobs::runner::{DrainReport, JobRunner};
use crate::providers::contracts::CancelToken;
use crate::reminders::capacity::{
    reconcile_with_capacity, CapacityPolicy, CapacityReport, PromptDemand,
};
use crate::reminders::reconcile::{NotificationEffects, ReconcileError};
use crate::reminders::state::{
    apply_user_time_correction, cancel_reminder, end_item_with_event, get_reminder,
    ReminderCancellation, ReminderRecord, ReminderStateError, UserTimeCorrection,
};
use crate::store::events::Event;
use crate::store::schema::{Clock, Database};

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    State(#[from] ReminderStateError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error("storage error: {0}")]
    Storage(String),
}

fn storage_error(error: impl std::fmt::Display) -> CoordinatorError {
    CoordinatorError::Storage(error.to_string())
}

/// The result of a committed user edit.
#[derive(Debug)]
pub struct ReminderEdit {
    /// The item's reminder as stored after settlement, so `schedule_state` is what the OS
    /// actually confirmed. `None` when the item has no reminder.
    pub reminder: Option<ReminderRecord>,
    /// The settlement pass, or why it could not run. An error here does not undo the edit.
    pub settled: Result<CapacityReport, ReconcileError>,
}

/// The result of draining ready jobs and then settling the OS.
#[derive(Debug)]
pub struct DrainAndSettle {
    pub drain: DrainReport,
    pub settled: Result<CapacityReport, ReconcileError>,
}

pub struct ReminderCoordinator<'a> {
    clock: &'a dyn Clock,
    policy: CapacityPolicy,
}

impl<'a> ReminderCoordinator<'a> {
    /// `clock` stamps the durable reminder writes and the settlement pass.
    pub fn new(clock: &'a dyn Clock) -> ReminderCoordinator<'a> {
        ReminderCoordinator {
            clock,
            policy: CapacityPolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: CapacityPolicy) -> ReminderCoordinator<'a> {
        self.policy = policy;
        self
    }

    /// Bring the OS into agreement with the durable desired state. Safe to run after any write,
    /// on launch and on foreground activation.
    pub fn settle(
        &self,
        database: &mut Database,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
    ) -> Result<CapacityReport, ReconcileError> {
        reconcile_with_capacity(database, self.clock, effects, &self.policy, prompts)
    }

    /// Drain ready jobs through `runner` (offline fast path or approved provider, applied
    /// through the interpretation boundary), then settle the OS so every reminder those jobs
    /// made desired is installed. Settlement runs even when the drain stopped early or a job
    /// failed: an earlier pass may have left desired reminders uninstalled.
    pub fn drain_and_settle(
        &self,
        database: &mut Database,
        runner: &JobRunner<'_>,
        cancel: &CancelToken,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
    ) -> DrainAndSettle {
        let drain = runner.drain(database, cancel);
        let settled = self.settle(database, effects, prompts);
        DrainAndSettle { drain, settled }
    }

    /// Set or change the reminder time from an explicit user choice, then settle. Retries with
    /// the same command ID return the original result and install nothing a second time.
    pub fn correct_time(
        &self,
        database: &mut Database,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
        correction: &UserTimeCorrection,
    ) -> Result<ReminderEdit, CoordinatorError> {
        let item_id = correction.item_id.clone();
        write(database, |tx| {
            apply_user_time_correction(tx, self.clock, correction)
        })?;
        self.finish_edit(database, effects, prompts, &item_id)
    }

    /// Cancel the item's reminder while the item stays active, then settle.
    pub fn cancel(
        &self,
        database: &mut Database,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
        cancellation: &ReminderCancellation,
    ) -> Result<ReminderEdit, CoordinatorError> {
        let item_id = cancellation.item_id.clone();
        write(database, |tx| cancel_reminder(tx, self.clock, cancellation))?;
        self.finish_edit(database, effects, prompts, &item_id)
    }

    /// Record a completion or cancellation of the item, which cancels its reminder in the same
    /// transaction, then settle so the pending request is removed.
    pub fn end_item(
        &self,
        database: &mut Database,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
        event: &Event,
        expected_revision: i32,
    ) -> Result<ReminderEdit, CoordinatorError> {
        let item_id = event.item_id.clone();
        write(database, |tx| {
            end_item_with_event(tx, self.clock, event, expected_revision)
        })?;
        self.finish_edit(database, effects, prompts, &item_id)
    }

    /// The stored reminder of `item_id`, without touching the OS.
    pub fn current_reminder(
        &self,
        database: &mut Database,
        item_id: &str,
    ) -> Result<Option<ReminderRecord>, CoordinatorError> {
        read_reminder(database, item_id)
    }

    fn finish_edit(
        &self,
        database: &mut Database,
        effects: &mut dyn NotificationEffects,
        prompts: &[PromptDemand],
        item_id: &str,
    ) -> Result<ReminderEdit, CoordinatorError> {
        let settled = self.settle(database, effects, prompts);
        let reminder = read_reminder(database, item_id)?;
        Ok(ReminderEdit { reminder, settled })
    }
}

fn write<T>(
    database: &mut Database,
    command: impl FnOnce(&Transaction<'_>) -> Result<T, ReminderStateError>,
) -> Result<T, CoordinatorError> {
    let transaction = database.immediate_transaction().map_err(storage_error)?;
    let value = command(&transaction)?;
    transaction.commit().map_err(storage_error)?;
    Ok(value)
}

fn read_reminder(
    database: &mut Database,
    item_id: &str,
) -> Result<Option<ReminderRecord>, CoordinatorError> {
    let transaction = database.transaction().map_err(storage_error)?;
    Ok(get_reminder(&transaction, item_id)?)
}
