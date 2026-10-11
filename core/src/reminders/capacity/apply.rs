use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Transaction;

use super::allocation::ReminderDemand;
use super::allocation::{
    allocate, Allocation, AllocationInput, CapacityPolicy, DeferredPrompt, PromptDemand,
};
use crate::providers::contracts::ErrorClass;
use crate::reminders::reconcile::{
    reconcile, EffectFailure, NotificationEffectError, NotificationEffects, PendingRequest,
    ReconcileError, ReconcileReport, ScheduleRequest,
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
/// A prompt request whose cancel fails is still pending, so it is charged against the budget:
/// the allocation is planned again with it counted as a request core cannot reclaim, any
/// reminder that no longer fits becomes `unschedulable(capacity_exceeded)`, and any prompt the
/// smaller budget newly defers is cancelled in turn, until every deferred prompt is either
/// cancelled or reported in `prompt_cancel_failures`. Only the final allocation is written. As
/// a last guard the effects handed to `reconcile` refuse to add a request while the pending set is at the OS
/// limit (a transient failure, retried by the next pass), whatever other cancel failed.
///
/// Safe to run after every write, on launch and on foreground activation; those events are the
/// refill cadence and no timer is involved. A failure to list pending requests aborts the pass
/// before any mutation. A failed prompt cancel is reported and retried by the next pass.
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

    let pending_ids: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.notification_id.as_str())
        .collect();
    let mut report = CapacityReport::default();
    // Prompts whose cancel failed: still pending, so they stay out of the demands handed to
    // `plan` and are counted with the requests core cannot reclaim. Each failure can shrink
    // the budget and defer more prompts, so cancel and plan repeat until no new cancel fails.
    // Every round that continues adds at least one failed prompt, so the loop terminates.
    let mut failed_deferrals: BTreeMap<String, DeferredPrompt> = BTreeMap::new();
    let mut cancelled: BTreeSet<String> = BTreeSet::new();
    loop {
        let remaining_prompts = remaining_prompts_of(prompts, &failed_deferrals);
        let mut allocation = plan(database, clock, policy, &remaining_prompts, &pending)?;

        let to_cancel: Vec<String> = allocation
            .deferred_prompts
            .iter()
            .filter(|prompt| {
                pending_ids.contains(prompt.notification_id.as_str())
                    && !cancelled.contains(&prompt.notification_id)
            })
            .map(|prompt| prompt.notification_id.clone())
            .collect();
        let mut new_failures = false;
        for notification_id in to_cancel {
            match effects.cancel(&notification_id) {
                Ok(()) => {
                    cancelled.insert(notification_id.clone());
                    report.prompts_cancelled.push(notification_id);
                }
                Err(error) => {
                    new_failures = true;
                    if let Some(deferral) = allocation
                        .deferred_prompts
                        .iter()
                        .find(|prompt| prompt.notification_id == notification_id)
                    {
                        failed_deferrals.insert(notification_id.clone(), deferral.clone());
                    }
                    report.prompt_cancel_failures.push(EffectFailure {
                        notification_id,
                        operation_type: OperationType::Cancel,
                        class: error.class,
                        code: error.code,
                    });
                }
            }
        }

        if !new_failures {
            for failed in failed_deferrals.values() {
                allocation.deferred_prompts.push(failed.clone());
            }
            report.allocation = allocation;
            break;
        }
    }
    // Only the final allocation is written, so a pass that changes nothing leaves every
    // reminder's `state_version` alone however many cancels failed on the way.
    settle(
        database,
        clock,
        policy,
        &remaining_prompts_of(prompts, &failed_deferrals),
        &pending,
        &mut report,
    )?;

    let mut guarded = PendingLimitGuard::new(effects, pending, policy.os_pending_limit);
    report.reconcile = reconcile(database, clock, &mut guarded)?;
    Ok(report)
}

fn remaining_prompts_of(
    prompts: &[PromptDemand],
    failed_deferrals: &BTreeMap<String, DeferredPrompt>,
) -> Vec<PromptDemand> {
    prompts
        .iter()
        .filter(|prompt| !failed_deferrals.contains_key(&prompt.notification_id))
        .cloned()
        .collect()
}

