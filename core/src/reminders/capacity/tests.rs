use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::Transaction;

use super::*;
use crate::providers::contracts::ErrorClass;
use crate::reminders::reconcile::{
    apply_completion, EffectCompletion, EffectOutcome, NotificationEffectError,
    NotificationEffects, PendingRequest, ReconcileError, ScheduleRequest,
};
use crate::reminders::state::{
    apply_user_time_correction, get_reminder, list_operations, AcknowledgmentState, DeliveryState,
    OperationState, OperationType, ReminderRecord, ReminderStateError, RequestState, ScheduleState,
    UnschedulableReason, UserTimeCorrection,
};
use crate::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use crate::store::schema::{Clock, Database};

struct FixedClock {
    instant: DateTime<Utc>,
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

fn instant(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn clock_at(moment: &str) -> FixedClock {
    FixedClock {
        instant: instant(moment),
    }
}

const CAPTURE_TIME: &str = "2026-01-15T10:30:00Z";
const PASS_TIME: &str = "2026-01-15T10:31:00Z";

fn base_due() -> DateTime<Utc> {
    instant("2026-02-01T09:00:00Z")
}

fn open_db(label: &str) -> Result<Database> {
    let path = format!(
        "{}/test_capacity_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    );
    let clock: Arc<dyn Clock> = Arc::new(clock_at(CAPTURE_TIME));
    Database::open(&path, clock)
}

fn insert_item(db: &mut Database, item_id: &str) -> Result<()> {
    let tx = db.transaction()?;
    let capture_id = format!("cap-{item_id}");
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, 'synthetic reminder text', NULL, '2026-01-15T10:30:00Z', 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, '2026-01-15T10:30:00Z', NULL)",
        rusqlite::params![&capture_id],
    )?;
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', '2026-01-15T10:30:00Z', '2026-01-15T10:30:00Z')",
        rusqlite::params![item_id, &capture_id],
    )?;
    tx.commit()?;
    let event = Event::new(
        format!("evt-type-{item_id}"),
        item_id.to_string(),
        0,
        EventType::Correction,
        EventPayload::Correction(Correction {
            kind: CorrectionKind::Type,
            old_value: None,
            new_value: "action".to_string(),
        }),
        CAPTURE_TIME.to_string(),
    )?;
    save_event(db, &event, 0)?;
    Ok(())
}

fn complete_item(db: &mut Database, item_id: &str) -> Result<()> {
    let event = Event::new(
        format!("evt-complete-{item_id}"),
        item_id.to_string(),
        1,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T10:40:00Z".to_string(),
    )?;
    save_event(db, &event, 1)?;
    Ok(())
}

