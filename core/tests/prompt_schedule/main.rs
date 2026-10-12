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
    cancel_prompts, is_prompt_notification_id, local_fire_instant, prompt_local_date,
    prompt_notification_id, reconcile_prompts, PromptFailure, PromptOperation, PromptSchedule,
    PromptScheduleError, PromptSettings, PromptTime, RefreshTrigger, PROMPT_HORIZON_DAYS,
    PROMPT_ID_PREFIX, SPENT_DATE_RETENTION_DAYS,
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
    create_reminder_due(db, item_id, text, "2026-01-16 09:00:00")
}

/// A reminder for `item_id` due at the explicit `phrase` (`YYYY-MM-DD HH:MM:SS`, UTC).
fn create_reminder_due(
    db: &mut Database,
    item_id: &str,
    text: &str,
    phrase: &str,
) -> Result<String> {
    insert_item(db, item_id, &format!("{text} {phrase}"))?;
    let clock = clock_at("2026-01-15T10:30:00Z");
    let record = run(db, |tx| {
        apply_derived_request(
            tx,
            &clock,
            &DerivedReminderRequest {
                item_id: item_id.to_string(),
                source_revision: 1,
                phrase: phrase.to_string(),
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
    /// `schedule` installs nothing and returns this error (a definitive rejection, or a
    /// transient failure before the OS accepted anything).
    fail_schedule: Option<NotificationEffectError>,
    /// `schedule` installs the request and then returns this error: the OS accepted it, but
    /// the acknowledgment was lost (a timeout, a cancelled caller).
    fail_after_install: Option<NotificationEffectError>,
    fail_cancel_for: Vec<String>,
    /// Identifiers the OS has delivered (and dropped) so far, in delivery order.
    delivered: Vec<String>,
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
        let due_now: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due_now {
            self.pending.remove(&id);
            self.delivered.push(id);
        }
    }

    /// The OS drops a request without delivering it (a purge, a permission toggle).
    fn vanish(&mut self, notification_id: &str) {
        self.pending.remove(notification_id);
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
        if let Some(error) = &self.fail_after_install {
            return Err(error.clone());
        }
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

    let report = cancel_prompts(
        &clock_at("2026-01-15T10:30:00Z"),
        &mut center,
        &mut PromptSchedule::default(),
    )?;

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
    // before any pass has seen the delivery. The refill starts after the high-water mark (the
    // 07:00 pass), so the 14th, a day the user has already lived through, is never planned;
    // the 15th still is, because nothing has yet shown that its request fired.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = "2026-01-13T20:00:00Z";
    schedule.refresh(RefreshTrigger::AppOpen, instant(rolled_back), UTC_ZONE);
    assert_eq!(schedule.planned()[0].local_date, date("2026-01-15"));
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
    schedule.record_capacity_pass(&displacement, now);
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
    schedule.record_capacity_pass(&displacement, now);
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
    schedule.record_capacity_pass(&displacement, now);
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
    schedule.record_capacity_pass(&displacement, after_fire);
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
fn repeating_a_pass_changes_nothing_and_a_definitively_rejected_install_is_retried() -> Result<()> {
    let mut db = open_db("idempotent")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter {
        fail_schedule: Some(NotificationEffectError::new(
            ErrorClass::Permanent,
            "rejected",
        )),
        ..FakeCenter::default()
    };

    // A definitive rejection installed nothing: the attempt is released, so the dates are not
    // spent and the next pass tries again.
    let failed = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert!(!failed.converged());
    assert_eq!(failed.failures.len(), PROMPT_HORIZON_DAYS);
    assert!(failed
        .failures
        .iter()
        .all(|f| f.operation == PromptOperation::Schedule));
    assert!(center.prompt_ids().is_empty());
    for day in NEXT_THREE {
        assert!(!schedule.is_spent(prompt_local_date(day).unwrap()), "{day}");
    }

    center.fail_schedule = None;
    let retried = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:31:00Z")?;
    assert!(retried.converged());
    assert_eq!(center.prompt_ids(), NEXT_THREE);

    center.reset_calls();
    let repeat = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:32:00Z")?;
    assert_eq!(center.mutations(), 0);
    assert_eq!(repeat.already_installed.len(), PROMPT_HORIZON_DAYS);
    assert!(repeat.installed.is_empty() && repeat.cancelled.is_empty());
    Ok(())
}

#[test]
fn a_lost_acknowledgment_on_install_does_not_replay_the_prompt_after_a_rollback() -> Result<()> {
    for class in [ErrorClass::Transient, ErrorClass::Cancelled] {
        let mut db = open_db(&format!("lost-ack-{class:?}"))?;
        let mut schedule = PromptSchedule::default();
        schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
        let mut center = FakeCenter {
            fail_after_install: Some(NotificationEffectError::new(class, "timed out")),
            ..FakeCenter::default()
        };

        // The OS accepts every request, but each call reports an ambiguous failure.
        let ambiguous = pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;
        assert!(!ambiguous.converged());
        assert_eq!(ambiguous.failures.len(), PROMPT_HORIZON_DAYS);
        assert!(ambiguous.installed.is_empty());
        assert_eq!(
            center.prompt_ids(),
            [
                "prompt:2026-01-15",
                "prompt:2026-01-16",
                "prompt:2026-01-17"
            ]
        );

        // The 09:00 prompt fires with no pass in between; the clock is then moved back.
        center.fire_due(instant("2026-01-15T10:00:00Z"));
        center.fail_after_install = None;
        let settled = pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;

        assert!(settled.converged(), "{class:?}");
        assert!(schedule.is_spent(date("2026-01-15")), "{class:?}");
        assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1, "{class:?}");
        assert_eq!(
            center.prompt_ids(),
            ["prompt:2026-01-16", "prompt:2026-01-17"],
            "{class:?}"
        );
        // The requests the OS did accept are confirmed by the listing, not installed again.
        assert_eq!(settled.already_installed.len(), 2, "{class:?}");
        assert_eq!(installs_of(&center, "prompt:2026-01-16"), 1, "{class:?}");
    }
    Ok(())
}

#[test]
fn a_lost_acknowledgment_is_confirmed_by_the_next_listing_in_the_ordinary_path() -> Result<()> {
    let mut db = open_db("lost-ack-ordinary")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T07:00:00Z"), UTC_ZONE);
    let mut center = FakeCenter {
        fail_after_install: Some(NotificationEffectError::new(
            ErrorClass::Transient,
            "timed out",
        )),
        ..FakeCenter::default()
    };
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:00:00Z")?;

    // No rollback, no fire: the next pass sees the requests held and spends nothing, so every
    // chosen day still gets its one prompt.
    center.fail_after_install = None;
    center.reset_calls();
    let confirmed = pass(&mut db, &mut center, &mut schedule, "2026-01-15T07:05:00Z")?;
    assert!(confirmed.converged());
    assert_eq!(center.mutations(), 0);
    assert_eq!(confirmed.already_installed.len(), PROMPT_HORIZON_DAYS);
    for day in ["2026-01-15", "2026-01-16", "2026-01-17"] {
        assert!(!schedule.is_spent(date(day)), "{day}");
    }
    center.fire_due(instant("2026-01-15T09:00:00Z"));
    assert_eq!(center.delivered, ["prompt:2026-01-15"]);
    Ok(())
}

#[test]
fn an_ambiguous_failure_that_installed_nothing_skips_the_date_rather_than_risk_a_replay(
) -> Result<()> {
    let mut db = open_db("ambiguous-nothing")?;
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter {
        fail_schedule: Some(NotificationEffectError::new(ErrorClass::Transient, "busy")),
        ..FakeCenter::default()
    };

    // Core cannot tell this failure from a lost acknowledgment, so the attempts stay held.
    let failed = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert_eq!(failed.failures.len(), PROMPT_HORIZON_DAYS);
    assert!(center.prompt_ids().is_empty());

    // The next listing does not show them: the dates are spent, not retried. The suppression
    // side of the ambiguity rule, and the horizon moves on at the next open.
    center.fail_schedule = None;
    let settled = pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:31:00Z")?;
    assert!(settled.converged());
    assert!(settled.installed.is_empty());
    assert!(center.prompt_ids().is_empty());
    for day in NEXT_THREE {
        assert!(schedule.is_spent(prompt_local_date(day).unwrap()), "{day}");
    }
    schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:31:00Z")?;
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-19",
            "prompt:2026-01-20",
            "prompt:2026-01-21"
        ]
    );
    for day in NEXT_THREE {
        assert_eq!(installs_of(&center, day), 0, "{day}");
    }
    Ok(())
}

#[test]
fn a_definitive_rejection_of_a_replacement_keeps_the_evidence_of_the_request_the_os_holds(
) -> Result<()> {
    let (mut db, mut center, mut schedule) = configure_and_install_from_seven("reject-replace")?;

    // The user moves the time to 11:00. The OS rejects every replacement outright and keeps
    // holding the 09:00 requests.
    schedule.configure(
        daily(PromptTime::new(11, 0).unwrap()),
        instant("2026-01-15T08:00:00Z"),
        UTC_ZONE,
    );
    center.fail_schedule = Some(NotificationEffectError::new(
        ErrorClass::Permanent,
        "rejected",
    ));
    let rejected = pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;
    assert!(!rejected.converged());
    assert!(rejected
        .failures
        .iter()
        .all(|f| f.operation == PromptOperation::Schedule));
    let still_held = center.pending.get("prompt:2026-01-15").copied();
    assert_eq!(still_held, Some(instant("2026-01-15T09:00:00Z")));

    // The 09:00 request the OS kept fires; the clock is moved back; the OS recovers.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    center.fail_schedule = None;
    let settled = pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:30:00Z")?;
    assert!(settled.converged());
    assert!(schedule.is_spent(date("2026-01-15")));
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    assert_eq!(
        center.pending["prompt:2026-01-16"],
        instant("2026-01-16T11:00:00Z")
    );
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

// ---- Fired evidence is a ledger, independent of the plan ----------------------------------

fn configure_and_install_from_seven(label: &str) -> Result<(Database, FakeCenter, PromptSchedule)> {
    let mut db = open_db(label)?;
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
    Ok((db, center, schedule))
}

#[test]
fn turning_prompting_off_and_on_after_a_rollback_does_not_replay_the_fired_prompt() -> Result<()> {
    let (mut db, mut center, mut schedule) = configure_and_install_from_seven("off-on-rollback")?;

    // The 15th fires; the clock is moved back; the user switches prompting off and on again
    // before any pass has looked at the OS. Nothing has shown the three requests still
    // pending when the plan was emptied, so all three dates are spent.
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.configure(PromptSettings::Off, rolled_back, UTC_ZONE);
    assert!(schedule.planned().is_empty());
    schedule.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    assert!(schedule
        .planned()
        .iter()
        .all(|prompt| prompt.local_date > date("2026-01-17")));
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;

    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert!(!center.pending.contains_key("prompt:2026-01-15"));
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
fn a_pass_while_off_then_re_enabling_after_a_rollback_does_not_replay_the_fired_prompt(
) -> Result<()> {
    let (mut db, mut center, mut schedule) = configure_and_install_from_seven("off-pass-on")?;

    center.fire_due(instant("2026-01-15T10:00:00Z"));
    let rolled_back = instant("2026-01-15T08:00:00Z");
    schedule.configure(PromptSettings::Off, rolled_back, UTC_ZONE);
    let off_pass = pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;
    assert_eq!(
        off_pass.cancelled,
        ["prompt:2026-01-16", "prompt:2026-01-17"]
    );
    assert!(center.prompt_ids().is_empty());

    schedule.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T08:00:00Z")?;

    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-18",
            "prompt:2026-01-19",
            "prompt:2026-01-20"
        ]
    );
    // Toggling again in the rolled-back clock spends nothing further: the OS held nothing.
    schedule.configure(PromptSettings::Off, rolled_back, UTC_ZONE);
    schedule.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    assert!(!schedule.is_spent(date("2026-01-21")));
    Ok(())
}

#[test]
fn disabling_in_the_ordinary_path_spends_the_held_dates_and_re_enabling_moves_on() -> Result<()> {
    let mut db = open_db("disable-spends")?;
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    schedule.configure(PromptSettings::Off, now, UTC_ZONE);
    for day in ["2026-01-16", "2026-01-17", "2026-01-18"] {
        assert!(schedule.is_spent(date(day)), "{day} spent on disable");
    }
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-19",
            "prompt:2026-01-20",
            "prompt:2026-01-21"
        ]
    );
    Ok(())
}

