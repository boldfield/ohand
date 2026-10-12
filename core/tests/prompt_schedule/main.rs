// S02: the bounded optional daily prompt schedule.

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::Transaction;
use std::collections::BTreeMap;
use std::sync::Arc;

use ohand_core::providers::contracts::ErrorClass;
use ohand_core::reminders::capacity::{
    reconcile_with_capacity, CapacityPolicy, MAX_PROMPT_REQUESTS,
};
use ohand_core::reminders::reconcile::{
    reconcile, NotificationEffectError, NotificationEffects, PayloadKind, PendingRequest,
    ScheduleRequest,
};
use ohand_core::reminders::state::{
    apply_derived_request, cancel_reminder, get_reminder, DerivedReminderRequest,
    ReminderCancellation, ReminderStateError, ScheduleState,
};
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use ohand_core::store::schema::{Clock, Database};
use ohand_core::suggestions::schedule::{
    cancel_prompts, is_prompt_notification_id, local_fire_instant, reconcile_prompts,
    PromptFailure, PromptOperation, PromptSchedule, PromptScheduleError, PromptSettings,
    PromptTime, RefreshTrigger, PROMPT_HORIZON_DAYS, PROMPT_ID_PREFIX,
};

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

fn open_db(label: &str) -> Result<Database> {
    let path = format!(
        "{}/test_prompt_schedule_{}_{}.db",
        std::env::temp_dir().display(),
        label,
        uuid::Uuid::new_v4()
    );
    let clock: Arc<dyn Clock> = Arc::new(clock_at("2026-01-15T10:30:00Z"));
    Database::open(&path, clock)
}

fn nine_am() -> PromptTime {
    PromptTime::new(9, 0).unwrap()
}

fn daily(time: PromptTime) -> PromptSettings {
    PromptSettings::Daily { time }
}

const UTC_ZONE: Tz = Tz::UTC;

fn insert_item(db: &mut Database, item_id: &str, text: &str) -> Result<()> {
    let tx = db.transaction()?;
    let capture_id = format!("cap-{item_id}");
    tx.execute(
        "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at, session_topic)
         VALUES (?, ?, NULL, '2026-01-15T10:30:00Z', 'UTC', 0, 'en', 'gregorian', 'personal', 'route-1', 0, '2026-01-15T10:30:00Z', NULL)",
        rusqlite::params![&capture_id, text],
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
        "2026-01-15T10:30:00Z".to_string(),
    )?;
    save_event(db, &event, 0)?;
    Ok(())
}

fn run<T>(
    db: &mut Database,
    command: impl FnOnce(&Transaction<'_>) -> Result<T, ReminderStateError>,
) -> Result<T, ReminderStateError> {
    let tx = db.immediate_transaction()?;
    let value = command(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// A reminder due on 2026-01-16 at 09:00:00Z for `item_id`.
fn create_reminder(db: &mut Database, item_id: &str, text: &str) -> Result<String> {
    insert_item(db, item_id, &format!("{text} 2026-01-16 09:00:00"))?;
    let clock = clock_at("2026-01-15T10:30:00Z");
    let record = run(db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &DerivedReminderRequest {
                item_id: item_id.to_string(),
                source_revision: 1,
                phrase: "2026-01-16 09:00:00".to_string(),
            },
        )
    })?;
    Ok(record.notification_id().expect("resolved reminder"))
}

/// The user cancels the reminder on `item_id`, freeing its OS slot.
fn cancel_reminder_on(db: &mut Database, item_id: &str) -> Result<()> {
    let clock = clock_at("2026-01-15T10:30:00Z");
    let cancelled = run(db, |tx| {
        let expected_revision: i32 = tx.query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [item_id],
            |row| row.get(0),
        )?;
        let reminder = get_reminder(tx, item_id)?.expect("reminder exists");
        cancel_reminder(
            tx,
            &clock,
            &ReminderCancellation {
                command_id: format!("cancel-{item_id}"),
                item_id: item_id.to_string(),
                expected_revision,
                expected_reminder_version: reminder.state_version,
            },
        )
    })?;
    assert!(cancelled.is_some());
    Ok(())
}

#[derive(Default)]
struct FakeCenter {
    pending: BTreeMap<String, DateTime<Utc>>,
    requests: Vec<ScheduleRequest>,
    schedule_calls: usize,
    cancel_calls: Vec<String>,
    fail_schedule: Option<NotificationEffectError>,
    fail_cancel_for: Vec<String>,
}

impl FakeCenter {
    fn prompt_ids(&self) -> Vec<String> {
        self.pending
            .keys()
            .filter(|id| is_prompt_notification_id(id))
            .cloned()
            .collect()
    }

    fn mutations(&self) -> usize {
        self.schedule_calls + self.cancel_calls.len()
    }

    fn reset_calls(&mut self) {
        self.schedule_calls = 0;
        self.cancel_calls.clear();
        self.requests.clear();
    }