fn run<T>(
    db: &mut Database,
    command: impl FnOnce(&Transaction<'_>) -> Result<T, ReminderStateError>,
) -> Result<T> {
    let tx = db.immediate_transaction()?;
    let value = command(&tx)?;
    tx.commit()?;
    Ok(value)
}

fn reminder_version(tx: &Transaction<'_>, item_id: &str) -> i64 {
    get_reminder(tx, item_id)
        .unwrap()
        .map_or(0, |record| record.state_version)
}

fn set_reminder_time(
    db: &mut Database,
    item_id: &str,
    due: DateTime<Utc>,
) -> Result<ReminderRecord> {
    let clock = clock_at(CAPTURE_TIME);
    run(db, |tx| {
        apply_user_time_correction(
            tx,
            &clock,
            &UserTimeCorrection {
                command_id: format!("cmd-{}", uuid::Uuid::new_v4()),
                item_id: item_id.to_string(),
                expected_revision: 1,
                expected_reminder_version: reminder_version(tx, item_id),
                instant: due,
                timezone_id: "UTC".to_string(),
            },
        )
    })
}

/// `count` items with reminders due one hour apart, starting at `base_due()`. Item `n` is due
/// `n` hours after the base, so priority order equals numeric order.
fn seed_reminders(db: &mut Database, count: usize) -> Result<Vec<ReminderRecord>> {
    (0..count)
        .map(|index| {
            let item_id = format!("item-{index:03}");
            insert_item(db, &item_id)?;
            set_reminder_time(db, &item_id, base_due() + Duration::hours(index as i64))
        })
        .collect()
}

fn reminder(db: &mut Database, item_id: &str) -> Result<ReminderRecord> {
    Ok(run(db, |tx| get_reminder(tx, item_id))?.expect("reminder exists"))
}

fn capacity_exceeded() -> RequestState {
    RequestState::Unschedulable(UnschedulableReason::CapacityExceeded)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    List,
    Schedule(String),
    Cancel(String),
}

/// A stand-in for the OS pending set that behaves as observed on a device: more than 64
/// requests are accepted without error and the oldest-added are silently evicted.
#[derive(Default)]
struct FakeCenter {
    pending: Vec<(String, DateTime<Utc>)>,
    calls: Vec<Call>,
    evicted: Vec<String>,
    high_water: usize,
    fail_list: bool,
    fail_cancel_for: BTreeSet<String>,
}

impl FakeCenter {
    fn holds(&self, notification_id: &str) -> bool {
        self.pending.iter().any(|(id, _)| id == notification_id)
    }

    fn mutations(&self) -> Vec<&Call> {
        self.calls
            .iter()
            .filter(|call| !matches!(call, Call::List))
            .collect()
    }

    fn add_foreign(&mut self, notification_id: &str) {
        self.pending
            .push((notification_id.to_string(), base_due() + Duration::days(1)));
    }

    /// The OS delivers and drops requests whose due instant has passed.
    fn fire_due(&mut self, now: DateTime<Utc>) {
        self.pending.retain(|(_, due)| *due > now);
    }
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.calls
            .push(Call::Schedule(request.notification_id.clone()));
        self.pending
            .retain(|(id, _)| id != &request.notification_id);
        self.pending
            .push((request.notification_id.clone(), request.fire_at));
        while self.pending.len() > OS_PENDING_LIMIT {
            let (evicted, _) = self.pending.remove(0);
            self.evicted.push(evicted);
        }
        self.high_water = self.high_water.max(self.pending.len());
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.calls.push(Call::Cancel(notification_id.to_string()));
        if self.fail_cancel_for.contains(notification_id) {
            return Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "cancel_unavailable",
            ));
        }
        self.pending.retain(|(id, _)| id != notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        self.calls.push(Call::List);
        if self.fail_list {
            return Err(NotificationEffectError::new(
                ErrorClass::Transient,
                "list_unavailable",
            ));
        }
        self.high_water = self.high_water.max(self.pending.len());
        Ok(self
            .pending
            .iter()
            .map(|(id, due)| PendingRequest {
                notification_id: id.clone(),
                fire_at: Some(*due),
            })
            .collect())
    }
}

fn pass_at(
    db: &mut Database,
    center: &mut FakeCenter,
    prompts: &[PromptDemand],
    now: &str,
) -> Result<CapacityReport> {
    Ok(reconcile_with_capacity(
        db,
        &clock_at(now),
        center,
        &CapacityPolicy::default(),
        prompts,
    )?)
}

fn pass(db: &mut Database, center: &mut FakeCenter) -> Result<CapacityReport> {
    pass_at(db, center, &[], PASS_TIME)
}

fn budget() -> usize {
    CapacityPolicy::default().usable_budget()
}

// ---- Pure allocation -------------------------------------------------------------------

fn demands(count: usize) -> Vec<ReminderDemand> {
    (0..count)
        .map(|index| ReminderDemand {
            reminder_id: format!("reminder-{index:03}"),
            fire_at: base_due() + Duration::hours(index as i64),
            created_at: instant(CAPTURE_TIME),
        })
        .collect()
}

fn input<'a>(
    reminders: &'a [ReminderDemand],
    prompts: &'a [PromptDemand],
    foreign_pending: usize,
) -> AllocationInput<'a> {
    AllocationInput {
        now: instant(CAPTURE_TIME),
        foreign_pending,
        reminders,
        prompts,
    }
}

