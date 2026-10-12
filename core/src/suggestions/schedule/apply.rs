use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::plan::{is_prompt_notification_id, PromptSchedule};
use crate::providers::contracts::ErrorClass;
use crate::reminders::capacity::{
    reconcile_with_capacity, CapacityPolicy, CapacityReport, PromptDemand,
};
use crate::reminders::reconcile::{
    NotificationEffects, PayloadKind, PendingRequest, ReconcileError, ScheduleRequest,
};
use crate::store::schema::{Clock, Database};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptOperation {
    Schedule,
    Cancel,
}

/// A native prompt effect that failed. Identifiers and classes only, never content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFailure {
    pub notification_id: String,
    pub operation: PromptOperation,
    pub class: ErrorClass,
    pub code: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptPassReport {
    /// The capacity pass, absent when prompting is off: switching it off touches no reminder.
    pub capacity: Option<CapacityReport>,
    pub installed: Vec<String>,
    pub already_installed: Vec<String>,
    pub cancelled: Vec<String>,
    pub failures: Vec<PromptFailure>,
}

impl PromptPassReport {
    pub fn converged(&self) -> bool {
        self.failures.is_empty() && self.capacity.as_ref().is_none_or(|c| c.converged())
    }
}

/// Cancel every pending daily prompt and nothing else. Reminders and requests of other
/// features are never touched. Cancellation is idempotent. A failed cancel is reported and
/// retried by the next pass.
pub fn cancel_prompts(
    effects: &mut dyn NotificationEffects,
) -> Result<PromptPassReport, ReconcileError> {
    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let mut report = PromptPassReport::default();
    cancel_where(effects, &pending, &mut report, |_| true);
    Ok(report)
}

/// Bring the OS in line with the schedule and report what changed.
///
/// Off: every pending prompt is cancelled and no capacity pass runs, so reminders are left
/// exactly as they were.
///
/// On: pending prompts outside the plan (an earlier time, another zone, a passed day) are
/// cancelled first so they stop consuming budget. The capacity pass then plans the offered
/// prompts after explicit reminders and cancels any it defers. Last, the granted prompts that
/// the OS does not already hold at the planned instant are installed with the generic payload
/// and their own opaque identifier as target. A failed install is reported and retried by any
/// later pass; it is never recorded as delivery. This function never extends the horizon:
/// only the schedule's own refresh rules do. It does retire occurrences the OS held but no
/// longer holds although core did not cancel them, so a prompt that fired is not reinstalled if
/// the clock is moved back, while a prompt core cancelled for explicit reminders is kept and
/// reinstalled when a slot frees. It records which prompts the OS now holds, and which core
/// cancelled, for the next pass.
pub fn reconcile_prompts(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    schedule: &mut PromptSchedule,
    policy: &CapacityPolicy,
) -> Result<PromptPassReport, ReconcileError> {
    if !schedule.is_enabled() {
        return cancel_prompts(effects);
    }
    let pass_clock = PassClock { now: clock.now() };
    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let pending_ids: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.notification_id.as_str())
        .collect();
    schedule.retire_elapsed(pass_clock.now);
    // A prompt the OS held and no longer holds, which core did not cancel, fired and the clock
    // moved back. Retiring it before allocation gives its slot to the next occurrence.
    schedule.retire_fired(pass_clock.now, &pending_ids);

    let mut report = PromptPassReport::default();
    let (demands, capacity) = capacity_pass(
        database,
        &pass_clock,
        effects,
        schedule,
        policy,
        &pending,
        &mut report,
    )?;
    let held: BTreeMap<&str, Option<DateTime<Utc>>> = pending
        .iter()
        .filter(|request| !report.cancelled.contains(&request.notification_id))
        .map(|request| (request.notification_id.as_str(), request.fire_at))
        .collect();
    for notification_id in &capacity.allocation.granted_prompts {
        let Some(demand) = demands
            .iter()
            .find(|demand| &demand.notification_id == notification_id)
        else {
            continue;
        };
        if held.get(notification_id.as_str()) == Some(&Some(demand.fire_at)) {
            report.already_installed.push(notification_id.clone());
            continue;
        }
        let request = ScheduleRequest {
            notification_id: notification_id.clone(),
            fire_at: demand.fire_at,
            payload_kind: PayloadKind::Generic,
            opaque_target_id: notification_id.clone(),
        };
        match effects.schedule(&request) {
            Ok(()) => report.installed.push(notification_id.clone()),
            Err(error) => report.failures.push(PromptFailure {
                notification_id: notification_id.clone(),
                operation: PromptOperation::Schedule,
                class: error.class,
                code: error.code,
            }),
        }
    }
    let held_now: BTreeSet<&str> = report
        .installed
        .iter()
        .chain(&report.already_installed)
        .map(String::as_str)
        .collect();
    let cancelled: BTreeSet<&str> = report.cancelled.iter().map(String::as_str).collect();
    schedule.record_installed(&held_now, &cancelled);
    report.capacity = Some(capacity);
    Ok(report)
}

/// Cancel pending prompts outside the schedule's demands, then run the capacity pass with
/// them. Returns the demands offered and the capacity report.
fn capacity_pass(
    database: &mut Database,
    pass_clock: &PassClock,
    effects: &mut dyn NotificationEffects,
    schedule: &PromptSchedule,
    policy: &CapacityPolicy,
    pending: &[PendingRequest],
    report: &mut PromptPassReport,
) -> Result<(Vec<PromptDemand>, CapacityReport), ReconcileError> {
    let demands = schedule.demands(pass_clock.now);
    let offered: BTreeSet<&str> = demands
        .iter()
        .map(|demand| demand.notification_id.as_str())
        .collect();
    cancel_where(effects, pending, report, |notification_id| {
        !offered.contains(notification_id)
    });

    let capacity = reconcile_with_capacity(database, pass_clock, effects, policy, &demands)?;
    report
        .cancelled
        .extend(capacity.prompts_cancelled.iter().cloned());
    report.failures.extend(
        capacity
            .prompt_cancel_failures
            .iter()
            .map(|failure| PromptFailure {
                notification_id: failure.notification_id.clone(),
                operation: PromptOperation::Cancel,
                class: failure.class,
                code: failure.code.clone(),
            }),
    );
    Ok((demands, capacity))
}

fn cancel_where(
    effects: &mut dyn NotificationEffects,
    pending: &[PendingRequest],
    report: &mut PromptPassReport,
    should_cancel: impl Fn(&str) -> bool,
) {
    for request in pending {
        let notification_id = &request.notification_id;
        if !is_prompt_notification_id(notification_id) || !should_cancel(notification_id) {
            continue;
        }
        match effects.cancel(notification_id) {
            Ok(()) => report.cancelled.push(notification_id.clone()),
            Err(error) => report.failures.push(PromptFailure {
                notification_id: notification_id.clone(),
                operation: PromptOperation::Cancel,
                class: error.class,
                code: error.code,
            }),
        }
    }
}

/// One instant for the whole pass, so the demands and the capacity pass agree on what is past.
struct PassClock {
    now: DateTime<Utc>,
}

impl Clock for PassClock {
    fn now(&self) -> DateTime<Utc> {
        self.now
    }
}