    /// The OS delivers and drops requests whose due instant has passed.
    fn fire_due(&mut self, now: DateTime<Utc>) {
        self.pending.retain(|_, due| *due > now);
    }
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.schedule_calls += 1;
        if let Some(error) = &self.fail_schedule {
            return Err(error.clone());
        }
        self.requests.push(request.clone());
        self.pending
            .insert(request.notification_id.clone(), request.fire_at);
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.cancel_calls.push(notification_id.to_string());
        if self.fail_cancel_for.iter().any(|id| id == notification_id) {
            return Err(NotificationEffectError::new(ErrorClass::Transient, "busy"));
        }
        self.pending.remove(notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
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

fn pass(
    db: &mut Database,
    center: &mut FakeCenter,
    schedule: &mut PromptSchedule,
    now: &str,
) -> Result<ohand_core::suggestions::schedule::PromptPassReport> {
    Ok(reconcile_prompts(
        db,
        &clock_at(now),
        center,
        schedule,
        &CapacityPolicy::default(),
    )?)
}

fn pass_with(
    db: &mut Database,
    center: &mut FakeCenter,
    schedule: &mut PromptSchedule,
    policy: &CapacityPolicy,
    now: &str,
) -> Result<ohand_core::suggestions::schedule::PromptPassReport> {
    Ok(reconcile_prompts(
        db,
        &clock_at(now),
        center,
        schedule,
        policy,
    )?)
}

fn date(text: &str) -> NaiveDate {
    text.parse().unwrap()
}

fn fire_instants(schedule: &PromptSchedule) -> Vec<DateTime<Utc>> {
    schedule.planned().iter().map(|p| p.fire_at).collect()
}

// ---- Off by default ----------------------------------------------------------------------

#[test]
fn prompting_is_off_by_default_and_plans_and_installs_nothing() -> Result<()> {
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");

    assert!(!schedule.is_enabled());
    assert_eq!(schedule.settings(), PromptSettings::Off);
    assert!(schedule.planned().is_empty());
    assert!(schedule.demands(now).is_empty());

    let mut db = open_db("off_default")?;
    let mut center = FakeCenter::default();
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    assert_eq!(center.mutations(), 0);
    assert!(report.capacity.is_none() && report.converged());
    Ok(())
}

#[test]
fn a_stored_default_is_off_and_an_invalid_time_cannot_be_enabled() {
    let stored = serde_json::to_string(&PromptSchedule::default()).unwrap();
    let restored: PromptSchedule = serde_json::from_str(&stored).unwrap();
    assert!(!restored.is_enabled());

    assert_eq!(
        PromptTime::new(24, 0),
        Err(PromptScheduleError::InvalidTime)
    );
    assert_eq!(
        PromptTime::new(9, 60),
        Err(PromptScheduleError::InvalidTime)
    );
    let invalid = r#"{"settings":{"mode":"daily","time":{"hour":25,"minute":0}},"planned":[]}"#;
    assert!(serde_json::from_str::<PromptSchedule>(invalid).is_err());
}

#[test]
fn a_configured_schedule_survives_a_store_and_reload() {
    let now = instant("2026-01-15T10:30:00Z");
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), now, UTC_ZONE);

    let restored: PromptSchedule =
        serde_json::from_str(&serde_json::to_string(&schedule).unwrap()).unwrap();

    assert_eq!(restored, schedule);
    assert_eq!(restored.demands(now), schedule.demands(now));
}

// ---- Bounded horizon and capacity ---------------------------------------------------------

#[test]
fn choosing_a_time_plans_only_the_next_few_future_occurrences() -> Result<()> {
    let now = instant("2026-01-15T10:30:00Z");
    let mut schedule = PromptSchedule::default();

    schedule.configure(daily(nine_am()), now, UTC_ZONE);

    // 09:00 today has passed, so the horizon starts tomorrow.
    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-16T09:00:00Z"),
            instant("2026-01-17T09:00:00Z"),
            instant("2026-01-18T09:00:00Z"),
        ]
    );
    assert_eq!(schedule.planned().len(), PROMPT_HORIZON_DAYS);
    const { assert!(PROMPT_HORIZON_DAYS < MAX_PROMPT_REQUESTS) };
    let ids: Vec<&str> = schedule
        .planned()
        .iter()
        .map(|p| p.notification_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "prompt:2026-01-16",
            "prompt:2026-01-17",
            "prompt:2026-01-18"
        ]
    );

    let mut db = open_db("horizon")?;
    let mut center = FakeCenter::default();
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert_eq!(report.installed.len(), PROMPT_HORIZON_DAYS);
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert!(report.converged());
    Ok(())
}

#[test]
fn a_time_later_today_is_included_in_the_horizon() {
    let now = instant("2026-01-15T10:30:00Z");
    let mut schedule = PromptSchedule::default();

    schedule.configure(daily(PromptTime::new(20, 15).unwrap()), now, UTC_ZONE);

    assert_eq!(
        schedule.planned()[0].fire_at,
        instant("2026-01-15T20:15:00Z")
    );
    assert_eq!(schedule.planned().len(), PROMPT_HORIZON_DAYS);
}