#[test]
fn the_planned_budget_is_always_below_the_os_limit() {
    assert_eq!(budget(), 60);
    assert!(budget() < OS_PENDING_LIMIT);
    let no_headroom = CapacityPolicy {
        replacement_headroom: 0,
        ..CapacityPolicy::default()
    };
    assert!(no_headroom.usable_budget() < OS_PENDING_LIMIT);
}

#[test]
fn below_at_and_above_the_budget_every_overflow_is_explicit() {
    for count in [budget() - 1, budget(), budget() + 1, budget() + 25] {
        let reminders = demands(count);
        let allocation = allocate(&CapacityPolicy::default(), &input(&reminders, &[], 0));

        assert_eq!(allocation.granted_reminders.len(), count.min(budget()));
        assert_eq!(
            allocation.overflow_reminders.len(),
            count.saturating_sub(budget())
        );
        let accounted: BTreeSet<&String> = allocation
            .granted_reminders
            .iter()
            .chain(&allocation.overflow_reminders)
            .chain(&allocation.elapsed_reminders)
            .collect();
        assert_eq!(accounted.len(), count, "every demand appears exactly once");
        assert_eq!(
            allocation.overflow_reminders,
            reminders[budget().min(count)..]
                .iter()
                .map(|demand| demand.reminder_id.clone())
                .collect::<Vec<_>>(),
            "the latest due reminders overflow, in priority order"
        );
    }
}

#[test]
fn priority_is_due_then_creation_then_identifier_whatever_the_input_order() {
    let early = instant("2026-01-15T10:00:00Z");
    let late = instant("2026-01-15T10:20:00Z");
    let due = base_due();
    let make = |reminder_id: &str, fire_at, created_at| ReminderDemand {
        reminder_id: reminder_id.to_string(),
        fire_at,
        created_at,
    };
    let forward = vec![
        make("c", due, late),
        make("b", due, early),
        make("a", due, early),
        make("z", due - Duration::hours(1), late),
    ];
    let mut backward = forward.clone();
    backward.reverse();
    let policy = CapacityPolicy {
        os_pending_limit: 5,
        replacement_headroom: 1,
        ..CapacityPolicy::default()
    };
    let granted_forward = allocate(&policy, &input(&forward, &[], 0));
    let granted_backward = allocate(&policy, &input(&backward, &[], 0));

    assert_eq!(granted_forward, granted_backward);
    assert_eq!(granted_forward.granted_reminders, vec!["z", "a", "b", "c"]);
}

#[test]
fn a_due_time_that_has_passed_needs_no_slot_and_is_not_overflow() {
    let mut reminders = demands(budget() + 1);
    reminders[0].fire_at = instant("2026-01-15T10:00:00Z");
    let allocation = allocate(&CapacityPolicy::default(), &input(&reminders, &[], 0));

    assert_eq!(allocation.elapsed_reminders, vec!["reminder-000"]);
    assert!(allocation.overflow_reminders.is_empty());
    assert_eq!(allocation.granted_reminders.len(), budget());
}

#[test]
fn requests_core_cannot_reclaim_reduce_the_budget() {
    let reminders = demands(budget());
    let allocation = allocate(&CapacityPolicy::default(), &input(&reminders, &[], 10));

    assert_eq!(allocation.reminder_budget, budget() - 10);
    assert_eq!(allocation.overflow_reminders.len(), 10);
    let saturated = allocate(&CapacityPolicy::default(), &input(&reminders, &[], 1000));
    assert_eq!(saturated.reminder_budget, 0);
    assert_eq!(saturated.overflow_reminders.len(), budget());
}

fn prompt(name: &str, hours_ahead: i64) -> PromptDemand {
    PromptDemand {
        notification_id: name.to_string(),
        fire_at: instant(CAPTURE_TIME) + Duration::hours(hours_ahead),
    }
}

