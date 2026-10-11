use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Transaction;

use super::completion::{apply_completion, CompletionOutcome, EffectCompletion, EffectOutcome};
use super::effects::{
    NotificationEffectError, NotificationEffects, PayloadKind, PendingRequest, ScheduleRequest,
};
use super::ReconcileError;
use crate::providers::contracts::ErrorClass;
use crate::reminders::state::{
    desired_notifications, get_reminder, list_operations, reconcile_inactive_items,
    DesiredNotification, OperationState, OperationType, UnschedulableReason,
};
use crate::store::schema::{Clock, Database};

/// One native effect that failed during a pass. Left durable and retried by the next pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectFailure {
    pub notification_id: String,
    pub operation_type: OperationType,
    pub class: ErrorClass,
    pub code: String,
}

/// What a pass did, as notification identifiers in processing order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Scheduled with the OS during this pass.
    pub installed: Vec<String>,
    /// Already pending in the OS with the desired due instant; no OS call was made.
    pub already_installed: Vec<String>,
    /// Stale requests cancelled in the OS.
    pub removed: Vec<String>,
    /// Cancel operations whose request was already absent from the OS.
    pub already_absent: Vec<String>,
    /// Never attempted and past due: now `Unschedulable(TimeInPast)`.
    pub expired: Vec<String>,
    /// Attempted, unconfirmed, absent from the OS and past due: it may have fired, so nothing
    /// is concluded and no reminder state changes.
    pub unconfirmed: Vec<String>,
    /// Installs that completed after their operation was superseded and were cancelled again.
    pub rolled_back: Vec<String>,
    pub failures: Vec<EffectFailure>,
}

impl ReconcileReport {
    pub fn converged(&self) -> bool {
        self.failures.is_empty()
    }
}

struct Plan {
    desired: Vec<DesiredNotification>,
    known_reminders: BTreeSet<String>,
    open_cancels: Vec<(String, String)>,
}

/// Bring the OS pending set into agreement with the durable desired state. Safe to run after
/// every write, retry and relaunch: a second pass over converged state makes no OS mutation.
///
/// A failure to list pending requests aborts the pass before any OS mutation. Failures of
/// individual effects are recorded in the report and in the operation rows, and do not stop
/// the remaining work.
pub fn reconcile(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
) -> Result<ReconcileReport, ReconcileError> {
    let mut report = ReconcileReport::default();

    {
        let tx = database.immediate_transaction().map_err(database_error)?;
        reconcile_inactive_items(&tx, clock)?;
        tx.commit()?;
    }

    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let pending_by_id: BTreeMap<String, PendingRequest> = pending
        .into_iter()
        .map(|request| (request.notification_id.clone(), request))
        .collect();

    let plan = {
        let tx = database.transaction().map_err(database_error)?;
        load_plan(&tx)?
    };
    let desired_ids: BTreeSet<&str> = plan
        .desired
        .iter()
        .map(|entry| entry.notification_id.as_str())
        .collect();

    let mut to_cancel: BTreeSet<String> = pending_by_id
        .keys()
        .filter(|id| {
            !desired_ids.contains(id.as_str())
                && reminder_id_of(id)
                    .is_some_and(|reminder_id| plan.known_reminders.contains(reminder_id))
        })
        .cloned()
        .collect();
    for (_, notification_id) in &plan.open_cancels {
        if pending_by_id.contains_key(notification_id)
            && !desired_ids.contains(notification_id.as_str())
        {
            to_cancel.insert(notification_id.clone());
        }
    }

    for (reminder_id, notification_id) in &plan.open_cancels {
        if !pending_by_id.contains_key(notification_id) {
            record_effect(
                database,
                clock,
                reminder_id,
                OperationType::Cancel,
                notification_id,
                EffectOutcome::Succeeded,
            )?;
            report.already_absent.push(notification_id.clone());
        }
    }

    for notification_id in &to_cancel {
        if cancel_in_os(database, clock, effects, notification_id, &mut report)? {
            report.removed.push(notification_id.clone());
        }
    }

    let now = clock.now();
    for entry in &plan.desired {
        let installed_with_desired_due =
            pending_by_id
                .get(&entry.notification_id)
                .is_some_and(|request| {
                    request.fire_at.map(|due| due.timestamp()) == Some(entry.fire_at.timestamp())
                });
        if installed_with_desired_due {
            let outcome = record_effect(
                database,
                clock,
                &entry.reminder_id,
                OperationType::Schedule,
                &entry.notification_id,
                EffectOutcome::Succeeded,
            )?;
            if !roll_back_if_stale(database, clock, effects, entry, outcome, &mut report)? {
                report.already_installed.push(entry.notification_id.clone());
            }
            continue;
        }
        if entry.fire_at <= now {
            // The desired instant has passed, so the request cannot be replaced; a same-ID
            // request reporting another due instant would fire at the wrong time and is
            // removed. One with an unreported instant may be the desired request about to be
            // delivered, so it is left alone. The durable state is concluded below as usual.
            let pending_at_another_due = pending_by_id
                .get(&entry.notification_id)
                .is_some_and(|request| request.fire_at.is_some());
            if pending_at_another_due
                && cancel_in_os(
                    database,
                    clock,
                    effects,
                    &entry.notification_id,
                    &mut report,
                )?
            {
                report.removed.push(entry.notification_id.clone());
            }
            match expire_if_never_attempted(database, clock, entry)? {
                ExpiryOutcome::Expired => report.expired.push(entry.notification_id.clone()),
                ExpiryOutcome::Unconfirmed => {
                    report.unconfirmed.push(entry.notification_id.clone())
                }
                ExpiryOutcome::Untouched => {}
            }
            continue;
        }
        schedule_in_os(database, clock, effects, entry, &mut report)?;
    }

    Ok(report)
}