#[test]
fn explicit_reminders_keep_their_slots_and_prompts_take_only_what_is_left() -> Result<()> {
    let mut db = open_db("capacity")?;
    // Four usable slots: three reminders leave exactly one for prompts.
    let policy = CapacityPolicy {
        os_pending_limit: 8,
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    };
    let mut reminder_ids = Vec::new();
    for index in 0..3 {
        reminder_ids.push(create_reminder(
            &mut db,
            &format!("item-{index}"),
            "water plants",
        )?);
    }
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();

    let report = reconcile_prompts(
        &mut db,
        &clock_at("2026-01-15T10:30:00Z"),
        &mut center,
        &mut schedule,
        &policy,
    )?;

    for reminder_id in &reminder_ids {
        assert!(
            center.pending.contains_key(reminder_id),
            "reminder installed"
        );
    }
    assert_eq!(center.prompt_ids(), vec!["prompt:2026-01-16".to_string()]);
    assert_eq!(center.pending.len(), 4);
    let capacity = report.capacity.expect("capacity pass ran");
    assert_eq!(capacity.allocation.granted_prompts, ["prompt:2026-01-16"]);
    assert_eq!(capacity.allocation.deferred_prompts.len(), 2);
    assert!(capacity.demoted.is_empty());
    Ok(())
}

#[test]
fn a_new_reminder_displaces_a_prompt_before_it_displaces_any_reminder() -> Result<()> {
    let mut db = open_db("displace")?;
    let policy = CapacityPolicy {
        os_pending_limit: 8,
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    };
    let first = create_reminder(&mut db, "item-0", "water plants")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    let now = clock_at("2026-01-15T10:30:00Z");
    reconcile_prompts(&mut db, &now, &mut center, &mut schedule, &policy)?;
    assert_eq!(center.prompt_ids().len(), 3);

    let second = create_reminder(&mut db, "item-1", "call the dentist")?;
    reconcile_prompts(&mut db, &now, &mut center, &mut schedule, &policy)?;

    assert!(center.pending.contains_key(&first) && center.pending.contains_key(&second));
    assert_eq!(center.prompt_ids().len(), 2);
    assert_eq!(center.pending.len(), 4);
    Ok(())
}

// ---- Disabling cancels only prompts ------------------------------------------------------

#[test]
fn disabling_cancels_every_prompt_and_leaves_reminders_and_other_requests_alone() -> Result<()> {
    let mut db = open_db("disable")?;
    let reminder_id = create_reminder(&mut db, "item-1", "call the roofer")?;
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    center.pending.insert(
        "other-feature-request".to_string(),
        instant("2026-01-20T09:00:00Z"),
    );
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    let before = get_reminder_row(&mut db, "item-1")?;
    center.reset_calls();

    schedule.configure(PromptSettings::Off, now, UTC_ZONE);
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    assert!(center.prompt_ids().is_empty());
    assert_eq!(report.cancelled.len(), PROMPT_HORIZON_DAYS);
    assert!(center
        .cancel_calls
        .iter()
        .all(|id| id.starts_with(PROMPT_ID_PREFIX)));
    assert_eq!(center.schedule_calls, 0);
    assert!(center.pending.contains_key(&reminder_id));
    assert!(center.pending.contains_key("other-feature-request"));
    assert!(
        report.capacity.is_none(),
        "no reminder pass runs when prompting is off"
    );
    assert_eq!(get_reminder_row(&mut db, "item-1")?, before);
    assert!(schedule.planned().is_empty() && schedule.demands(now).is_empty());
    Ok(())
}

fn get_reminder_row(db: &mut Database, item_id: &str) -> Result<(ScheduleState, i64)> {
    let record = run(db, |tx| get_reminder(tx, item_id))?.expect("reminder");
    Ok((record.schedule_state, record.state_version))
}

#[test]
fn cancel_prompts_removes_leftovers_even_without_a_stored_plan() -> Result<()> {
    let mut center = FakeCenter::default();
    center.pending.insert(
        "prompt:2026-01-16".to_string(),
        instant("2026-01-16T09:00:00Z"),
    );
    center.pending.insert(
        "some-reminder#1".to_string(),
        instant("2026-01-16T09:00:00Z"),
    );

    let report = cancel_prompts(&mut center)?;

    assert_eq!(report.cancelled, ["prompt:2026-01-16"]);
    assert_eq!(
        center.pending.keys().collect::<Vec<_>>(),
        ["some-reminder#1"]
    );
    Ok(())
}

#[test]
fn a_failed_prompt_cancel_is_reported_and_retried_by_the_next_pass() -> Result<()> {
    let mut db = open_db("cancel_retry")?;
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    schedule.configure(PromptSettings::Off, now, UTC_ZONE);
    center.fail_cancel_for = vec!["prompt:2026-01-17".to_string()];

    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    assert!(!report.converged());
    assert_eq!(
        report.failures,
        vec![PromptFailure {
            notification_id: "prompt:2026-01-17".to_string(),
            operation: PromptOperation::Cancel,
            class: ErrorClass::Transient,
            code: "busy".to_string(),
        }]
    );
    center.fail_cancel_for.clear();
    let retry = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:31:00Z")?;
    assert!(retry.converged());
    assert!(center.prompt_ids().is_empty());
    Ok(())
}

// ---- Refill only on app open --------------------------------------------------------------

#[test]
fn only_an_app_open_refills_the_horizon() -> Result<()> {
    let mut db = open_db("refill")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    // Two prompts fire. Returning to the app and a clock change add nothing.
    let later = instant("2026-01-17T12:00:00Z");
    center.fire_due(later);
    schedule.refresh(RefreshTrigger::Reentry, later, UTC_ZONE);
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, later, UTC_ZONE);
    assert_eq!(
        fire_instants(&schedule),
        vec![instant("2026-01-18T09:00:00Z")]
    );
    pass(&mut db, &mut center, &mut schedule, "2026-01-17T12:00:00Z")?;
    assert_eq!(center.prompt_ids(), vec!["prompt:2026-01-18".to_string()]);

    schedule.refresh(RefreshTrigger::AppOpen, later, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-17T12:00:00Z")?;

    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-18",
            "prompt:2026-01-19",
            "prompt:2026-01-20"
        ]
    );
    Ok(())
}