#[test]
fn prompts_receive_only_what_explicit_reminders_leave() {
    let prompts: Vec<PromptDemand> = (1..=5)
        .map(|day| prompt(&format!("p{day}"), day * 24))
        .collect();
    let reminders = demands(budget() - 2);
    let allocation = allocate(&CapacityPolicy::default(), &input(&reminders, &prompts, 0));

    assert_eq!(allocation.granted_reminders.len(), budget() - 2);
    assert_eq!(allocation.granted_prompts, vec!["p1", "p2"]);
    assert_eq!(
        allocation.deferred_prompts,
        vec![
            DeferredPrompt {
                notification_id: "p3".into(),
                reason: PromptDeferral::CapacityExceeded
            },
            DeferredPrompt {
                notification_id: "p4".into(),
                reason: PromptDeferral::CapacityExceeded
            },
            DeferredPrompt {
                notification_id: "p5".into(),
                reason: PromptDeferral::CapacityExceeded
            },
        ]
    );

    let full = demands(budget() + 3);
    let squeezed = allocate(&CapacityPolicy::default(), &input(&full, &prompts, 0));
    assert!(squeezed.granted_prompts.is_empty());
    assert_eq!(squeezed.deferred_prompts.len(), prompts.len());
}

#[test]
fn prompts_are_bounded_by_horizon_limit_and_elapsed_time_without_catch_up() {
    let policy = CapacityPolicy::default();
    let mut prompts: Vec<PromptDemand> = (1..=9)
        .map(|day| prompt(&format!("p{day}"), day * 24 - 12))
        .collect();
    prompts.push(prompt("missed", -5));
    prompts.push(prompt("far", 24 * 30));
    prompts.push(prompt("p1", 12));
    let allocation = allocate(&policy, &input(&[], &prompts, 0));

    assert_eq!(allocation.granted_prompts.len(), MAX_PROMPT_REQUESTS);
    let reason_of = |name: &str| {
        allocation
            .deferred_prompts
            .iter()
            .find(|deferred| deferred.notification_id == name)
            .map(|deferred| deferred.reason)
    };
    assert_eq!(reason_of("missed"), Some(PromptDeferral::Elapsed));
    assert_eq!(reason_of("far"), Some(PromptDeferral::BeyondHorizon));
    assert_eq!(reason_of("p8"), Some(PromptDeferral::BeyondHorizon));
    assert!(allocation.granted_prompts.contains(&"p1".to_string()));
    assert_eq!(
        allocation
            .granted_prompts
            .iter()
            .filter(|name| name.as_str() == "p1")
            .count(),
        1,
        "a repeated stable identifier is one request"
    );
}

#[test]
fn more_prompts_than_the_limit_inside_the_horizon_are_deferred_for_the_limit() {
    let prompts: Vec<PromptDemand> = (1..=10)
        .map(|hour| prompt(&format!("p{hour:02}"), hour))
        .collect();
    let allocation = allocate(&CapacityPolicy::default(), &input(&[], &prompts, 0));

    assert_eq!(allocation.granted_prompts.len(), MAX_PROMPT_REQUESTS);
    assert_eq!(allocation.deferred_prompts.len(), 3);
    assert!(allocation
        .deferred_prompts
        .iter()
        .all(|deferred| deferred.reason == PromptDeferral::PromptLimit));
}

// ---- Durable reconciliation ------------------------------------------------------------

#[test]
fn below_the_budget_everything_is_installed_and_nothing_is_held_back() -> Result<()> {
    let mut db = open_db("below")?;
    let records = seed_reminders(&mut db, budget() - 1)?;
    let mut center = FakeCenter::default();

    let report = pass(&mut db, &mut center)?;

    assert!(report.converged());
    assert!(report.demoted.is_empty() && report.allocation.overflow_reminders.is_empty());
    assert_eq!(center.pending.len(), budget() - 1);
    assert!(center.evicted.is_empty());
    for record in &records {
        assert!(center.holds(&record.notification_id().unwrap()));
    }
    Ok(())
}