#[test]
fn the_ledger_survives_a_store_and_reload() -> Result<()> {
    let (mut db, mut center, mut schedule) = configure_and_install_from_seven("ledger-reload")?;
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    schedule.refresh(
        RefreshTrigger::Reentry,
        instant("2026-01-15T10:00:00Z"),
        UTC_ZONE,
    );
    assert!(schedule.is_spent(date("2026-01-15")));
    assert_eq!(schedule.high_water(), Some(instant("2026-01-15T10:00:00Z")));

    let stored = serde_json::to_string(&schedule)?;
    let mut restored: PromptSchedule = serde_json::from_str(&stored)?;
    assert_eq!(restored, schedule);

    // The restored ledger still fences the fired date in a rolled-back clock, and still knows
    // which requests the OS was handed: the 16th vanishing is a delivery, not a free slot.
    let rolled_back = instant("2026-01-15T08:00:00Z");
    restored.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    assert_eq!(fire_instants(&restored)[0], instant("2026-01-16T09:00:00Z"));
    center.vanish("prompt:2026-01-16");
    pass(&mut db, &mut center, &mut restored, "2026-01-15T08:00:00Z")?;
    assert_eq!(installs_of(&center, "prompt:2026-01-15"), 1);
    assert_eq!(installs_of(&center, "prompt:2026-01-16"), 1);
    assert!(restored.is_spent(date("2026-01-16")));
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-17", "prompt:2026-01-18"]
    );

    // A schedule stored before the ledger existed loads as an unspent, unobserved schedule.
    let legacy = r#"{"settings":{"mode":"daily","time":{"hour":9,"minute":0}},"planned":[]}"#;
    let legacy: PromptSchedule = serde_json::from_str(legacy)?;
    assert!(legacy.is_enabled() && legacy.high_water().is_none());
    Ok(())
}

