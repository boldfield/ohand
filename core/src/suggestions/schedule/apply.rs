use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::plan::{is_prompt_notification_id, prompt_local_date, PromptSchedule};
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
///
/// The schedule's ledger is settled first with the clock and the OS listing: a held request
/// the listing no longer shows is spent, and the requests this pass cancels after seeing them
/// pending are released, not spent. With prompting off every held date was already spent when
/// it was switched off, so this pass changes nothing in the ledger.
pub fn cancel_prompts(
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    schedule: &mut PromptSchedule,
) -> Result<PromptPassReport, ReconcileError> {
    let now = clock.now();
    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let pending_ids: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.notification_id.as_str())
        .collect();
    schedule.observe_pending(now, &pending_ids);
    let mut report = PromptPassReport::default();
    cancel_where(effects, &pending, &mut report, |_| true);
    schedule.release(report.cancelled.iter().map(String::as_str));
    Ok(report)
}

/// Bring the OS in line with the schedule and report what changed.
///
/// Off: every pending prompt is cancelled and no capacity pass runs, so reminders are left
/// exactly as they were.
///
/// On: the ledger is settled first with this pass's clock reading and OS listing, before
/// anything is cancelled. A date whose request core had handed to the OS and the listing no
/// longer shows is spent: it fired, even if the clock has since been moved back, and it is
/// never installed again. Then pending prompts outside the plan (an earlier time, another
/// zone, a passed or spent day) are cancelled so they stop consuming budget. The capacity
/// pass plans the offered prompts after explicit reminders and cancels any it defers. Every
/// request cancelled this way was seen pending in this same listing, so its date is released
/// rather than spent and comes back when a slot frees. Last, the granted prompts that the OS
/// does not already hold at the planned instant are installed with the generic payload and
/// their own opaque identifier as target.
///
/// Each install attempt is recorded as held *before* the native call. A call that fails after
/// the OS accepted the request (a lost acknowledgment, which surfaces as `Transient` or
/// `Cancelled`) therefore still leaves the evidence that spends the date once the clock reaches
/// its instant or a listing no longer shows it, so it is never installed again after a
/// rollback. The same evidence means an ambiguous failure that in fact installed nothing costs
/// that date its prompt: the next listing does not show the request, the date is spent, and the
/// horizon moves on at the next open. That is the suppression side of the ledger's ambiguity
/// rule, not a defect. A definitive rejection (`Permanent`, `Unauthorized`, `Unsupported`, the
/// classes the reminder side treats as having installed nothing) releases the attempt, or
/// restores the evidence of a request the OS already held under that identifier, and the
/// next pass retries it. Every failure is reported either way. This function never extends
/// the horizon: only the schedule's own refresh rules do.
///
/// The ledger lives in `schedule`, a caller-held value: persist it after every pass. See the
/// module documentation for the crash window this leaves.
pub fn reconcile_prompts(
    database: &mut Database,
    clock: &dyn Clock,
    effects: &mut dyn NotificationEffects,
    schedule: &mut PromptSchedule,
    policy: &CapacityPolicy,
) -> Result<PromptPassReport, ReconcileError> {
    if !schedule.is_enabled() {
        return cancel_prompts(clock, effects, schedule);
    }
    let pass_clock = PassClock { now: clock.now() };
    let pending = effects
        .list_pending()
        .map_err(ReconcileError::ListPending)?;
    let pending_ids: BTreeSet<&str> = pending
        .iter()
        .map(|request| request.notification_id.as_str())
        .collect();
    schedule.observe_pending(pass_clock.now, &pending_ids);

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
    schedule.release(report.cancelled.iter().map(String::as_str));
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
        let Some(local_date) = prompt_local_date(notification_id) else {
            continue;
        };
        if held.get(notification_id.as_str()) == Some(&Some(demand.fire_at)) {
            report.already_installed.push(notification_id.clone());
            schedule.record_held(local_date, demand.fire_at);
            continue;
        }
        // The attempt marker: held before the call, so a lost acknowledgment leaves evidence.
        let previously_pending = held.get(notification_id.as_str()).copied();
        schedule.record_held(local_date, demand.fire_at);
        let request = ScheduleRequest {
            notification_id: notification_id.clone(),
            fire_at: demand.fire_at,
            payload_kind: PayloadKind::Generic,
            opaque_target_id: notification_id.clone(),
        };
        match effects.schedule(&request) {
            Ok(()) => report.installed.push(notification_id.clone()),
            Err(error) => {
                if installs_nothing(error.class) {
                    match previously_pending {
                        // The OS still holds the request this call meant to replace; its
                        // evidence stands at the instant the listing reported.
                        Some(Some(instant)) => schedule.record_held(local_date, instant),
                        // Held at an unreported instant: keep the attempted one, which the
                        // next listing or the clock settles.
                        Some(None) => {}
                        None => schedule.release([notification_id.as_str()]),
                    }
                }
                report.failures.push(PromptFailure {
                    notification_id: notification_id.clone(),
                    operation: PromptOperation::Schedule,
                    class: error.class,
                    code: error.code,
                });
            }
        }
    }
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

/// Whether a failed `schedule` call with this class is a definitive rejection that installed
/// nothing, following the reminder side's failure disposition (`reminders::reconcile`).
/// `Transient` and `Cancelled` prove nothing: the OS may have accepted the request before the
/// acknowledgment was lost, so an attempt that fails with them stays held.
fn installs_nothing(class: ErrorClass) -> bool {
    matches!(
        class,
        ErrorClass::Permanent | ErrorClass::Unauthorized | ErrorClass::Unsupported
    )
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