#[test]
fn at_the_budget_everything_is_installed_below_the_os_limit() -> Result<()> {
    let mut db = open_db("at")?;
    seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();

    let report = pass(&mut db, &mut center)?;

    assert!(report.demoted.is_empty());
    assert_eq!(center.pending.len(), budget());
    assert!(budget() < OS_PENDING_LIMIT);
    assert!(center.evicted.is_empty());
    Ok(())
}

#[test]
fn above_the_budget_overflow_is_unschedulable_not_dropped_and_not_installed() -> Result<()> {
    let mut db = open_db("above")?;
    let total = OS_PENDING_LIMIT + 10;
    let records = seed_reminders(&mut db, total)?;
    let mut center = FakeCenter::default();

    let report = pass(&mut db, &mut center)?;

    assert!(report.converged());
    assert!(center.evicted.is_empty(), "the OS never had to evict");
    assert!(center.high_water <= budget());
    assert_eq!(center.pending.len(), budget());
    assert_eq!(report.demoted.len(), total - budget());
    for (index, record) in records.iter().enumerate() {
        let current = reminder(&mut db, &format!("item-{index:03}"))?;
        let notification_id = record.notification_id().unwrap();
        if index < budget() {
            assert_eq!(current.request_state, RequestState::Resolved);
            assert_eq!(current.schedule_state, ScheduleState::Scheduled);
            assert!(center.holds(&notification_id));
        } else {
            assert_eq!(current.request_state, capacity_exceeded());
            assert_eq!(current.schedule_state, ScheduleState::NotScheduled);
            assert_eq!(current.resolved_instant, record.resolved_instant);
            assert_eq!(current.schedule_generation, record.schedule_generation);
            assert!(!center.holds(&notification_id), "never falsely installed");
            assert!(
                !center.calls.contains(&Call::Schedule(notification_id)),
                "no schedule call was made for overflow"
            );
        }
    }
    Ok(())
}

#[test]
fn a_repeated_pass_changes_nothing() -> Result<()> {
    let mut db = open_db("idempotent")?;
    seed_reminders(&mut db, budget() + 3)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let held_back = reminder(&mut db, &format!("item-{:03}", budget()))?;
    center.calls.clear();

    let report = pass(&mut db, &mut center)?;

    assert!(center.mutations().is_empty());
    assert!(report.demoted.is_empty() && report.refilled.is_empty() && report.expired.is_empty());
    assert_eq!(
        reminder(&mut db, &format!("item-{:03}", budget()))?,
        held_back,
        "no state-version churn"
    );
    Ok(())
}

#[test]
fn a_freed_slot_refills_the_highest_priority_held_back_reminder_with_its_stable_id() -> Result<()> {
    let mut db = open_db("refill")?;
    let records = seed_reminders(&mut db, budget() + 2)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let first_held = records[budget()].clone();
    let second_held = records[budget() + 1].clone();

    complete_item(&mut db, "item-000")?;
    let report = pass(&mut db, &mut center)?;

    assert!(report.converged());
    assert_eq!(report.refilled, vec![first_held.reminder_id.clone()]);
    let refilled = reminder(&mut db, "item-060")?;
    assert_eq!(refilled.request_state, RequestState::Resolved);
    assert_eq!(refilled.schedule_state, ScheduleState::Scheduled);
    assert_eq!(refilled.schedule_generation, first_held.schedule_generation);
    assert_eq!(refilled.notification_id(), first_held.notification_id());
    assert!(center.holds(&first_held.notification_id().unwrap()));
    assert_eq!(
        reminder(&mut db, "item-061")?.request_state,
        capacity_exceeded(),
        "the lower priority reminder keeps waiting"
    );
    assert!(!center.holds(&second_held.notification_id().unwrap()));
    assert!(center.evicted.is_empty());
    assert!(center.high_water <= budget());
    Ok(())
}