#[test]
fn rolling_the_clock_back_does_not_replay_a_prompt_that_already_fired() -> Result<()> {
    let mut db = open_db("rollback")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    assert!(center.pending.contains_key("prompt:2026-01-15"));

    // The OS delivers the 09:00 prompt; the clock is then moved back to 08:00.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, rolled_back, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;

    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    let installs_of_fired = center
        .requests
        .iter()
        .filter(|request| request.notification_id == "prompt:2026-01-15")
        .count();
    assert_eq!(installs_of_fired, 1);
    Ok(())
}

#[test]
fn a_rollback_then_an_app_open_or_time_change_still_skips_the_fired_occurrence() -> Result<()> {
    let mut db = open_db("rollback-open")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:00:00Z")?;

    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.refresh(RefreshTrigger::AppOpen, rolled_back, UTC_ZONE);
    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-16T09:00:00Z"),
            instant("2026-01-17T09:00:00Z"),
            instant("2026-01-18T09:00:00Z"),
        ]
    );

    // Choosing the same time again in the rolled-back clock does not bring today's back, and
    // neither does a later time today: the 15th has had its prompt.
    schedule.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-16T09:00:00Z"));
    let two_pm = PromptTime::new(14, 0).unwrap();
    schedule.configure(daily(two_pm), rolled_back, UTC_ZONE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-16T14:00:00Z"));

    let restored: PromptSchedule = serde_json::from_str(&serde_json::to_string(&schedule)?)?;
    assert_eq!(restored, schedule);
    Ok(())
}

#[test]
fn a_prompt_deferred_by_capacity_is_not_mistaken_for_one_that_fired() -> Result<()> {
    let mut db = open_db("deferred-not-fired")?;
    let policy = CapacityPolicy {
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    };
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    let clock = clock_at("2026-01-15T10:30:00Z");
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    create_reminder(&mut db, "item-0", "water plants")?;
    create_reminder(&mut db, "item-1", "call the dentist")?;
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    let planned_before = fire_instants(&schedule);

    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    assert_eq!(fire_instants(&schedule), planned_before);
    assert_eq!(planned_before.len(), PROMPT_HORIZON_DAYS);
    Ok(())
}

#[test]
fn an_app_open_that_is_the_first_look_after_a_rollback_does_not_replay_the_fired_prompt(
) -> Result<()> {
    let mut db = open_db("rollback-first-open")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    assert!(center.pending.contains_key("prompt:2026-01-15"));

    center.fire_due(instant("2026-01-15T10:00:00Z"));
    schedule.refresh(
        RefreshTrigger::AppOpen,
        instant("2026-01-15T08:00:00Z"),
        UTC_ZONE,
    );
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;

    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    let installs_of_fired = center
        .requests
        .iter()
        .filter(|request| request.notification_id == "prompt:2026-01-15")
        .count();
    assert_eq!(installs_of_fired, 1);
    Ok(())
}

fn installs_of(center: &FakeCenter, notification_id: &str) -> usize {
    center
        .requests
        .iter()
        .filter(|request| request.notification_id == notification_id)
        .count()
}

#[test]
fn an_app_open_after_a_multi_day_rollback_does_not_replay_the_fired_prompt() -> Result<()> {
    let mut db = open_db("multi-day-rollback-open")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-15",
            "prompt:2026-01-16",
            "prompt:2026-01-17"
        ]
    );

    // The 15th fires; the clock is then moved back almost two days and the app is opened
    // before any pass has seen the delivery. The refill plans the 14th, ahead of the 15th.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = "2026-01-13T20:00:00Z";
    schedule.refresh(RefreshTrigger::AppOpen, instant(rolled_back), UTC_ZONE);
    assert_eq!(schedule.planned()[0].local_date, date("2026-01-14"));
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;

    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert!(!center.pending.contains_key("prompt:2026-01-15"));
    // Nothing on or before the delivered date comes back: not the 14th, not the 15th.
    assert!(!center.pending.contains_key("prompt:2026-01-14"));
    assert!(center.pending.contains_key("prompt:2026-01-16"));
    assert!(schedule
        .planned()
        .iter()
        .all(|prompt| prompt.local_date > date("2026-01-15")));

    // Later looks at the rolled-back clock, including another app open, still skip it.
    schedule.refresh(RefreshTrigger::AppOpen, instant(rolled_back), UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-16",
            "prompt:2026-01-17",
            "prompt:2026-01-18"
        ]
    );
    Ok(())
}

#[test]
fn a_time_change_after_a_multi_day_rollback_does_not_replay_the_fired_prompt() -> Result<()> {
    let mut db = open_db("multi-day-rollback-clock")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;

    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = "2026-01-13T20:00:00Z";
    schedule.refresh(
        RefreshTrigger::ClockOrZoneChange,
        instant(rolled_back),
        UTC_ZONE,
    );
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    // The fence never passes a prompt the OS still holds.
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );

    // The next app open, still on the rolled-back clock, refills after the fired date only.
    schedule.refresh(RefreshTrigger::AppOpen, instant(rolled_back), UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(installs_of(&center, "prompt:2026-01-14"), 0);
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-16",
            "prompt:2026-01-17",
            "prompt:2026-01-18"
        ]
    );
    Ok(())
}

