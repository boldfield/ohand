//! T03: reminder and suggestion resilience after a lapse. Every test drives the real reminder
//! coordinator, capacity allocator, reconciliation, prompt schedule and prompt coordinator over a
//! migrated, file-backed SQLite store, and reopens that file to model a restart.
//!
//! Evidence class: the operating system is the in-memory `FakeCenter` below, a mock of the
//! pending-notification set that keeps its contents across app restarts the way the real one
//! does. These tests prove the core's decisions (what it installs, cancels, withholds and never
//! fabricates). They are simulator/mock evidence and are not proof that a physical device
//! delivered anything; that belongs to the device matrix.
//!
//! No test builds a provider, job runner or network client: installed explicit reminders and the
//! prompt schedule are driven only by durable state and the OS pending set.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use ohand_core::export::ReadAuthentication;
use ohand_core::lifecycle::delete_intent::mark_deletion_intent;
use ohand_core::providers::contracts::ErrorClass;
use ohand_core::reminders::capacity::{CapacityPolicy, CapacityReport};
use ohand_core::reminders::coordinator::ReminderCoordinator;
use ohand_core::reminders::reconcile::{
    NotificationEffectError, NotificationEffects, PendingRequest, ScheduleRequest,
};
use ohand_core::reminders::state::{
    apply_derived_request, AcknowledgmentState, DeliveryState, DerivedReminderRequest,
    ReminderRecord, RequestState, ScheduleState, UnschedulableReason,
};
use ohand_core::store::captures::{self, Capture};
use ohand_core::store::events::{self, Correction, CorrectionKind, Event, EventPayload, EventType};
use ohand_core::store::schema::{Clock, Database};
use ohand_core::suggestions::coordinator::{PromptActivation, PromptCoordinator};
use ohand_core::suggestions::schedule::{
    is_prompt_notification_id, reconcile_prompts, PromptPassReport, PromptSchedule, PromptSettings,
    PromptTime, RefreshTrigger, PROMPT_HORIZON_DAYS,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const START: &str = "2026-01-15T10:30:00Z";
const REMINDER_PHRASE: &str = "2026-01-16 09:00:00";
const REMINDER_DUE: &str = "2026-01-16T09:00:00Z";
const AUTHENTICATED: ReadAuthentication = ReadAuthentication::Authenticated;

fn instant(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn zone(name: &str) -> Tz {
    name.parse().unwrap()
}

fn nine_am() -> PromptSettings {
    PromptSettings::Daily {
        time: PromptTime::new(9, 0).unwrap(),
    }
}

struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    fn at(text: &str) -> Arc<TestClock> {
        Arc::new(TestClock {
            now: Mutex::new(instant(text)),
        })
    }

    fn set(&self, text: &str) {
        *self.now.lock().unwrap() = instant(text);
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}

// ---- Fake OS ----

/// The OS pending set. It outlives any one `Database` handle, like the real OS outlives the app.
#[derive(Default)]
struct FakeCenter {
    pending: BTreeMap<String, DateTime<Utc>>,
    schedule_calls: usize,
    cancel_calls: usize,
    /// Everything the OS has delivered, in order. A request is delivered at most once.
    delivered: Vec<String>,
    fail_schedule: Option<NotificationEffectError>,
    /// Install, then report this error: the acknowledgment is lost.
    fail_after_install: Option<NotificationEffectError>,
}

impl FakeCenter {
    fn mutations(&self) -> usize {
        self.schedule_calls + self.cancel_calls
    }

    fn reset_counters(&mut self) {
        self.schedule_calls = 0;
        self.cancel_calls = 0;
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

    fn prompt_ids(&self) -> Vec<String> {
        self.pending
            .keys()
            .filter(|id| is_prompt_notification_id(id))
            .cloned()
            .collect()
    }

    fn reminder_ids(&self) -> Vec<String> {
        self.pending
            .keys()
            .filter(|id| !is_prompt_notification_id(id))
            .cloned()
            .collect()
    }
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.schedule_calls += 1;
        if let Some(error) = &self.fail_schedule {
            return Err(error.clone());
        }
        self.pending
            .insert(request.notification_id.clone(), request.fire_at);
        match &self.fail_after_install {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.cancel_calls += 1;
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

// ---- Fixture ----

struct Fixture {
    db: Database,
    path: String,
    clock: Arc<TestClock>,
    center: FakeCenter,
    schedule: PromptSchedule,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Fixture {
    fn new() -> Fixture {
        let path = format!(
            "{}/test_notification_resilience_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let clock = TestClock::at(START);
        let db = Database::open(&path, clock.clone()).unwrap();
        Fixture {
            db,
            path,
            clock,
            center: FakeCenter::default(),
            schedule: PromptSchedule::default(),
        }
    }

    /// The app is killed and relaunched: a new connection over the same file. The OS pending
    /// set survives, and so does the caller-persisted prompt schedule (serialized round trip).
    fn restart(&mut self) {
        self.db = Database::open(&self.path, self.clock.clone()).unwrap();
        let stored = serde_json::to_string(&self.schedule).unwrap();
        self.schedule = serde_json::from_str(&stored).unwrap();
    }

    /// An active action item whose capture text is `text`. Returns its revision.
    fn add_item(&mut self, item_id: &str, text: &str) -> i32 {
        let capture_id = format!("capture-{item_id}");
        let capture = Capture::new(
            capture_id.clone(),
            Some(text.to_string()),
            None,
            START.to_string(),
            "UTC".to_string(),
            0,
            "en".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            "route-default".to_string(),
            false,
            START.to_string(),
            None,
        )
        .unwrap();
        captures::save_capture(&mut self.db, &capture).unwrap();
        let transaction = self.db.immediate_transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
                 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, START, START],
            )
            .unwrap();
        let event = Event::new(
            format!("type-{item_id}"),
            item_id.to_string(),
            0,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind: CorrectionKind::Type,
                old_value: None,
                new_value: "action".to_string(),
            }),
            START.to_string(),
        )
        .unwrap();
        events::save_event_in_tx(&transaction, &event, 0).unwrap();
        transaction.commit().unwrap();
        1
    }

    /// An item with an explicit reminder due at `phrase` (UTC), not yet installed.
    fn add_reminder(&mut self, item_id: &str, phrase: &str) -> ReminderRecord {
        self.add_item(item_id, &format!("Call back {phrase}"));
        let transaction = self.db.immediate_transaction().unwrap();
        let record = apply_derived_request(
            &transaction,
            self.clock.as_ref(),
            &DerivedReminderRequest {
                item_id: item_id.to_string(),
                source_revision: 1,
                phrase: phrase.to_string(),
            },
        )
        .unwrap();
        transaction.commit().unwrap();
        record
    }

    fn reminder(&mut self, item_id: &str) -> ReminderRecord {
        ReminderCoordinator::new(self.clock.as_ref())
            .current_reminder(&mut self.db, item_id)
            .unwrap()
            .expect("reminder exists")
    }

    fn settle_with(
        &mut self,
        policy: CapacityPolicy,
    ) -> Result<CapacityReport, ohand_core::reminders::reconcile::ReconcileError> {
        let prompts = self.schedule.demands(self.clock.now());
        ReminderCoordinator::new(self.clock.as_ref())
            .with_policy(policy)
            .settle(&mut self.db, &mut self.center, &prompts)
    }

    /// The foreground settlement: reminders first (offering the schedule's prompt demands), then
    /// the prompt pass, then the schedule is told what the capacity pass cancelled.
    fn settle(&mut self) -> CapacityReport {
        let report = self.settle_with(CapacityPolicy::default()).expect("settle");
        self.schedule
            .record_capacity_pass(&report, self.clock.now());
        report
    }

    fn prompt_pass(&mut self) -> PromptPassReport {
        reconcile_prompts(
            &mut self.db,
            self.clock.as_ref(),
            &mut self.center,
            &mut self.schedule,
            &CapacityPolicy::default(),
        )
        .expect("prompt pass")
    }

    fn enable_prompts(&mut self, device_zone: Tz) {
        self.schedule
            .configure(nine_am(), self.clock.now(), device_zone);
        self.prompt_pass();
    }

    /// Time passes with the app closed: the OS delivers what is due and nothing else runs.
    fn lapse_until(&mut self, moment: &str) {
        self.clock.set(moment);
        self.center.fire_due(self.clock.now());
    }

    fn app_open(&mut self, device_zone: Tz) -> PromptPassReport {
        self.restart();
        self.schedule
            .refresh(RefreshTrigger::AppOpen, self.clock.now(), device_zone);
        self.settle();
        self.prompt_pass()
    }

    fn activate_prompt(&mut self, prompt_id: &str) -> PromptActivation {
        PromptCoordinator::new(self.clock.as_ref())
            .activate(&mut self.db, AUTHENTICATED, prompt_id, None)
            .unwrap()
    }

    fn lifecycle(&mut self, item_id: &str) -> String {
        let transaction = self.db.transaction().unwrap();
        events::get_item_snapshot(&transaction, item_id)
            .unwrap()
            .unwrap()
            .lifecycle_state
    }

    fn revision(&mut self, item_id: &str) -> i32 {
        let transaction = self.db.transaction().unwrap();
        events::get_item_snapshot(&transaction, item_id)
            .unwrap()
            .unwrap()
            .revision
    }

    fn event_count(&mut self) -> i64 {
        self.db
            .transaction()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }
}

fn transient(code: &str) -> NotificationEffectError {
    NotificationEffectError::new(ErrorClass::Transient, code)
}

fn local_date(text: &str) -> NaiveDate {
    text.parse().unwrap()
}

// ---- Restart ----

#[test]
fn restart_after_a_lost_acknowledgment_leaves_exactly_one_request() {
    let mut fixture = Fixture::new();
    let created = fixture.add_reminder("item-a", REMINDER_PHRASE);
    let notification_id = created.notification_id().expect("resolved reminder");
    fixture.center.fail_after_install = Some(transient("ack-lost"));

    let first = fixture.settle();
    assert!(!first.converged(), "the lost acknowledgment is reported");
    assert_ne!(
        fixture.reminder("item-a").schedule_state,
        ScheduleState::Scheduled,
        "an unconfirmed install is never reported as scheduled"
    );
    assert_eq!(fixture.center.schedule_calls, 1);

    fixture.center.fail_after_install = None;
    fixture.restart();
    let second = fixture.settle();

    assert!(second.converged());
    assert_eq!(
        fixture.center.schedule_calls, 1,
        "the request the OS already holds is not installed again"
    );
    assert_eq!(fixture.center.reminder_ids(), vec![notification_id]);
    assert_eq!(
        fixture.reminder("item-a").schedule_state,
        ScheduleState::Scheduled
    );

    fixture.center.reset_counters();
    for _ in 0..3 {
        fixture.restart();
        fixture.settle();
    }
    assert_eq!(
        fixture.center.mutations(),
        0,
        "repeated launches change nothing"
    );
}

#[test]
fn a_restart_that_finds_the_pending_set_purged_reinstalls_once_not_twice() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.settle();
    assert_eq!(fixture.center.reminder_ids().len(), 1);

    // The OS lost its pending set (restore, reinstall): the desired reminder is installed again.
    fixture.center.pending.clear();
    fixture.center.reset_counters();
    fixture.restart();
    fixture.settle();
    fixture.settle();

    assert_eq!(fixture.center.schedule_calls, 1);
    assert_eq!(fixture.center.reminder_ids().len(), 1);
    assert!(fixture.center.delivered.is_empty(), "nothing is fabricated");
}

// ---- Permission and capacity changes ----

#[test]
fn a_permission_denial_is_surfaced_and_never_reported_as_scheduled_or_retried_blindly() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.center.fail_schedule = Some(NotificationEffectError::new(
        ErrorClass::Unauthorized,
        "notifications-denied",
    ));

    let denied = fixture.settle();

    assert!(!denied.converged());
    let record = fixture.reminder("item-a");
    assert_eq!(record.schedule_state, ScheduleState::ScheduleFailed);
    assert_eq!(record.delivery_state, DeliveryState::Unknown);
    assert!(fixture.center.pending.is_empty());

    // Permission comes back, but the rejection stays surfaced until it is explicitly re-armed:
    // no silent retry loop and no pretend success.
    fixture.center.fail_schedule = None;
    fixture.center.reset_counters();
    fixture.restart();
    let after_restart = fixture.settle();

    assert_eq!(fixture.center.schedule_calls, 0);
    assert!(!after_restart.reconcile.withheld.is_empty());
    assert_eq!(
        fixture.reminder("item-a").schedule_state,
        ScheduleState::ScheduleFailed
    );
    assert!(fixture.center.delivered.is_empty());
}

#[test]
fn capacity_overflow_is_explicit_and_a_freed_slot_refills_without_duplicates() {
    let mut fixture = Fixture::new();
    let tight = CapacityPolicy {
        os_pending_limit: 6,
        replacement_headroom: 4,
        ..CapacityPolicy::default()
    };
    let phrases = [
        "2026-01-16 09:00:00",
        "2026-01-16 10:00:00",
        "2026-01-16 11:00:00",
    ];
    for (index, phrase) in phrases.iter().enumerate() {
        fixture.add_reminder(&format!("item-{index}"), phrase);
    }

    let report = fixture.settle_with(tight).expect("tight pass");
    assert!(report.converged());
    assert_eq!(fixture.center.reminder_ids().len(), 2, "only the budget");
    let overflow = fixture.reminder("item-2");
    assert_eq!(
        overflow.request_state,
        RequestState::Unschedulable(UnschedulableReason::CapacityExceeded)
    );
    assert_ne!(overflow.schedule_state, ScheduleState::Scheduled);

    // The soonest reminder's item ends (the OS drops its request) and the held-back reminder
    // returns with the same identifier.
    let freed = fixture.reminder("item-2").notification_id();
    let first_id = fixture.reminder("item-0").notification_id().unwrap();
    fixture.center.pending.remove(&first_id);
    fixture.db = Database::open(&fixture.path, fixture.clock.clone()).unwrap();
    {
        let transaction = fixture.db.immediate_transaction().unwrap();
        transaction
            .execute(
                "UPDATE items SET lifecycle_state = 'completed' WHERE item_id = 'item-0'",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();
    }
    fixture.center.reset_counters();
    let refill = fixture.settle_with(tight).expect("refill pass");

    assert!(refill.converged());
    assert_eq!(refill.refilled.len(), 1);
    assert_eq!(fixture.center.reminder_ids().len(), 2);
    assert!(fixture
        .center
        .reminder_ids()
        .contains(&freed.expect("generation kept")));
    let before = fixture.center.schedule_calls;
    fixture.settle_with(tight).expect("idempotent pass");
    assert_eq!(
        fixture.center.schedule_calls, before,
        "no duplicate install"
    );
}

// ---- Time zones ----

#[test]
fn a_zone_change_keeps_the_absolute_reminder_instant_and_replans_prompts_only_on_the_next_open() {
    let mut fixture = Fixture::new();
    let new_york = zone("America/New_York");
    let tokyo = zone("Asia/Tokyo");
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.enable_prompts(new_york);
    fixture.settle();
    fixture.prompt_pass();
    assert_eq!(
        fixture.center.pending.get("prompt:2026-01-16"),
        Some(&instant("2026-01-16T14:00:00Z"))
    );
    let reminder_id = fixture.reminder("item-a").notification_id().unwrap();
    assert_eq!(
        fixture.center.pending.get(&reminder_id),
        Some(&instant(REMINDER_DUE))
    );

    // Landing in Tokyo: the zone event alone neither refills nor replays prompts, and the
    // explicit reminder keeps its absolute instant.
    fixture.clock.set("2026-01-15T23:00:00Z");
    fixture.schedule.refresh(
        RefreshTrigger::ClockOrZoneChange,
        fixture.clock.now(),
        tokyo,
    );
    fixture.center.reset_counters();
    fixture.settle();
    fixture.prompt_pass();
    assert_eq!(fixture.center.schedule_calls, 0);
    assert_eq!(
        fixture.center.pending.get(&reminder_id),
        Some(&instant(REMINDER_DUE))
    );

    // The next open plans for the Tokyo morning, entirely in the future, one per date.
    let report = fixture.app_open(tokyo);
    assert!(report.converged());
    assert_eq!(
        fixture.center.pending.get("prompt:2026-01-16"),
        Some(&instant("2026-01-16T00:00:00Z"))
    );
    assert_eq!(fixture.center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert!(fixture
        .center
        .prompt_ids()
        .iter()
        .all(|id| fixture.center.pending[id] > fixture.clock.now()));
    assert_eq!(
        fixture.center.pending.get(&reminder_id),
        Some(&instant(REMINDER_DUE)),
        "still exactly one request for the reminder"
    );
    assert_eq!(fixture.center.reminder_ids().len(), 1);
}

// ---- Multi-day nonuse ----

#[test]
fn ten_silent_days_deliver_once_leave_no_backlog_and_never_complete_anything() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.add_item("item-open", "Water the plants");
    fixture.enable_prompts(Tz::UTC);
    fixture.settle();
    fixture.prompt_pass();
    let reminder_id = fixture.reminder("item-a").notification_id().unwrap();
    let events_before = fixture.event_count();
    let revision_before = fixture.revision("item-a");

    fixture.lapse_until("2026-01-25T14:00:00Z");

    assert_eq!(
        fixture
            .center
            .delivered
            .iter()
            .filter(|id| **id == reminder_id)
            .count(),
        1,
        "delivered exactly once by the OS"
    );
    assert!(fixture.center.pending.is_empty(), "nothing queued up");

    // Returning without a cold open (foreground passes) schedules nothing.
    fixture.center.reset_counters();
    fixture.settle();
    let silent = fixture.prompt_pass();
    assert_eq!(fixture.center.mutations(), 0, "no catch-up burst");
    assert!(silent.installed.is_empty());

    // The reminder was never marked delivered, opened, acknowledged or completed by silence.
    let record = fixture.reminder("item-a");
    assert_eq!(
        record.acknowledgment_state,
        AcknowledgmentState::NotAcknowledged
    );
    assert_ne!(record.delivery_state, DeliveryState::Opened);
    assert_eq!(fixture.lifecycle("item-a"), "active");
    assert_eq!(fixture.revision("item-a"), revision_before);
    assert_eq!(fixture.event_count(), events_before);

    // A cold open starts from the future: three upcoming prompts, none for the missed days.
    let reopened = fixture.app_open(Tz::UTC);
    assert!(reopened.converged());
    assert_eq!(fixture.center.prompt_ids().len(), PROMPT_HORIZON_DAYS);
    assert!(fixture
        .center
        .pending
        .values()
        .all(|due| *due > instant("2026-01-25T14:00:00Z")));
    assert!(!fixture.center.pending.contains_key("prompt:2026-01-16"));
    assert!(
        !fixture.center.pending.contains_key(&reminder_id),
        "a passed reminder is not reinstalled"
    );
    assert_eq!(fixture.center.reminder_ids().len(), 0);
    assert_eq!(fixture.lifecycle("item-a"), "active");
    assert_eq!(fixture.lifecycle("item-open"), "active");
}

#[test]
fn a_reminder_that_lapsed_before_it_was_ever_installed_is_expired_not_backdated() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    // The app was never reopened between creation and a time well after the due instant.
    fixture.clock.set("2026-01-20T08:00:00Z");

    fixture.restart();
    let report = fixture.settle();

    assert_eq!(fixture.center.schedule_calls, 0, "never installed late");
    assert_eq!(report.reconcile.expired.len(), 1);
    let record = fixture.reminder("item-a");
    assert_eq!(
        record.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(record.resolved_instant, Some(instant(REMINDER_DUE)));
    assert_eq!(record.delivery_state, DeliveryState::Unknown);
    assert!(fixture.center.delivered.is_empty());
}

// ---- Installed reminders do not depend on AI, Mac or network ----

#[test]
fn an_installed_reminder_fires_and_settles_with_no_provider_runner_or_network() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.settle();
    let reminder_id = fixture.reminder("item-a").notification_id().unwrap();

    // Everything the capture pipeline needs is absent: this fixture holds no provider profile,
    // route, job runner or transport. The request in the OS fires on its own.
    fixture.restart();
    fixture.lapse_until("2026-01-16T09:00:01Z");
    assert_eq!(fixture.center.delivered, vec![reminder_id]);

    fixture.center.reset_counters();
    fixture.restart();
    let after = fixture.settle();
    assert!(after.converged());
    assert_eq!(fixture.center.mutations(), 0, "no duplicate after delivery");
    assert_eq!(fixture.center.delivered.len(), 1);
}

// ---- Prompts and deleted items ----

#[test]
fn a_deleted_item_is_never_offered_again_and_its_reminder_request_is_removed() {
    let mut fixture = Fixture::new();
    fixture.add_reminder("item-a", REMINDER_PHRASE);
    fixture.add_item("item-b", "Water the plants");
    let revision = fixture.revision("item-a");
    let revision_b = fixture.revision("item-b");
    fixture.settle();
    let reminder_id = fixture.reminder("item-a").notification_id().unwrap();
    assert!(fixture.center.pending.contains_key(&reminder_id));

    let offered = match fixture.activate_prompt("prompt:2026-01-15") {
        PromptActivation::Offer(offer) => offer.item_id,
        PromptActivation::NothingToOffer => panic!("the active action should be offered"),
    };
    assert_eq!(
        offered, "item-b",
        "an item that already has its own reminder is not suggested"
    );

    // Deleted while the offer sits in a notification; days pass before the next activation.
    let now = fixture.clock.now();
    mark_deletion_intent(&mut fixture.db, "item-a", revision, now).unwrap();
    mark_deletion_intent(&mut fixture.db, "item-b", revision_b, now).unwrap();
    fixture.clock.set("2026-01-18T09:00:00Z");
    fixture.restart();
    fixture.settle();

    assert!(
        !fixture.center.pending.contains_key(&reminder_id),
        "the deleted item's reminder is cancelled"
    );
    for day in ["2026-01-16", "2026-01-17", "2026-01-18"] {
        assert_eq!(
            fixture.activate_prompt(&format!("prompt:{day}")),
            PromptActivation::NothingToOffer,
            "stale selection must not resurface on {day}"
        );
    }
    assert!(fixture.center.delivered.is_empty());
}

#[test]
fn the_ledger_survives_restarts_so_a_fired_prompt_date_is_never_replayed() {
    let mut fixture = Fixture::new();
    fixture.enable_prompts(Tz::UTC);
    fixture.prompt_pass();
    fixture.lapse_until("2026-01-16T09:30:00Z");
    assert_eq!(
        fixture.center.delivered,
        vec!["prompt:2026-01-16".to_string()]
    );

    // Time is set back (a manual clock change) and the app restarts and reopens.
    fixture.clock.set("2026-01-16T06:00:00Z");
    fixture.center.reset_counters();
    fixture.app_open(Tz::UTC);

    assert!(fixture.schedule.is_spent(local_date("2026-01-16")));
    assert!(!fixture.center.pending.contains_key("prompt:2026-01-16"));
    assert_eq!(fixture.center.delivered.len(), 1);
    assert!(!fixture.center.prompt_ids().is_empty());
    assert!(fixture.center.prompt_ids().len() <= PROMPT_HORIZON_DAYS);
    assert!(fixture
        .center
        .pending
        .values()
        .all(|due| *due > instant("2026-01-16T09:30:00Z")));
}