#[test]
fn a_new_sooner_reminder_displaces_the_farthest_and_refill_restores_its_identifier() -> Result<()> {
    let mut db = open_db("displace")?;
    let records = seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let farthest = records[budget() - 1].clone();
    insert_item(&mut db, "item-sooner")?;
    set_reminder_time(&mut db, "item-sooner", base_due() - Duration::hours(1))?;

    let report = pass(&mut db, &mut center)?;

    assert!(report.converged());
    assert_eq!(report.demoted, vec![farthest.reminder_id.clone()]);
    let displaced = reminder(&mut db, "item-059")?;
    assert_eq!(displaced.request_state, capacity_exceeded());
    assert!(!center.holds(&farthest.notification_id().unwrap()));
    assert!(center.holds(&reminder(&mut db, "item-sooner")?.notification_id().unwrap()));
    assert!(center.evicted.is_empty() && center.high_water <= budget());

    complete_item(&mut db, "item-sooner")?;
    let report = pass(&mut db, &mut center)?;

    assert_eq!(report.refilled, vec![farthest.reminder_id.clone()]);
    let restored = reminder(&mut db, "item-059")?;
    assert_eq!(restored.request_state, RequestState::Resolved);
    assert_eq!(restored.notification_id(), farthest.notification_id());
    assert!(center.holds(&farthest.notification_id().unwrap()));
    Ok(())
}

#[test]
fn a_time_correction_while_held_back_starts_a_new_generation_and_competes_again() -> Result<()> {
    let mut db = open_db("correction")?;
    let records = seed_reminders(&mut db, budget() + 1)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let held = records[budget()].clone();

    let corrected = set_reminder_time(&mut db, "item-060", base_due() - Duration::days(1))?;
    assert_eq!(corrected.schedule_generation, held.schedule_generation + 1);
    let report = pass(&mut db, &mut center)?;

    assert!(report.converged());
    assert_eq!(
        report.demoted,
        vec![records[budget() - 1].reminder_id.clone()]
    );
    assert!(center.holds(&corrected.notification_id().unwrap()));
    assert!(center.evicted.is_empty());
    Ok(())
}

#[test]
fn elapsed_time_frees_a_slot_without_becoming_delivery_evidence() -> Result<()> {
    let mut db = open_db("elapsed")?;
    let records = seed_reminders(&mut db, budget() + 1)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let held = records[budget()].clone();

    // After the first two reminders are due. The OS has delivered and dropped them.
    let later = "2026-02-01T10:30:00Z";
    center.fire_due(instant(later));
    let report = pass_at(&mut db, &mut center, &[], later)?;

    assert!(report.converged());
    assert_eq!(report.refilled, vec![held.reminder_id.clone()]);
    assert_eq!(report.allocation.elapsed_reminders.len(), 2);
    for item_id in ["item-000", "item-001"] {
        let passed = reminder(&mut db, item_id)?;
        assert_eq!(passed.request_state, RequestState::Resolved);
        assert_eq!(passed.delivery_state, DeliveryState::Unknown);
        assert_eq!(
            passed.acknowledgment_state,
            AcknowledgmentState::NotAcknowledged
        );
    }
    assert!(center.holds(&held.notification_id().unwrap()));
    Ok(())
}

