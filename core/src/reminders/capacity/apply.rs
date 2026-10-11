use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Transaction;

use super::allocation::ReminderDemand;
use super::allocation::{allocate, Allocation, AllocationInput, CapacityPolicy, PromptDemand};
use crate::reminders::reconcile::{
    reconcile, EffectFailure, NotificationEffects, ReconcileError, ReconcileReport,
};
use crate::reminders::state::{
    get_reminder, list_operations, OperationState, OperationType, ReminderRecord, RequestState,
    ScheduleState, UnschedulableReason,
};
use crate::store::schema::{Clock, Database};

/// What a capacity pass changed. Reminder identifiers are reminder IDs, never content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapacityReport {
    pub allocation: Allocation,
    /// Moved from `resolved` to `unschedulable(capacity_exceeded)` on this pass.
    pub demoted: Vec<String>,
    /// Returned from `unschedulable(capacity_exceeded)` to `resolved` on this pass, with the
    /// same schedule generation and therefore the same native identifier.
    pub refilled: Vec<String>,
    /// Held back for capacity and then past due before a slot freed: now
    /// `unschedulable(time_in_past)`, as for any reminder that was never installed.
    pub expired: Vec<String>,
    /// Pending prompt requests cancelled because explicit reminders needed their slot.
    pub prompts_cancelled: Vec<String>,
    pub prompt_cancel_failures: Vec<EffectFailure>,
    pub reconcile: ReconcileReport,
}

impl CapacityReport {
    pub fn converged(&self) -> bool {
        self.prompt_cancel_failures.is_empty() && self.reconcile.converged()
    }
}

/// Apply the capacity rule, then reconcile the OS with the result.
///
/// Order matters for the OS: durable reminder state is settled first (overflow reminders stop
/// being desired, refilled ones become desired again), requests of prompts that no longer fit
/// are cancelled next, and only then does `reconcile` run. It cancels every request outside the
/// desired set before it installs anything, so the pending set never grows past the planned
/// budget and the OS never has to evict silently.
///
/// Safe to run after every write, on launch and on foreground activation; those events are the
/// refill cadence and no timer is involved. A failure to list pending requests aborts the pass
/// before any mutation. A failed prompt cancel is reported and retried by the next pass; the
/// replacement headroom is what keeps the pending set under the OS limit meanwhile.
pub fn reconcile_with_capacity(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    policy: &CapacityPolicy,
    prompts: &[PromptDemand],
) -> Result<CapacityReport, ReconcileError> {
    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let prompt_ids: BTreeSet<&str> = prompts
        .iter()
        .map(|prompt| prompt.notification_id.as_str())
        .collect();

    let mut report = CapacityReport::default();
    {
        let tx = database.immediate_transaction().map_err(database_error)?;
        let known_reminders = known_reminder_ids(&tx)?;
        let foreign_pending = pending
            .iter()
            .filter(|request| {
                !prompt_ids.contains(request.notification_id.as_str())
                    && !reminder_id_of(&request.notification_id)
                        .is_some_and(|reminder_id| known_reminders.contains(reminder_id))
            })
            .count();
        let records = capacity_candidates(&tx)?;
        let demands: Vec<ReminderDemand> = records
            .values()
            .filter_map(|record| {
                Some(ReminderDemand {
                    reminder_id: record.reminder_id.clone(),
                    fire_at: record.resolved_instant?,
                    created_at: record.created_at,
                })
            })
            .collect();
        report.allocation = allocate(
            policy,
            &AllocationInput {
                now: clock.now(),
                foreign_pending,
                reminders: &demands,
                prompts,
            },
        );
        apply_allocation(&tx, clock.now(), &records, &mut report)?;
        tx.commit()?;
    }

    let pending_ids: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.notification_id.as_str())
        .collect();
    let to_cancel: Vec<String> = report
        .allocation
        .deferred_prompts
        .iter()
        .filter(|prompt| pending_ids.contains(prompt.notification_id.as_str()))
        .map(|prompt| prompt.notification_id.clone())
        .collect();
    for notification_id in to_cancel {
        match effects.cancel(&notification_id) {
            Ok(()) => report.prompts_cancelled.push(notification_id),
            Err(error) => report.prompt_cancel_failures.push(EffectFailure {
                notification_id,
                operation_type: OperationType::Cancel,
                class: error.class,
                code: error.code,
            }),
        }
    }

    report.reconcile = reconcile(database, clock, effects)?;
    Ok(report)
}

fn apply_allocation(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    records: &BTreeMap<String, ReminderRecord>,
    report: &mut CapacityReport,
) -> Result<(), ReconcileError> {
    let allocation = &report.allocation;
    let overflow = allocation.overflow_reminders.clone();
    let granted = allocation.granted_reminders.clone();
    let elapsed = allocation.elapsed_reminders.clone();

    for reminder_id in overflow {
        let record = &records[&reminder_id];
        if record.request_state == RequestState::Resolved && demote(tx, now, record)? {
            report.demoted.push(reminder_id);
        }
    }
    for reminder_id in granted {
        let record = &records[&reminder_id];
        if is_capacity_exceeded(record) && refill(tx, now, record)? {
            report.refilled.push(reminder_id);
        }
    }
    for reminder_id in elapsed {
        let record = &records[&reminder_id];
        if is_capacity_exceeded(record) && expire(tx, now, record)? {
            report.expired.push(reminder_id);
        }
    }
    Ok(())
}