#[test]
fn a_later_time_on_a_day_whose_prompt_already_fired_adds_no_second_prompt() -> Result<()> {
    let mut db = open_db("one-per-day")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;

    // The 09:00 prompt fires; at 10:00 the user moves the prompt to 11:00.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let now = instant("2026-01-15T10:00:00Z");
    schedule.configure(daily(PromptTime::new(11, 0).unwrap()), now, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:00:00Z")?;

    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-16T11:00:00Z"),
            instant("2026-01-17T11:00:00Z"),
            instant("2026-01-18T11:00:00Z"),
        ]
    );
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    Ok(())
}

#[test]
fn a_time_change_before_the_first_look_after_a_rollback_does_not_replay_the_fired_prompt(
) -> Result<()> {
    let mut db = open_db("rollback-then-time-change")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    assert!(center.pending.contains_key("prompt:2026-01-15"));

    // The 09:00 prompt fires; the clock is moved back to 08:00 and, before any pass looks at
    // the OS, the user moves the prompt to 11:00. The 15th is re-planned at 11:00 under the same
    // identifier, so the evidence that the OS held it must survive the change.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = "2026-01-15T08:00:00Z";
    let eleven_am = PromptTime::new(11, 0).unwrap();
    schedule.configure(daily(eleven_am), instant(rolled_back), UTC_ZONE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-15T11:00:00Z"));
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;

    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert!(!center.pending.contains_key("prompt:2026-01-15"));
    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-16T11:00:00Z"),
            instant("2026-01-17T11:00:00Z"),
        ]
    );
    // The still-held days moved to the new time under their own identifiers.
    assert_eq!(
        center.pending["prompt:2026-01-16"],
        instant("2026-01-16T11:00:00Z")
    );
    assert_eq!(
        center.pending["prompt:2026-01-17"],
        instant("2026-01-17T11:00:00Z")
    );

    // Later looks in the rolled-back clock, including an app open and choosing the original
    // time again, still never bring the 15th back.
    schedule.refresh(RefreshTrigger::AppOpen, instant(rolled_back), UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    schedule.configure(daily(nine_am()), instant(rolled_back), UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-16",
            "prompt:2026-01-17",
            "prompt:2026-01-18"
        ]
    );
    Ok(())
}

#[test]
fn an_earlier_time_chosen_in_a_rolled_back_clock_does_not_replay_the_fired_prompt() -> Result<()> {
    let mut db = open_db("rollback-then-earlier-time")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;

    // The 09:00 prompt fires; the clock is moved back to 08:00 and the user picks 08:30, still
    // ahead of the rolled-back clock but before the instant that actually fired.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = "2026-01-15T08:00:00Z";
    let half_past_eight = PromptTime::new(8, 30).unwrap();
    schedule.configure(daily(half_past_eight), instant(rolled_back), UTC_ZONE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-15T08:30:00Z"));
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;

    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    assert_eq!(
        center.pending["prompt:2026-01-16"],
        instant("2026-01-16T08:30:00Z")
    );
    assert!(schedule
        .planned()
        .iter()
        .all(|prompt| prompt.local_date > date("2026-01-15")));

    // A pass that merely runs again at the same instant changes nothing.
    center.reset_calls();
    pass(&mut db, &mut center, &mut schedule, rolled_back)?;
    assert_eq!(center.mutations(), 0);
    Ok(())
}

#[test]
fn a_prompt_cancelled_by_the_reminder_capacity_pass_never_costs_an_earlier_pending_prompt(
) -> Result<()> {
    let mut db = open_db("reminder-side-capacity")?;
    let policy = CapacityPolicy {
        os_pending_limit: 8,
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    };
    let mut schedule = PromptSchedule::default();
    let moment = "2026-01-15T10:30:00Z";
    let now = instant(moment);
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    let clock = clock_at(moment);
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-16",
            "prompt:2026-01-17",
            "prompt:2026-01-18"
        ]
    );

    // The reminder side makes room itself, cancels the latest prompt and reports it.
    create_reminder(&mut db, "item-0", "water plants")?;
    create_reminder(&mut db, "item-1", "call the dentist")?;
    let displacement = reconcile_with_capacity(
        &mut db,
        &clock,
        &mut center,
        &policy,
        &schedule.demands(now),
    )?;
    schedule.record_capacity_pass(&displacement);
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );

    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;

    assert_eq!(
        fire_instants(&schedule)[..2],
        [
            instant("2026-01-16T09:00:00Z"),
            instant("2026-01-17T09:00:00Z")
        ]
    );
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    assert!(!center
        .cancel_calls
        .iter()
        .any(|id| id == "prompt:2026-01-16" || id == "prompt:2026-01-17"));
    Ok(())
}

fn tight_policy() -> CapacityPolicy {
    CapacityPolicy {
        os_pending_limit: 8,
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    }
}

const NEXT_THREE: [&str; 3] = [
    "prompt:2026-01-16",
    "prompt:2026-01-17",
    "prompt:2026-01-18",
];

