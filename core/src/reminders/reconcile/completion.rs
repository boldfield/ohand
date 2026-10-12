use rusqlite::{OptionalExtension, Transaction};

use super::effects::is_definitive_rejection;
use super::ReconcileError;
use crate::providers::contracts::ErrorClass;
use crate::reminders::state::{
    cancel_for_inactive_item, get_reminder, list_operations, OperationState, OperationType,
    ReminderOperation, ReminderRecord, RequestState, ScheduleState,
};
use crate::store::schema::Clock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectOutcome {
    Succeeded,
    Failed(ErrorClass),
}

/// The result of one native effect, named by the operation's effect identity (operation type
/// plus `reminder_id#generation`) so it can only ever apply to the operation it was issued for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectCompletion {
    pub reminder_id: String,
    pub effect_identity: String,
    pub outcome: EffectOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionOutcome {
    Applied,
    /// Duplicate, or out of order after the operation reached its terminal state.
    AlreadyRecorded,
    /// The operation is no longer current. No reminder state changed. When
    /// `cancel_required`, the OS may hold a request core no longer wants and the caller must
    /// cancel `notification_id`.
    Stale {
        cancel_required: bool,
    },
    /// A cancelled effect proves nothing either way.
    NoChange,
}

/// Apply one native completion in the caller's transaction. Idempotent for duplicates and safe
/// for completions that arrive after newer state.
pub fn apply_completion(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    completion: &EffectCompletion,
) -> Result<CompletionOutcome, ReconcileError> {
    let item_id: String = tx
        .query_row(
            "SELECT item_id FROM reminders WHERE reminder_id = ?",
            [&completion.reminder_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| ReconcileError::UnknownReminder(completion.reminder_id.clone()))?;

    // An item that ended through another path must retire its reminder before any completion
    // is judged, so a late install can never look current for an inactive item.
    let lifecycle_state: String = tx.query_row(
        "SELECT lifecycle_state FROM items WHERE item_id = ?",
        [&item_id],
        |row| row.get(0),
    )?;
    if lifecycle_state != "active" {
        cancel_for_inactive_item(tx, clock, &item_id)?;
    }

    let reminder = get_reminder(tx, &item_id)?
        .ok_or_else(|| ReconcileError::UnknownReminder(completion.reminder_id.clone()))?;
    let operation = list_operations(tx, &completion.reminder_id)?
        .into_iter()
        .find(|operation| operation.effect_identity == completion.effect_identity)
        .ok_or_else(|| ReconcileError::UnknownEffect {
            reminder_id: completion.reminder_id.clone(),
            effect_identity: completion.effect_identity.clone(),
        })?;

    match operation.operation_type {
        OperationType::Schedule => {
            schedule_completion(tx, clock, &reminder, &operation, completion.outcome)
        }
        OperationType::Cancel => {
            cancel_completion(tx, clock, &reminder, &operation, completion.outcome)
        }
    }
}

fn schedule_completion(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    reminder: &ReminderRecord,
    operation: &ReminderOperation,
    outcome: EffectOutcome,
) -> Result<CompletionOutcome, ReconcileError> {
    let succeeded = outcome == EffectOutcome::Succeeded;
    let current = matches!(
        operation.operation_state,
        OperationState::Pending | OperationState::Failed | OperationState::Rejected
    ) && reminder.request_state == RequestState::Resolved
        && reminder.notification_id().as_deref() == Some(operation.notification_id.as_str());

    if operation.operation_state == OperationState::Acknowledged {
        return Ok(CompletionOutcome::AlreadyRecorded);
    }
    if !current {
        return Ok(CompletionOutcome::Stale {
            cancel_required: succeeded,
        });
    }
    match outcome {
        EffectOutcome::Succeeded => {
            set_operation_state(tx, operation, OperationState::Acknowledged)?;
            set_schedule_state(tx, clock, reminder, ScheduleState::Scheduled)?;
            Ok(CompletionOutcome::Applied)
        }
        EffectOutcome::Failed(ErrorClass::Cancelled) => Ok(CompletionOutcome::NoChange),
        EffectOutcome::Failed(class) => {
            // A definitive rejection is terminal for failures: a later or duplicate failure
            // of any class cannot re-open it, only an explicit re-arm can.
            if operation.operation_state == OperationState::Rejected {
                return Ok(CompletionOutcome::AlreadyRecorded);
            }
            let disposition = if is_definitive_rejection(class) {
                OperationState::Rejected
            } else {
                OperationState::Failed
            };
            if operation.operation_state == disposition
                && reminder.schedule_state == ScheduleState::ScheduleFailed
            {
                return Ok(CompletionOutcome::AlreadyRecorded);
            }
            set_operation_state(tx, operation, disposition)?;
            set_schedule_state(tx, clock, reminder, ScheduleState::ScheduleFailed)?;
            Ok(CompletionOutcome::Applied)
        }
    }
}

/// Explicit recovery for a schedule the native layer rejected definitively: re-arm the
/// current generation's `Rejected` schedule operation as `Pending` so the next reconciliation
/// attempts it again, and return the reminder to `pending_schedule`. Called by a user retry or
/// a permission/configuration change (N04), never by reconciliation itself. Returns whether an
/// operation was re-armed; a reminder without a rejected current schedule is left untouched.
pub fn retry_rejected_schedule(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    item_id: &str,
) -> Result<bool, ReconcileError> {
    let Some(reminder) = get_reminder(tx, item_id)? else {
        return Ok(false);
    };
    let Some(notification_id) = reminder.notification_id() else {
        return Ok(false);
    };
    if reminder.request_state != RequestState::Resolved {
        return Ok(false);
    }
    let identity = format!("{}:{}", OperationType::Schedule.as_str(), notification_id);
    let Some(operation) = list_operations(tx, &reminder.reminder_id)?
        .into_iter()
        .find(|operation| operation.effect_identity == identity)
    else {
        return Ok(false);
    };
    if operation.operation_state != OperationState::Rejected {
        return Ok(false);
    }
    set_operation_state(tx, &operation, OperationState::Pending)?;
    set_schedule_state(tx, clock, &reminder, ScheduleState::PendingSchedule)?;
    Ok(true)
}

fn cancel_completion(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    reminder: &ReminderRecord,
    operation: &ReminderOperation,
    outcome: EffectOutcome,
) -> Result<CompletionOutcome, ReconcileError> {
    if !matches!(
        operation.operation_state,
        OperationState::Pending | OperationState::Failed
    ) {
        return Ok(CompletionOutcome::AlreadyRecorded);
    }
    match outcome {
        EffectOutcome::Succeeded => {
            set_operation_state(tx, operation, OperationState::Acknowledged)?;
            let latest_scheduled = list_operations(tx, &reminder.reminder_id)?
                .into_iter()
                .rfind(|candidate| candidate.operation_type == OperationType::Schedule);
            let describes_cancelled_generation = reminder.request_state != RequestState::Resolved
                && latest_scheduled.is_some_and(|scheduled| {
                    scheduled.notification_id == operation.notification_id
                });
            if describes_cancelled_generation {
                set_schedule_state(tx, clock, reminder, ScheduleState::NotScheduled)?;
            }
            Ok(CompletionOutcome::Applied)
        }
        EffectOutcome::Failed(ErrorClass::Cancelled) => Ok(CompletionOutcome::NoChange),
        EffectOutcome::Failed(_) => {
            if operation.operation_state == OperationState::Failed {
                return Ok(CompletionOutcome::AlreadyRecorded);
            }
            set_operation_state(tx, operation, OperationState::Failed)?;
            Ok(CompletionOutcome::Applied)
        }
    }
}

fn set_operation_state(
    tx: &Transaction<'_>,
    operation: &ReminderOperation,
    state: OperationState,
) -> Result<(), ReconcileError> {
    tx.execute(
        "UPDATE reminder_operations SET operation_state = ? WHERE operation_id = ?",
        rusqlite::params![state.as_str(), &operation.operation_id],
    )?;
    Ok(())
}

fn set_schedule_state(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    reminder: &ReminderRecord,
    state: ScheduleState,
) -> Result<(), ReconcileError> {
    if reminder.schedule_state == state {
        return Ok(());
    }
    tx.execute(
        "UPDATE reminders SET schedule_state = ?, updated_at = ? WHERE reminder_id = ?",
        rusqlite::params![
            state.as_str(),
            clock
                .now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            &reminder.reminder_id
        ],
    )?;
    Ok(())
}