#[test]
fn a_held_back_reminder_whose_time_passes_expires_and_is_never_installed() -> Result<()> {
    let mut db = open_db("expire")?;
    let records = seed_reminders(&mut db, budget() + 1)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let held = records[budget()].clone();

    let after_all = "2026-02-10T00:00:00Z";
    center.fire_due(instant(after_all));
    let report = pass_at(&mut db, &mut center, &[], after_all)?;

    assert_eq!(report.expired, vec![held.reminder_id.clone()]);
    assert!(report.refilled.is_empty());
    let expired = reminder(&mut db, "item-060")?;
    assert_eq!(
        expired.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(expired.resolved_instant, held.resolved_instant);
    assert_eq!(expired.delivery_state, DeliveryState::Unknown);
    assert!(!center
        .calls
        .contains(&Call::Schedule(held.notification_id().unwrap())));
    Ok(())
}

#[test]
fn a_failed_cancel_of_a_displaced_request_is_reported_and_retried() -> Result<()> {
    let mut db = open_db("cancel_retry")?;
    let records = seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;
    let farthest = records[budget() - 1].notification_id().unwrap();
    insert_item(&mut db, "item-sooner")?;
    set_reminder_time(&mut db, "item-sooner", base_due() - Duration::hours(1))?;
    center.fail_cancel_for.insert(farthest.clone());

    let report = pass(&mut db, &mut center)?;

    assert!(!report.converged());
    assert_eq!(report.demoted.len(), 1);
    assert!(center.holds(&farthest), "the request is still pending");

    center.fail_cancel_for.clear();
    let retry = pass(&mut db, &mut center)?;

    assert!(retry.converged());
    assert!(
        retry.demoted.is_empty(),
        "already demoted; no second change"
    );
    assert!(!center.holds(&farthest));
    assert_eq!(
        reminder(&mut db, "item-059")?.request_state,
        capacity_exceeded()
    );
    Ok(())
}

#[test]
fn a_failure_to_list_pending_requests_aborts_before_any_change() -> Result<()> {
    let mut db = open_db("list_failure")?;
    seed_reminders(&mut db, budget() + 1)?;
    let mut center = FakeCenter {
        fail_list: true,
        ..FakeCenter::default()
    };

    let result = reconcile_with_capacity(
        &mut db,
        &clock_at(PASS_TIME),
        &mut center,
        &CapacityPolicy::default(),
        &[],
    );

    assert!(matches!(result, Err(ReconcileError::ListPending(_))));
    assert!(center.mutations().is_empty());
    assert_eq!(
        reminder(&mut db, "item-060")?.request_state,
        RequestState::Resolved
    );
    Ok(())
}

#[test]
fn a_definitively_rejected_schedule_stays_rejected_across_demotion_and_refill() -> Result<()> {
    let mut db = open_db("rejected")?;
    let records = seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();
    let target = records[budget() - 1].clone();
    let notification_id = target.notification_id().unwrap();
    let tx = db.immediate_transaction()?;
    apply_completion(
        &tx,
        &clock_at(PASS_TIME),
        &EffectCompletion {
            reminder_id: target.reminder_id.clone(),
            effect_identity: format!("schedule:{notification_id}"),
            outcome: EffectOutcome::Failed(ErrorClass::Permanent),
        },
    )?;
    tx.commit()?;

    insert_item(&mut db, "item-sooner")?;
    set_reminder_time(&mut db, "item-sooner", base_due() - Duration::hours(1))?;
    let demoting = pass(&mut db, &mut center)?;
    assert_eq!(demoting.demoted, vec![target.reminder_id.clone()]);

    complete_item(&mut db, "item-sooner")?;
    let refilling = pass(&mut db, &mut center)?;

    assert_eq!(refilling.refilled, vec![target.reminder_id.clone()]);
    let restored = reminder(&mut db, "item-059")?;
    assert_eq!(restored.request_state, RequestState::Resolved);
    assert_eq!(restored.schedule_state, ScheduleState::ScheduleFailed);
    assert_eq!(refilling.reconcile.withheld, vec![notification_id.clone()]);
    assert!(!center.holds(&notification_id));
    let operations = run(&mut db, |tx| list_operations(tx, &target.reminder_id))?;
    assert!(operations.iter().any(|operation| {
        operation.operation_type == OperationType::Schedule
            && operation.operation_state == OperationState::Rejected
    }));
    Ok(())
}

#[test]
fn completed_items_free_their_slots() -> Result<()> {
    let mut db = open_db("inactive")?;
    seed_reminders(&mut db, budget() + 1)?;
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center)?;

    complete_item(&mut db, "item-010")?;
    let report = pass(&mut db, &mut center)?;

    assert_eq!(report.refilled.len(), 1);
    assert_eq!(center.pending.len(), budget());
    assert_eq!(
        reminder(&mut db, "item-010")?.request_state,
        RequestState::Cancelled
    );
    Ok(())
}