#[test]
fn prompts_displaced_entirely_by_the_reminder_pass_return_once_capacity_frees() -> Result<()> {
    let mut db = open_db("full-displacement")?;
    let policy = tight_policy();
    let mut schedule = PromptSchedule::default();
    let moment = "2026-01-15T10:30:00Z";
    let now = instant(moment);
    let clock = clock_at(moment);
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    assert_eq!(center.prompt_ids(), NEXT_THREE);

    // Four reminders fill the whole usable budget; the reminder side cancels every prompt.
    for index in 0..4 {
        create_reminder(&mut db, &format!("item-{index}"), "water plants")?;
    }
    let displacement = reconcile_with_capacity(
        &mut db,
        &clock,
        &mut center,
        &policy,
        &schedule.demands(now),
    )?;
    assert_eq!(displacement.prompts_cancelled, NEXT_THREE);
    schedule.record_capacity_pass(&displacement);
    assert!(center.prompt_ids().is_empty());

    // The prompt pass sees all three absent while reminders hold every slot: not deliveries.
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;
    assert_eq!(
        fire_instants(&schedule),
        [
            instant("2026-01-16T09:00:00Z"),
            instant("2026-01-17T09:00:00Z"),
            instant("2026-01-18T09:00:00Z"),
        ]
    );
    assert!(center.prompt_ids().is_empty());

    for index in 0..4 {
        cancel_reminder_on(&mut db, &format!("item-{index}"))?;
    }
    schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;

    assert_eq!(
        fire_instants(&schedule),
        [
            instant("2026-01-16T09:00:00Z"),
            instant("2026-01-17T09:00:00Z"),
            instant("2026-01-18T09:00:00Z"),
        ]
    );
    assert_eq!(center.prompt_ids(), NEXT_THREE);
    Ok(())
}

#[test]
fn a_reported_reminder_pass_keeps_displaced_prompts_even_if_capacity_frees_before_the_next_look(
) -> Result<()> {
    let mut db = open_db("reported-displacement")?;
    let policy = tight_policy();
    let mut schedule = PromptSchedule::default();
    let moment = "2026-01-15T10:30:00Z";
    let now = instant(moment);
    let clock = clock_at(moment);
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;

    for index in 0..4 {
        create_reminder(&mut db, &format!("item-{index}"), "water plants")?;
    }
    let displacement = reconcile_with_capacity(
        &mut db,
        &clock,
        &mut center,
        &policy,
        &schedule.demands(now),
    )?;
    schedule.record_capacity_pass(&displacement);
    assert!(center.prompt_ids().is_empty());

    // The reminders go away before the prompt pass ever sees the empty OS.
    for index in 0..4 {
        cancel_reminder_on(&mut db, &format!("item-{index}"))?;
    }
    schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
    reconcile_prompts(&mut db, &clock, &mut center, &mut schedule, &policy)?;

    assert_eq!(center.prompt_ids(), NEXT_THREE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-16T09:00:00Z"));
    Ok(())
}

#[test]
fn a_rollback_under_tight_capacity_gives_the_fired_prompt_slot_to_the_next_in_one_pass(
) -> Result<()> {
    let mut db = open_db("rollback-tight")?;
    let policy = tight_policy();
    // Two reminders leave two prompt slots.
    create_reminder(&mut db, "item-0", "water plants")?;
    create_reminder(&mut db, "item-1", "call the dentist")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T07:00:00Z",
    )?;
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-15", "prompt:2026-01-16"]
    );

    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, rolled_back, UTC_ZONE);
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;

    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    center.reset_calls();
    let repeat = pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;
    assert_eq!(center.mutations(), 0);
    assert!(repeat.installed.is_empty() && repeat.cancelled.is_empty());
    let installs_of_fired = center
        .requests
        .iter()
        .filter(|request| request.notification_id == "prompt:2026-01-15")
        .count();
    assert_eq!(installs_of_fired, 0);
    Ok(())
}

#[test]
fn a_fired_prompt_withheld_for_capacity_after_a_rollback_is_not_replayed_when_capacity_frees(
) -> Result<()> {
    let mut db = open_db("rollback-withheld")?;
    let policy = tight_policy();
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T07:00:00Z",
    )?;
    assert!(center.pending.contains_key("prompt:2026-01-15"));

    center.fire_due(instant("2026-01-15T10:00:00Z"));
    for index in 0..4 {
        create_reminder(&mut db, &format!("item-{index}"), "water plants")?;
    }
    // Rolled back while reminders hold every slot: every prompt is deferred.
    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, rolled_back, UTC_ZONE);
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;
    assert!(center.prompt_ids().is_empty());

    for index in 0..4 {
        cancel_reminder_on(&mut db, &format!("item-{index}"))?;
    }
    schedule.refresh(RefreshTrigger::AppOpen, rolled_back, UTC_ZONE);
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;

    assert!(!center.pending.contains_key("prompt:2026-01-15"));
    assert_eq!(center.prompt_ids(), NEXT_THREE);
    assert_eq!(fire_instants(&schedule)[0], instant("2026-01-16T09:00:00Z"));
    let installs_of_fired = center
        .requests
        .iter()
        .filter(|request| request.notification_id == "prompt:2026-01-15")
        .count();
    assert_eq!(installs_of_fired, 1);
    Ok(())
}