#[test]
fn a_forward_clock_jump_and_back_suppresses_the_days_in_between_rather_than_replaying() -> Result<()>
{
    let mut db = open_db("forward-jump")?;
    let mut schedule = PromptSchedule::default();
    schedule.configure(daily(nine_am()), instant("2026-01-15T10:30:00Z"), UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    // The clock is set five days ahead by mistake. The OS delivers everything it held; the
    // app comes to the foreground and sees the 20th.
    let ahead = instant("2026-01-20T12:00:00Z");
    center.fire_due(ahead);
    assert_eq!(center.delivered.len(), PROMPT_HORIZON_DAYS);
    schedule.refresh(RefreshTrigger::Reentry, ahead, UTC_ZONE);
    assert!(schedule.planned().is_empty());

    // The clock is corrected. Nothing between the real date and the high-water mark is
    // planned: those requests may have been delivered, and ambiguity suppresses.
    let corrected = instant("2026-01-15T12:00:00Z");
    schedule.refresh(RefreshTrigger::AppOpen, corrected, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T12:00:00Z")?;
    assert_eq!(
        fire_instants(&schedule),
        [
            instant("2026-01-21T09:00:00Z"),
            instant("2026-01-22T09:00:00Z"),
            instant("2026-01-23T09:00:00Z"),
        ]
    );
    for day in ["2026-01-16", "2026-01-17", "2026-01-18"] {
        assert_eq!(installs_of(&center, &format!("prompt:{day}")), 1);
    }
    Ok(())
}

#[test]
fn requests_the_os_lost_on_its_own_are_spent_and_never_cost_a_pending_prompt() -> Result<()> {
    let mut db = open_db("os-loss")?;
    let mut schedule = PromptSchedule::default();
    let now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;

    // The OS drops the 17th without delivering it (a purge core cannot distinguish from a
    // delivery under a moved clock). It is spent; the 16th and 18th are untouched.
    center.vanish("prompt:2026-01-17");
    let mutations_before = center.mutations();
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert_eq!(center.mutations(), mutations_before);
    assert!(schedule.is_spent(date("2026-01-17")));
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-16", "prompt:2026-01-18"]
    );

    // The next open fills around the spent date, up to the horizon, and never reinstalls it.
    schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:30:00Z")?;
    assert_eq!(
        center.prompt_ids(),
        [
            "prompt:2026-01-16",
            "prompt:2026-01-18",
            "prompt:2026-01-19"
        ]
    );
    assert_eq!(installs_of(&center, "prompt:2026-01-17"), 1);
    Ok(())
}

#[test]
fn spent_dates_are_pruned_only_by_age_from_the_high_water_mark() -> Result<()> {
    let (mut db, mut center, mut schedule) = configure_and_install_from_seven("pruning")?;
    center.fire_due(instant("2026-01-15T10:00:00Z"));
    pass(&mut db, &mut center, &mut schedule, "2026-01-15T10:00:00Z")?;
    assert!(schedule.is_spent(date("2026-01-15")));

    // No operation removes the date: rollbacks, time changes, disable, re-enable, opens.
    let rolled_back = instant("2026-01-14T08:00:00Z");
    schedule.refresh(RefreshTrigger::ClockOrZoneChange, rolled_back, UTC_ZONE);
    schedule.configure(daily(PromptTime::new(6, 0).unwrap()), rolled_back, UTC_ZONE);
    schedule.configure(PromptSettings::Off, rolled_back, UTC_ZONE);
    schedule.configure(daily(nine_am()), rolled_back, UTC_ZONE);
    schedule.refresh(RefreshTrigger::AppOpen, rolled_back, UTC_ZONE);
    assert!(schedule.is_spent(date("2026-01-15")));
    assert!(schedule
        .planned()
        .iter()
        .all(|prompt| prompt.local_date != date("2026-01-15")));

    // Only age prunes it: once the high-water date is more than the retention beyond it.
    let retention = chrono::Duration::days(SPENT_DATE_RETENTION_DAYS);
    schedule.refresh(
        RefreshTrigger::Reentry,
        instant("2026-01-15T10:00:00Z") + retention,
        UTC_ZONE,
    );
    assert!(schedule.is_spent(date("2026-01-15")));
    schedule.refresh(
        RefreshTrigger::Reentry,
        instant("2026-01-16T00:00:00Z") + retention,
        UTC_ZONE,
    );
    assert!(!schedule.is_spent(date("2026-01-15")));
    Ok(())
}

#[test]
fn the_ordinary_path_installs_every_chosen_day_exactly_once() -> Result<()> {
    let mut db = open_db("ordinary")?;
    let mut schedule = PromptSchedule::default();
    let mut now = instant("2026-01-15T10:30:00Z");
    schedule.configure(daily(nine_am()), now, UTC_ZONE);
    let mut center = FakeCenter::default();
    reconcile_prompts(
        &mut db,
        &FixedClock { instant: now },
        &mut center,
        &mut schedule,
        &CapacityPolicy::default(),
    )?;

    // Ten days of use: the app is opened each morning before the prompt, returns to the
    // foreground in the afternoon, and on the fourth day the user moves the time to 10:00.
    let mut day = date("2026-01-16");
    for ordinal in 0..10 {
        now = day.and_hms_opt(7, 30, 0).unwrap().and_utc();
        center.fire_due(now);
        schedule.refresh(RefreshTrigger::AppOpen, now, UTC_ZONE);
        reconcile_prompts(
            &mut db,
            &FixedClock { instant: now },
            &mut center,
            &mut schedule,
            &CapacityPolicy::default(),
        )?;
        if ordinal == 3 {
            now = day.and_hms_opt(8, 0, 0).unwrap().and_utc();
            schedule.configure(daily(PromptTime::new(10, 0).unwrap()), now, UTC_ZONE);
            reconcile_prompts(
                &mut db,
                &FixedClock { instant: now },
                &mut center,
                &mut schedule,
                &CapacityPolicy::default(),
            )?;
        }
        now = day.and_hms_opt(15, 0, 0).unwrap().and_utc();
        center.fire_due(now);
        schedule.refresh(RefreshTrigger::Reentry, now, UTC_ZONE);
        reconcile_prompts(
            &mut db,
            &FixedClock { instant: now },
            &mut center,
            &mut schedule,
            &CapacityPolicy::default(),
        )?;
        assert!(
            center.delivered.contains(&prompt_notification_id(day)),
            "{day} delivered"
        );
        day = day.succ_opt().unwrap();
    }

    let mut delivered = center.delivered.clone();
    delivered.sort();
    delivered.dedup();
    assert_eq!(
        delivered.len(),
        center.delivered.len(),
        "no date delivered twice"
    );
    let mut expected: Vec<String> = (0..10)
        .map(|offset| prompt_notification_id(date("2026-01-16") + chrono::Duration::days(offset)))
        .collect();
    expected.sort();
    assert_eq!(delivered, expected);
    assert!(center.pending.values().all(|due| *due > now));
    // The afternoon re-entry dropped the delivered day and refilled nothing; the two days the
    // morning open planned beyond it are still held, at the time chosen on the fourth day.
    assert_eq!(
        center.prompt_ids(),
        ["prompt:2026-01-26", "prompt:2026-01-27"]
    );
    assert_eq!(
        center.pending["prompt:2026-01-26"],
        instant("2026-01-26T10:00:00Z")
    );
    Ok(())
}

// ---- Interleavings -----------------------------------------------------------------------
//
// A seeded deterministic generator drives random interleavings of time changes, disable and
// re-enable, every refresh trigger, clock jumps forward and back (within the retention), zone
// changes, reminder-side capacity passes, OS deliveries and OS losses, and reconcile passes,
// some of whose install calls fail: definitively, ambiguously with nothing installed, or
// ambiguously after the OS accepted the request (a lost acknowledgment).
// The invariant: once the OS has delivered or dropped a date's request, that identifier is
// never installed again. Replay is the only defect; suppression is allowed.

/// Error classes a lost acknowledgment surfaces as: the OS may have accepted the request.
const AMBIGUOUS: [ErrorClass; 2] = [ErrorClass::Transient, ErrorClass::Cancelled];

/// Error classes that are definitive rejections: the OS installed nothing.
const DEFINITIVE: [ErrorClass; 3] = [
    ErrorClass::Permanent,
    ErrorClass::Unauthorized,
    ErrorClass::Unsupported,
];

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

struct Interleaving {
    seed: u64,
    db: Database,
    center: FakeCenter,
    schedule: PromptSchedule,
    policy: CapacityPolicy,
    now: DateTime<Utc>,
    /// The furthest the clock has been: rollbacks stay within the retention of it.
    peak: DateTime<Utc>,
    zone: Tz,
    reminders: Vec<String>,
    next_item: usize,
    /// Identifiers the OS has delivered or dropped. None may be installed again.
    gone: std::collections::BTreeSet<String>,
    /// Prompt requests the reminder-side capacity pass cancelled.
    displaced: usize,
    /// Install calls the OS accepted and then reported as failed.
    lost_acknowledgments: usize,
    checked_requests: usize,
    log: Vec<String>,
}

impl Interleaving {
    fn new(seed: u64) -> Result<Interleaving> {
        let now = instant("2026-01-15T10:30:00Z");
        let mut schedule = PromptSchedule::default();
        schedule.configure(daily(nine_am()), now, UTC_ZONE);
        Ok(Interleaving {
            seed,
            db: open_db(&format!("interleaving-{seed}"))?,
            center: FakeCenter::default(),
            schedule,
            policy: tight_policy(),
            now,
            peak: now,
            zone: UTC_ZONE,
            reminders: Vec::new(),
            next_item: 0,
            gone: std::collections::BTreeSet::new(),
            displaced: 0,
            lost_acknowledgments: 0,
            checked_requests: 0,
            log: Vec::new(),
        })
    }

    fn clock(&self) -> FixedClock {
        FixedClock { instant: self.now }
    }

    fn step(&mut self, rng: &mut SplitMix) -> Result<()> {
        match rng.below(28) {
            0..=5 => {
                let minutes = i64::try_from(rng.below(60 * 48) + 1)?;
                self.now += chrono::Duration::minutes(minutes);
                self.peak = self.peak.max(self.now);
                self.center.fire_due(self.now);
                self.note_gone();
                self.log.push(format!("advance to {}", self.now));
            }
            6 => {
                let minutes = i64::try_from(rng.below(60 * 24 * 13) + 1)?;
                let floor = self.peak - chrono::Duration::days(SPENT_DATE_RETENTION_DAYS - 1);
                self.now = (self.now - chrono::Duration::minutes(minutes)).max(floor);
                self.log.push(format!("rollback to {}", self.now));
            }
            7 => {
                let hour = u8::try_from(rng.below(24))?;
                let minute = u8::try_from(rng.below(4) * 15)?;
                let time = PromptTime::new(hour, minute).unwrap();
                self.schedule.configure(daily(time), self.now, self.zone);
                self.log.push(format!("configure {hour:02}:{minute:02}"));
            }
            8 => {
                self.schedule
                    .configure(PromptSettings::Off, self.now, self.zone);
                self.log.push("configure off".to_string());
            }
            9 | 10 => {
                self.schedule
                    .configure(daily(nine_am()), self.now, self.zone);
                self.log.push("configure 09:00 (on)".to_string());
            }
            11 | 12 => {
                self.schedule
                    .refresh(RefreshTrigger::AppOpen, self.now, self.zone);
                self.log.push("refresh app open".to_string());
            }
            13 => {
                self.schedule
                    .refresh(RefreshTrigger::Reentry, self.now, self.zone);
                self.log.push("refresh reentry".to_string());
            }
            14 => {
                let zones: [Tz; 3] = [
                    UTC_ZONE,
                    "America/New_York".parse().unwrap(),
                    "Asia/Tokyo".parse().unwrap(),
                ];
                self.zone = zones[usize::try_from(rng.below(3))?];
                self.schedule
                    .refresh(RefreshTrigger::ClockOrZoneChange, self.now, self.zone);
                self.log
                    .push(format!("refresh clock/zone change ({})", self.zone));
            }
            15 => {
                if self.reminders.len() < 4 {
                    let item_id = format!("item-{}", self.next_item);
                    self.next_item += 1;
                    create_reminder_due(
                        &mut self.db,
                        &item_id,
                        "water plants",
                        "2027-01-01 09:00:00",
                    )?;
                    self.reminders.push(item_id);
                } else if let Some(item_id) = self.reminders.pop() {
                    cancel_reminder_on(&mut self.db, &item_id)?;
                }
                self.reminder_side_pass()?;
            }
            16 => {
                if let Some(item_id) = self.reminders.pop() {
                    cancel_reminder_on(&mut self.db, &item_id)?;
                }
                self.reminder_side_pass()?;
            }
            17 => {
                let pending = self.center.prompt_ids();
                if !pending.is_empty() {
                    let victim = &pending[usize::try_from(rng.below(pending.len() as u64))?];
                    self.center.vanish(victim);
                    self.gone.insert(victim.clone());
                    self.log.push(format!("os drops {victim}"));
                }
            }
            24 => {
                // Lost acknowledgment: the OS accepts each request but the call reports an
                // ambiguous failure.
                let class = AMBIGUOUS[usize::try_from(rng.below(2))?];
                self.center.fail_after_install =
                    Some(NotificationEffectError::new(class, "timed out"));
                let installs_before = self.center.requests.len();
                self.reconcile_prompts("reconcile prompts (lost acknowledgment)")?;
                self.center.fail_after_install = None;
                self.lost_acknowledgments += self.center.requests.len() - installs_before;
            }
            25 => {
                // Definitive rejection: nothing is installed and the attempt is released.
                let class = DEFINITIVE[usize::try_from(rng.below(3))?];
                self.center.fail_schedule = Some(NotificationEffectError::new(class, "rejected"));
                self.reconcile_prompts("reconcile prompts (definitive rejection)")?;
                self.center.fail_schedule = None;
            }
            26 => {
                // Ambiguous failure that installed nothing: indistinguishable from a lost
                // acknowledgment, so the attempt stays held and the date may be skipped.
                let class = AMBIGUOUS[usize::try_from(rng.below(2))?];
                self.center.fail_schedule = Some(NotificationEffectError::new(class, "busy"));
                self.reconcile_prompts("reconcile prompts (ambiguous, nothing installed)")?;
                self.center.fail_schedule = None;
            }
            27 => {
                // The next prompt fires while core is not looking, and the clock is then moved
                // back to before it with no observation in between: the AC2 scenario in its
                // primitive form, applied to whatever state the other steps left behind.
                let next_due = self
                    .center
                    .pending
                    .iter()
                    .filter(|(id, _)| is_prompt_notification_id(id))
                    .map(|(_, due)| *due)
                    .min();
                if let Some(due) = next_due.filter(|due| *due > self.now) {
                    self.now = due;
                    self.peak = self.peak.max(self.now);
                    self.center.fire_due(self.now);
                    self.note_gone();
                    let minutes = i64::try_from(rng.below(60 * 24) + 1)?;
                    let floor = self.peak - chrono::Duration::days(SPENT_DATE_RETENTION_DAYS - 1);
                    self.now = (due - chrono::Duration::minutes(minutes)).max(floor);
                    self.log.push(format!(
                        "fire at {due} unobserved, rollback to {}",
                        self.now
                    ));
                }
            }
            _ => {
                self.reconcile_prompts("reconcile prompts")?;
            }
        }
        self.check_no_replay();
        Ok(())
    }

    fn reconcile_prompts(&mut self, label: &str) -> Result<()> {
        let clock = self.clock();
        reconcile_prompts(
            &mut self.db,
            &clock,
            &mut self.center,
            &mut self.schedule,
            &self.policy,
        )?;
        self.log.push(label.to_string());
        self.check_pass_bounds();
        Ok(())
    }

    fn reminder_side_pass(&mut self) -> Result<()> {
        let clock = self.clock();
        let demands = self.schedule.demands(self.now);
        let report = reconcile_with_capacity(
            &mut self.db,
            &clock,
            &mut self.center,
            &self.policy,
            &demands,
        )?;
        self.schedule.record_capacity_pass(&report, self.now);
        self.displaced += report.prompts_cancelled.len();
        self.log.push(format!(
            "reminder-side pass ({} reminders, cancelled {:?})",
            self.reminders.len(),
            report.prompts_cancelled
        ));
        Ok(())
    }

    fn note_gone(&mut self) {
        for id in &self.center.delivered {
            if is_prompt_notification_id(id) {
                self.gone.insert(id.clone());
            }
        }
    }

    fn check_no_replay(&mut self) {
        for request in &self.center.requests[self.checked_requests..] {
            assert!(
                !self.gone.contains(&request.notification_id),
                "seed {}: {} installed again after the OS delivered or dropped it\n{}",
                self.seed,
                request.notification_id,
                self.log.join("\n")
            );
        }
        self.checked_requests = self.center.requests.len();
    }

    fn check_pass_bounds(&self) {
        let context = || format!("seed {}\n{}", self.seed, self.log.join("\n"));
        assert!(
            self.center.prompt_ids().len() <= PROMPT_HORIZON_DAYS,
            "{}",
            context()
        );
        assert!(
            self.schedule.planned().len() <= PROMPT_HORIZON_DAYS,
            "{}",
            context()
        );
        assert!(
            self.schedule
                .planned()
                .iter()
                .all(|prompt| prompt.fire_at > self.now),
            "{}",
            context()
        );
        if !self.schedule.is_enabled() {
            assert!(self.center.prompt_ids().is_empty(), "{}", context());
        }
    }
}

#[test]
fn random_interleavings_never_install_a_prompt_after_the_os_delivered_or_dropped_it() -> Result<()>
{
    let mut total_installs = 0;
    let mut total_gone = 0;
    let mut total_displaced = 0;
    let mut total_lost_acknowledgments = 0;
    for seed in 1..=48 {
        let mut rng = SplitMix(seed);
        let mut run = Interleaving::new(seed)?;
        for _ in 0..80 {
            run.step(&mut rng)?;
        }
        total_installs += run.center.schedule_calls;
        total_gone += run.gone.len();
        total_displaced += run.displaced;
        total_lost_acknowledgments += run.lost_acknowledgments;
        // Finish with a pass at the clock's furthest reading, then one more look back.
        run.now = run.peak;
        let clock = run.clock();
        reconcile_prompts(
            &mut run.db,
            &clock,
            &mut run.center,
            &mut run.schedule,
            &run.policy,
        )?;
        run.check_pass_bounds();
        run.check_no_replay();
    }
    // The generator is not vacuous: prompts were installed, delivered or dropped, displaced by
    // reminders, and installed under a lost acknowledgment across the runs.
    assert!(
        total_installs > 250
            && total_gone > 100
            && total_displaced > 12
            && total_lost_acknowledgments > 24,
        "{total_installs} installs, {total_gone} delivered or dropped, {total_displaced} \
         displaced, {total_lost_acknowledgments} lost acknowledgments"
    );
    Ok(())
}