/// After a request is confirmed live in the OS, cancel it again if it is no longer desired.
/// The completion outcome alone is not enough: an already-acknowledged operation reports
/// `AlreadyRecorded` without consulting current state, and the plan is a snapshot. Returns
/// whether the request was rolled back.
fn roll_back_if_stale(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    entry: &DesiredNotification,
    outcome: Option<CompletionOutcome>,
    report: &mut ReconcileReport,
) -> Result<bool, ReconcileError> {
    let stale_success = matches!(
        outcome,
        Some(CompletionOutcome::Stale {
            cancel_required: true
        })
    );
    if !stale_success && is_still_desired(database, &entry.notification_id)? {
        return Ok(false);
    }
    report.rolled_back.push(entry.notification_id.clone());
    cancel_in_os(database, clock, effects, &entry.notification_id, report)?;
    Ok(true)
}

/// Re-read the durable desired set. The plan is a snapshot, so a cancel, change or deletion
/// committed after it was taken must still stop the request it describes from staying live.
fn is_still_desired(
    database: &mut Database,
    notification_id: &str,
) -> Result<bool, ReconcileError> {
    let tx = database.transaction().map_err(database_error)?;
    Ok(desired_notifications(&tx)?
        .iter()
        .any(|entry| entry.notification_id == notification_id))
}

fn load_plan(tx: &Transaction<'_>) -> Result<Plan, ReconcileError> {
    let desired = desired_notifications(tx)?;
    let known_reminders = tx
        .prepare("SELECT reminder_id FROM reminders")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let open_cancels = tx
        .prepare(
            "SELECT reminder_id, external_id FROM reminder_operations
             WHERE operation_type = 'cancel' AND operation_state IN ('pending', 'failed')
               AND external_id IS NOT NULL
             ORDER BY created_at, rowid",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Plan {
        desired,
        known_reminders,
        open_cancels,
    })
}

fn reminder_id_of(notification_id: &str) -> Option<&str> {
    notification_id
        .rsplit_once('#')
        .map(|(reminder_id, _)| reminder_id)
}

fn database_error(error: anyhow::Error) -> ReconcileError {
    ReconcileError::Database(error.to_string())
}

fn effect_identity(operation_type: OperationType, notification_id: &str) -> String {
    format!("{}:{}", operation_type.as_str(), notification_id)
}

/// Record an effect result against its operation when core has one; a request core never
/// issued (for example a rolled-back install of a deleted reminder's operation) has no row.
fn record_effect(
    database: &mut Database,
    clock: &dyn Clock,
    reminder_id: &str,
    operation_type: OperationType,
    notification_id: &str,
    outcome: EffectOutcome,
) -> Result<Option<CompletionOutcome>, ReconcileError> {
    let identity = effect_identity(operation_type, notification_id);
    let tx = database.immediate_transaction().map_err(database_error)?;
    let recorded = list_operations(&tx, reminder_id)?
        .iter()
        .any(|operation| operation.effect_identity == identity);
    if !recorded {
        return Ok(None);
    }
    let result = apply_completion(
        &tx,
        clock,
        &EffectCompletion {
            reminder_id: reminder_id.to_string(),
            effect_identity: identity,
            outcome,
        },
    )?;
    tx.commit()?;
    Ok(Some(result))
}

fn cancel_in_os(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    notification_id: &str,
    report: &mut ReconcileReport,
) -> Result<bool, ReconcileError> {
    let Some(reminder_id) = reminder_id_of(notification_id) else {
        return Ok(false);
    };
    match effects.cancel(notification_id) {
        Ok(()) => {
            record_effect(
                database,
                clock,
                reminder_id,
                OperationType::Cancel,
                notification_id,
                EffectOutcome::Succeeded,
            )?;
            Ok(true)
        }
        Err(error) => {
            record_effect(
                database,
                clock,
                reminder_id,
                OperationType::Cancel,
                notification_id,
                EffectOutcome::Failed(error.class),
            )?;
            report
                .failures
                .push(failure(notification_id, OperationType::Cancel, error));
            Ok(false)
        }
    }
}