#[test]
fn foreign_requests_the_os_holds_reduce_the_reminder_budget() -> Result<()> {
    let mut db = open_db("foreign")?;
    seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();
    for index in 0..5 {
        center.add_foreign(&format!("other-feature-{index}"));
    }

    let report = pass(&mut db, &mut center)?;

    assert_eq!(report.allocation.reminder_budget, budget() - 5);
    assert_eq!(report.demoted.len(), 5);
    assert_eq!(center.pending.len(), budget());
    assert!(center.evicted.is_empty());
    Ok(())
}

#[test]
fn prompts_yield_their_slots_to_reminders_before_anything_is_installed() -> Result<()> {
    let mut db = open_db("prompts")?;
    seed_reminders(&mut db, budget() - 3)?;
    let mut center = FakeCenter::default();
    let prompts: Vec<PromptDemand> = (1..=5)
        .map(|day| PromptDemand {
            notification_id: format!("prompt-{day}"),
            fire_at: instant(PASS_TIME) + Duration::days(day),
        })
        .collect();

    let first = pass_at(&mut db, &mut center, &prompts, PASS_TIME)?;
    assert_eq!(first.allocation.granted_prompts.len(), 3);
    assert_eq!(first.prompts_cancelled.len(), 0);
    // The prompt scheduler installs what was granted.
    for demand in prompts.iter().take(3) {
        center
            .pending
            .push((demand.notification_id.clone(), demand.fire_at));
    }
    assert_eq!(center.pending.len(), budget());

    // Three more explicit reminders arrive: prompts must give way before they are installed.
    for index in 0..3 {
        let item_id = format!("item-new-{index}");
        insert_item(&mut db, &item_id)?;
        set_reminder_time(&mut db, &item_id, base_due() - Duration::hours(index + 1))?;
    }
    center.calls.clear();
    let second = pass_at(&mut db, &mut center, &prompts, PASS_TIME)?;

    assert!(second.converged());
    assert!(second.allocation.granted_prompts.is_empty());
    assert_eq!(second.prompts_cancelled.len(), 3);
    assert!(center.evicted.is_empty());
    assert!(center.high_water <= budget());
    assert_eq!(center.pending.len(), budget());
    assert!(center
        .pending
        .iter()
        .all(|(id, _)| !id.starts_with("prompt-")));
    let first_schedule = center
        .calls
        .iter()
        .position(|call| matches!(call, Call::Schedule(_)))
        .expect("reminders were installed");
    let last_prompt_cancel = center
        .calls
        .iter()
        .rposition(|call| matches!(call, Call::Cancel(id) if id.starts_with("prompt-")))
        .expect("prompts were cancelled");
    assert!(last_prompt_cancel < first_schedule);
    Ok(())
}

#[test]
fn a_failed_prompt_cancel_is_reported_and_retried() -> Result<()> {
    let mut db = open_db("prompt_cancel")?;
    seed_reminders(&mut db, budget())?;
    let mut center = FakeCenter::default();
    center.add_foreign("prompt-1");
    center.fail_cancel_for.insert("prompt-1".to_string());
    let prompts = [PromptDemand {
        notification_id: "prompt-1".to_string(),
        fire_at: instant(PASS_TIME) + Duration::days(1),
    }];

    let report = pass_at(&mut db, &mut center, &prompts, PASS_TIME)?;

    assert!(!report.converged());
    assert_eq!(report.prompt_cancel_failures.len(), 1);
    assert!(center.holds("prompt-1"));

    center.fail_cancel_for.clear();
    let retry = pass_at(&mut db, &mut center, &prompts, PASS_TIME)?;
    assert!(retry.converged());
    assert!(!center.holds("prompt-1"));
    Ok(())
}