#[test]
fn a_reported_reminder_pass_after_a_fire_then_a_rollback_does_not_replay_the_fired_prompt(
) -> Result<()> {
    let mut db = open_db("rollback-reported")?;
    let policy = tight_policy();
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T07:00:00Z",
    )?;
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-15",
            "prompt:2026-01-16",
            "prompt:2026-01-17"
        ]
    );

    // 09:00 fires; at 10:00 the reminder side fills capacity and reports what it cancelled.
    let after_fire = instant("2026-01-15T10:00:00Z");
    center.fire_due(after_fire);
    for index in 0..4 {
        create_reminder(&mut db, &format!("item-{index}"), "water plants")?;
    }
    let displacement = reconcile_with_capacity(
        &mut db,
        &clock_at("2026-01-15T10:00:00Z"),
        &mut center,
        &policy,
        &schedule.demands(after_fire),
    )?;
    schedule.record_capacity_pass(&displacement);
    assert_eq!(
        displacement.prompts_cancelled,
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );

    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, rolled_back, UTC_ZONE);
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;
    for index in 0..4 {
        cancel_reminder_on(&mut db, &format!("item-{index}"))?;
    }
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;

    // The cancelled prompts come back; the fired one does not.
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    schedule.refresh(RefreshTrigger::AppOpen, rolled_back, UTC_ZONE);
    pass_with(
        &mut db,
        &mut center,
        &mut schedule,
        &policy,
        "2026-01-15T08:00:00Z",
    )?;
    assert_eq!(center.prompt_ids(), NEXT_THREE);
    let installs_of_fired = center
        .requests
        .iter()
        .filter(|request| request.notification_id == "prompt:2026-01-15")
        .count();
    assert_eq!(installs_of_fired, 1);
    Ok(())
}

#[test]
fn reentry_and_clock_changes_do_not_fill_an_empty_plan() {
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let after_nonuse = instant("2026-02-15T10:30:00Z");

    schedule.refresh(RefreshTrigger::Reentry, after_nonuse, UTC_ZONE);
    assert!(schedule.planned().is_empty());
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, after_nonuse, UTC_ZONE);
    assert!(schedule.planned().is_empty());
    assert!(schedule.demands(after_nonuse).is_empty());
}

#[test]
fn multi_day_nonuse_leaves_no_backlog_and_the_next_open_starts_in_the_future() -> Result<()> {
    let mut db = open_db("nonuse")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    // Ten days pass without opening the app.
    let returned = instant("2026-01-25T14:00:00Z");
    center.fire_due(returned);
    assert!(
        center.prompt_ids().is_empty(),
        "nothing is left queued after the horizon"
    );
    center.reset_calls();
    pass(&mut db, &mut center, &mut schedule, "2026-01-25T14:00:00Z")?;
    assert_eq!(
        center.mutations(),
        0,
        "a pass without an open schedules nothing"
    );

    schedule.refresh(RefreshTrigger::AppOpen, returned, UTC_ZONE);
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-25T14:00:00Z")?;

    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-26T09:00:00Z"),
            instant("2026-01-27T09:00:00Z"),
            instant("2026-01-28T09:00:00Z"),
        ]
    );
    assert_eq!(report.installed.len(), PROMPT_HORIZON_DAYS);
    assert!(center.pending.values().all(|due| *due > returned));
    Ok(())
}

#[test]
fn changing_the_time_does_not_replay_a_passed_time_and_replaces_stale_requests() -> Result<()> {
    let mut db = open_db("time_change")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
    assert_eq!(
        center.pending.get("prompt:2026-01-15"),
        Some(&instant("2026-01-15T09:00:00Z"))
    );

    // At 10:30 the user moves the prompt to 08:00. Today's 08:00 and 09:00 have both passed.
    let now = instant("2026-01-15T10:30:00Z");
    center.fire_due(now);
    schedule.configure(daily(PromptTime::new(8, 0).unwrap()), now, UTC_ZONE);
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-01-16T08:00:00Z"),
            instant("2026-01-17T08:00:00Z"),
            instant("2026-01-18T08:00:00Z"),
        ]
    );
    // The old 09:00 requests for the 16th and 17th were replaced under the same identifiers.
    assert_eq!(
        center.pending.get("prompt:2026-01-16"),
        Some(&instant("2026-01-16T08:00:00Z"))
    );
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert_eq!(report.installed.len(), PROMPT_HORIZON_DAYS);
    Ok(())
}