fn is_capacity_exceeded(record: &ReminderRecord) -> bool {
    record.request_state == RequestState::Unschedulable(UnschedulableReason::CapacityExceeded)
}

/// Keeps the schedule generation, so the native identifier survives a later refill. The
/// generation's schedule operation is left as recorded, and an OS request that may still be
/// pending is cancelled by `reconcile`, which removes every request of a known reminder outside
/// the desired set. `schedule_state` reports `not_scheduled` because the reminder is no longer
/// desired, not because the cancel was confirmed.
fn demote(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    record: &ReminderRecord,
) -> Result<bool, ReconcileError> {
    let changed = tx.execute(
        "UPDATE reminders SET request_state = 'unschedulable', unschedulable_reason = ?,
                schedule_state = 'not_scheduled', state_version = state_version + 1,
                updated_at = ?
         WHERE reminder_id = ? AND request_state = 'resolved' AND schedule_generation = ?",
        rusqlite::params![
            UnschedulableReason::CapacityExceeded.as_str(),
            format_instant(now),
            &record.reminder_id,
            record.schedule_generation,
        ],
    )?;
    Ok(changed > 0)
}

/// Same generation, same identifier. A schedule the OS rejected definitively stays rejected
/// and is withheld until an explicit recovery re-arms it; every other state is retried by the
/// next reconciliation.
fn refill(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    record: &ReminderRecord,
) -> Result<bool, ReconcileError> {
    let identity = record
        .notification_id()
        .map(|notification_id| format!("{}:{}", OperationType::Schedule.as_str(), notification_id));
    let rejected = match identity {
        Some(identity) => list_operations(tx, &record.reminder_id)?
            .iter()
            .any(|operation| {
                operation.effect_identity == identity
                    && operation.operation_state == OperationState::Rejected
            }),
        None => false,
    };
    let schedule_state = if rejected {
        ScheduleState::ScheduleFailed
    } else {
        ScheduleState::PendingSchedule
    };
    let changed = tx.execute(
        "UPDATE reminders SET request_state = 'resolved', unschedulable_reason = NULL,
                schedule_state = ?, state_version = state_version + 1, updated_at = ?
         WHERE reminder_id = ? AND request_state = 'unschedulable'
           AND unschedulable_reason = ? AND schedule_generation = ?",
        rusqlite::params![
            schedule_state.as_str(),
            format_instant(now),
            &record.reminder_id,
            UnschedulableReason::CapacityExceeded.as_str(),
            record.schedule_generation,
        ],
    )?;
    Ok(changed > 0)
}

/// The time passed while the reminder was held back, so it can never be refilled. The instant
/// is kept and nothing is adjusted; no delivery or acknowledgment fact is touched.
fn expire(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    record: &ReminderRecord,
) -> Result<bool, ReconcileError> {
    let changed = tx.execute(
        "UPDATE reminders SET unschedulable_reason = ?, state_version = state_version + 1,
                updated_at = ?
         WHERE reminder_id = ? AND request_state = 'unschedulable'
           AND unschedulable_reason = ? AND schedule_generation = ?",
        rusqlite::params![
            UnschedulableReason::TimeInPast.as_str(),
            format_instant(now),
            &record.reminder_id,
            UnschedulableReason::CapacityExceeded.as_str(),
            record.schedule_generation,
        ],
    )?;
    Ok(changed > 0)
}

/// Resolved reminders of active items, and those of active items held back for capacity,
/// keyed by reminder ID.
fn capacity_candidates(
    tx: &Transaction<'_>,
) -> Result<BTreeMap<String, ReminderRecord>, ReconcileError> {
    let item_ids = tx
        .prepare(
            "SELECT reminders.item_id FROM reminders
             JOIN items ON items.item_id = reminders.item_id
             WHERE items.lifecycle_state = 'active' AND reminders.resolved_instant IS NOT NULL
               AND (reminders.request_state = 'resolved'
                    OR (reminders.request_state = 'unschedulable'
                        AND reminders.unschedulable_reason = ?))
             ORDER BY reminders.reminder_id",
        )?
        .query_map([UnschedulableReason::CapacityExceeded.as_str()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut records = BTreeMap::new();
    for item_id in item_ids {
        if let Some(record) = get_reminder(tx, &item_id)? {
            records.insert(record.reminder_id.clone(), record);
        }
    }
    Ok(records)
}

fn known_reminder_ids(tx: &Transaction<'_>) -> Result<BTreeSet<String>, ReconcileError> {
    Ok(tx
        .prepare("SELECT reminder_id FROM reminders")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?)
}

fn reminder_id_of(notification_id: &str) -> Option<&str> {
    notification_id
        .rsplit_once('#')
        .map(|(reminder_id, _)| reminder_id)
}

fn database_error(error: anyhow::Error) -> ReconcileError {
    ReconcileError::Database(error.to_string())
}

fn format_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}