/// The allocation from the durable reminders and the pending list, plus the records it was
/// computed from. Pending requests that are neither known reminders nor in `prompts` reduce the
/// budget.
fn compute_allocation(
    tx: &Transaction<'_>,
    clock: &dyn Clock,
    policy: &CapacityPolicy,
    prompts: &[PromptDemand],
    pending: &[PendingRequest],
) -> Result<(BTreeMap<String, ReminderRecord>, Allocation), ReconcileError> {
    let prompt_ids: BTreeSet<&str> = prompts
        .iter()
        .map(|prompt| prompt.notification_id.as_str())
        .collect();
    let known_reminders = known_reminder_ids(tx)?;
    let foreign_pending = pending
        .iter()
        .filter(|request| {
            !prompt_ids.contains(request.notification_id.as_str())
                && !reminder_id_of(&request.notification_id)
                    .is_some_and(|reminder_id| known_reminders.contains(reminder_id))
        })
        .count();
    let records = capacity_candidates(tx)?;
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
    let allocation = allocate(
        policy,
        &AllocationInput {
            now: clock.now(),
            foreign_pending,
            reminders: &demands,
            prompts,
        },
    );
    Ok((records, allocation))
}

/// The allocation without writing anything.
fn plan(
    database: &mut Database,
    clock: &dyn Clock,
    policy: &CapacityPolicy,
    prompts: &[PromptDemand],
    pending: &[PendingRequest],
) -> Result<Allocation, ReconcileError> {
    let tx = database.immediate_transaction().map_err(database_error)?;
    let (_, allocation) = compute_allocation(&tx, clock, policy, prompts, pending)?;
    Ok(allocation)
}

/// One transaction: compute the allocation and apply it.
fn settle(
    database: &mut Database,
    clock: &dyn Clock,
    policy: &CapacityPolicy,
    prompts: &[PromptDemand],
    pending: &[PendingRequest],
    report: &mut CapacityReport,
) -> Result<Allocation, ReconcileError> {
    let tx = database.immediate_transaction().map_err(database_error)?;
    let (records, allocation) = compute_allocation(&tx, clock, policy, prompts, pending)?;
    apply_allocation(&tx, clock.now(), &records, &allocation, report)?;
    tx.commit()?;
    Ok(allocation)
}

/// Wraps the effects handed to `reconcile` and never lets the pending set grow past the OS
/// limit, which would make the OS evict a request silently. It follows the pending set from
/// the listing it was given and from the effects that succeed; a schedule that would add a new
/// request at the limit fails as transient without reaching the OS, so the reminder stays
/// visibly unconfirmed and the next pass retries it.
struct PendingLimitGuard<'a> {
    inner: &'a mut dyn NotificationEffects,
    pending: BTreeSet<String>,
    limit: usize,
}

impl<'a> PendingLimitGuard<'a> {
    fn new(
        inner: &'a mut dyn NotificationEffects,
        pending: Vec<PendingRequest>,
        limit: usize,
    ) -> PendingLimitGuard<'a> {
        PendingLimitGuard {
            inner,
            pending: pending
                .into_iter()
                .map(|request| request.notification_id)
                .collect(),
            limit,
        }
    }
}

impl NotificationEffects for PendingLimitGuard<'_> {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        if !self.pending.contains(&request.notification_id) && self.pending.len() >= self.limit {
            return Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "pending_limit_reached",
            ));
        }
        self.inner.schedule(request)?;
        self.pending.insert(request.notification_id.clone());
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.inner.cancel(notification_id)?;
        self.pending.remove(notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        let listed = self.inner.list_pending()?;
        self.pending = listed
            .iter()
            .map(|request| request.notification_id.clone())
            .collect();
        Ok(listed)
    }
}

fn apply_allocation(
    tx: &Transaction<'_>,
    now: DateTime<Utc>,
    records: &BTreeMap<String, ReminderRecord>,
    allocation: &Allocation,
    report: &mut CapacityReport,
) -> Result<(), ReconcileError> {
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