#[test]
fn a_zone_change_at_the_next_open_cancels_requests_outside_the_new_plan() -> Result<()> {
    let mut db = open_db("zone")?;
    let new_york: Tz = "America/New_York".parse().unwrap();
    let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T15:00:00Z");
    schedule.configure(daily(nine_am()), now, new_york);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T15:00:00Z")?;
    assert_eq!(
        center.pending.get("prompt:2026-01-16"),
        Some(&instant("2026-01-16T14:00:00Z"))
    );

    // The clock-or-zone event itself neither refills nor replays.
    let landed = instant("2026-01-15T23:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, landed, tokyo);
    assert_eq!(schedule.planned().len(), PROMPT_HORIZON_DAYS);
    assert_eq!(
        schedule.planned()[0].fire_at,
        instant("2026-01-16T14:00:00Z")
    );

    // The next open plans for the local morning in the new zone.
    schedule.refresh(RefreshTrigger::AppOpen, landed, tokyo);
    let report = pass(&mut db, &mut center, &mut schedule, "2026-01-15T23:00:00Z")?;

    assert_eq!(
        schedule.planned()[0].fire_at,
        instant("2026-01-16T00:00:00Z")
    );
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert_eq!(
        center.pending.get("prompt:2026-01-16"),
        Some(&instant("2026-01-16T00:00:00Z"))
    );
    assert!(report.converged());
    Ok(())
}

// ---- Local time ---------------------------------------------------------------------------

#[test]
fn a_skipped_local_time_fires_after_the_gap_and_a_repeated_one_fires_first() {
    let new_york: Tz = "America/New_York".parse().unwrap();
    let time = PromptTime::new(2, 30).unwrap();

    // 2026-03-08 02:30 does not exist in New York; clocks jump from 02:00 to 03:00 (07:00Z).
    assert_eq!(
        local_fire_instant(date("2026-03-08"), time, new_york),
        Some(instant("2026-03-08T07:00:00Z"))
    );
    // 2026-11-01 01:30 happens twice; the first is 05:30Z (EDT).
    assert_eq!(
        local_fire_instant(
            date("2026-11-01"),
            PromptTime::new(1, 30).unwrap(),
            new_york
        ),
        Some(instant("2026-11-01T05:30:00Z"))
    );
    assert_eq!(
        local_fire_instant(date("2026-03-09"), time, new_york),
        Some(instant("2026-03-09T06:30:00Z"))
    );
}

#[test]
fn the_local_time_is_kept_across_a_dst_change_inside_the_horizon() {
    let new_york: Tz = "America/New_York".parse().unwrap();
    let mut schedule = PromptSchedule::default();

    schedule.configure(daily(nine_am()), instant("2026-03-07T20:00:00Z"), new_york);

    assert_eq!(
        fire_instants(&schedule),
        vec![
            instant("2026-03-08T13:00:00Z"),
            instant("2026-03-09T13:00:00Z"),
            instant("2026-03-10T13:00:00Z"),
        ]
    );
}

// ---- Idempotency and failure --------------------------------------------------------------

#[test]
fn repeating_a_pass_changes_nothing_and_a_failed_install_is_retried() -> Result<()> {
    let mut db = open_db("idempotent")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter {
        fail_schedule: Some(NotificationEffectError::new(ErrorClass::Transient, "busy")),
        ..FakeCenter::default()
    };

    let failed = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert!(!failed.converged());
    assert_eq!(failed.failures.len(), PROMPT_HORIZON_DAYS);
    assert!(failed
        .failures
        .iter()
        .all(|f| f.operation == PromptOperation::Schedule));
    assert!(center.prompt_ids().is_empty());

    center.fail_schedule = None;
    let retried = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:31:00Z")?;
    assert!(retried.converged());
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);

    center.reset_calls();
    let repeat = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:32:00Z")?;
    assert_eq!(center.mutations(), 0);
    assert_eq!(repeat.already_installed.len(), PROMPT_HORIZON_DAYS);
    assert!(repeat.installed.is_empty() && repeat.cancelled.is_empty());
    Ok(())
}

#[test]
fn a_prompt_pass_leaves_explicit_reminders_installed_for_the_reminder_reconciler() -> Result<()> {
    let mut db = open_db("reminder_rest")?;
    let reminder_id = create_reminder(&mut db, "item-1", "call the roofer")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();

    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    let reminder_report = reconcile(&mut db, &clock_at("2026-01-15T10:30:00Z"), &mut center)?;

    assert!(center.pending.contains_key(&reminder_id));
    assert!(
        reminder_report.removed.is_empty(),
        "reminder reconcile leaves prompts alone"
    );
    assert_eq!(center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert_eq!(
        get_reminder_row(&mut db, "item-1")?.0,
        ScheduleState::Scheduled
    );
    Ok(())
}

// ---- Payload privacy ----------------------------------------------------------------------

#[test]
fn prompt_requests_are_generic_and_carry_no_item_content() -> Result<()> {
    let mut db = open_db("payload")?;
    let secret = "SECRET-diagnosis-appointment";
    create_reminder(&mut db, "item-private", secret)?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();

    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    let prompt_requests: Vec<&ScheduleRequest> = center
        .requests
        .iter()
        .filter(|request| is_prompt_notification_id(&request.notification_id))
        .collect();
    assert_eq!(prompt_requests.len(), PROMPT_HORIZON_DAYS);
    for request in prompt_requests {
        assert_eq!(request.payload_kind, PayloadKind::Generic);
        assert_eq!(request.opaque_target_id, request.notification_id);
        assert!(is_prompt_notification_id(&request.notification_id));
        let rendered = format!("{request:?}");
        assert!(!rendered.contains(secret) && !rendered.contains("item-private"));
        assert!(request.notification_id.len() <= 32);
    }
    let serialized = serde_json::to_string(&schedule)?;
    assert!(!serialized.contains(secret) && !serialized.contains("item-private"));
    Ok(())
}

#[test]
fn prompt_identifiers_cannot_alias_a_reminder_identifier() {
    assert!(is_prompt_notification_id("prompt:2026-01-16"));
    assert!(!is_prompt_notification_id("prompt:2026-01-16#1"));
    assert!(!is_prompt_notification_id("3f2a0c3e#1"));
}