fn schedule_in_os(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    entry: &DesiredNotification,
    report: &mut ReconcileReport,
) -> Result<(), ReconcileError> {
    mark_schedule_attempted(database, entry)?;
    let request = ScheduleRequest {
        notification_id: entry.notification_id.clone(),
        fire_at: entry.fire_at,
        payload_kind: PayloadKind::Generic,
        opaque_target_id: entry.item_id.clone(),
    };
    match effects.schedule(&request) {
        Ok(()) => {
            let outcome = record_effect(
                database,
                clock,
                &entry.reminder_id,
                OperationType::Schedule,
                &entry.notification_id,
                EffectOutcome::Succeeded,
            )?;
            if !roll_back_if_stale(database, clock, effects, entry, outcome, report)? {
                report.installed.push(entry.notification_id.clone());
            }
        }
        Err(error) => {
            let outcome = record_effect(
                database,
                clock,
                &entry.reminder_id,
                OperationType::Schedule,
                &entry.notification_id,
                EffectOutcome::Failed(error.class),
            )?;
            report.failures.push(failure(
                &entry.notification_id,
                OperationType::Schedule,
                error,
            ));
            // A failed call may still have installed the request (a cancelled caller, for
            // example). Cancel is idempotent, so a request that is no longer desired is
            // removed whatever the error class.
            roll_back_if_stale(database, clock, effects, entry, outcome, report)?;
        }
    }
    Ok(())
}

fn failure(
    notification_id: &str,
    operation_type: OperationType,
    error: NotificationEffectError,
) -> EffectFailure {
    EffectFailure {
        notification_id: notification_id.to_string(),
        operation_type,
        class: error.class,
        code: error.code,
    }
}

/// Durably record, before the native call, that this schedule was attempted. Committed in its
/// own transaction so it survives a crash anywhere after the OS call starts.
fn mark_schedule_attempted(
    database: &mut Database,
    entry: &DesiredNotification,
) -> Result<(), ReconcileError> {
    let tx = database.immediate_transaction().map_err(database_error)?;
    tx.execute(
        "UPDATE reminder_operations SET operation_state = ?
         WHERE reminder_id = ? AND effect_identity = ? AND operation_state = ?",
        rusqlite::params![
            OperationState::Failed.as_str(),
            &entry.reminder_id,
            effect_identity(OperationType::Schedule, &entry.notification_id),
            OperationState::Pending.as_str(),
        ],
    )?;
    tx.commit()?;
    Ok(())
}

enum ExpiryOutcome {
    Expired,
    Unconfirmed,
    Untouched,
}

/// A due time that has passed for a reminder whose schedule was never attempted cannot be
/// installed any more: the opportunity is kept as `Unschedulable(TimeInPast)` with the instant
/// preserved. A schedule that was attempted or acknowledged may have installed a request that
/// has since fired or been dismissed, so absence from the pending set proves nothing and no
/// reminder state is changed.
fn expire_if_never_attempted(
    database: &mut Database,
    clock: &dyn Clock,
    entry: &DesiredNotification,
) -> Result<ExpiryOutcome, ReconcileError> {
    let tx = database.immediate_transaction().map_err(database_error)?;
    let Some(reminder) = get_reminder(&tx, &entry.item_id)? else {
        return Ok(ExpiryOutcome::Untouched);
    };
    if reminder.notification_id().as_deref() != Some(entry.notification_id.as_str()) {
        return Ok(ExpiryOutcome::Untouched);
    }
    let identity = effect_identity(OperationType::Schedule, &entry.notification_id);
    let attempt_state = list_operations(&tx, &reminder.reminder_id)?
        .iter()
        .find(|operation| operation.effect_identity == identity)
        .map(|operation| operation.operation_state);
    match attempt_state {
        Some(OperationState::Acknowledged) => return Ok(ExpiryOutcome::Untouched),
        Some(OperationState::Failed) => return Ok(ExpiryOutcome::Unconfirmed),
        _ => {}
    }
    let changed = tx.execute(
        "UPDATE reminders SET request_state = 'unschedulable', unschedulable_reason = ?,
                schedule_state = 'not_scheduled', state_version = state_version + 1,
                updated_at = ?
         WHERE reminder_id = ? AND request_state = 'resolved' AND schedule_generation = ?",
        rusqlite::params![
            UnschedulableReason::TimeInPast.as_str(),
            format_instant(clock.now()),
            &reminder.reminder_id,
            reminder.schedule_generation,
        ],
    )?;
    tx.execute(
        "UPDATE reminder_operations SET operation_state = 'superseded'
         WHERE reminder_id = ? AND effect_identity = ? AND operation_state = 'pending'",
        rusqlite::params![&reminder.reminder_id, identity],
    )?;
    tx.commit()?;
    Ok(if changed > 0 {
        ExpiryOutcome::Expired
    } else {
        ExpiryOutcome::Untouched
    })
}

fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}
