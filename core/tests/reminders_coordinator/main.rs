//! N05: capture-to-installed-notification path and the edit/cancel/completion path. Every test
//! drives a saved capture in a migrated, file-backed SQLite store through the job runner, the
//! real interpretation dispatcher (offline fast path or the scripted Anthropic adapter), the real
//! apply boundary and the real reconciliation against a fake OS pending set, then reads durable
//! state back through a fresh connection.

use chrono::{DateTime, Duration, Utc};
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, InterpretationDispatcher, InterpretationRoute,
};
use ohand_core::jobs::queue::enqueue_job;
use ohand_core::jobs::runner::{
    InterpretationCapability, JobCapabilities, JobResult, JobRunner, RunnerConfig, Settlement,
};
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::providers::anthropic::fake::{FakeAnthropicStep, FakeAnthropicTransport};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    CancelToken, CapabilityMetadata, ErrorClass, ManualClock, ProviderCapability, ProviderProfile,
    ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode,
};
use ohand_core::reminders::coordinator::{CoordinatorError, ReminderCoordinator};
use ohand_core::reminders::reconcile::{
    NotificationEffectError, NotificationEffects, PendingRequest, ScheduleRequest,
};
use ohand_core::reminders::state::{
    ReminderCancellation, ReminderRecord, ReminderStateError, RequestState, ScheduleState,
    UnschedulableReason, UserTimeCorrection,
};
use ohand_core::store::events::{Event, EventPayload, EventType};
use ohand_core::store::schema::{Clock, Database};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const CAPTURED: &str = "2026-01-15T10:30:00Z";
const ROUTE_ID: &str = "route-1";
const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const ANTHROPIC_MODEL: &str = "synthetic-anthropic-model";
const OFFLINE_COMMAND: &str = "Remind me 2026-02-20 14:30:00 to call mom";
const OFFLINE_DUE: &str = "2026-02-20T14:30:00Z";
const FREE_FORM: &str = "The leak is getting worse. Remind me 2026-02-20 14:30:00 to call them";

fn instant(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    List,
    Schedule(String),
    Cancel(String),
}

/// The OS pending set. It outlives any one `Database` handle, like the real OS outlives the app.
#[derive(Default)]
struct FakeCenter {
    pending: BTreeMap<String, DateTime<Utc>>,
    calls: Vec<Call>,
    fail_list: Option<NotificationEffectError>,
    fail_schedule: Option<NotificationEffectError>,
    fail_cancel: Option<NotificationEffectError>,
    install_then_report_transient: bool,
}

impl FakeCenter {
    fn mutations(&self) -> Vec<Call> {
        self.calls
            .iter()
            .filter(|call| !matches!(call, Call::List))
            .cloned()
            .collect()
    }

    fn schedules(&self) -> usize {
        self.calls
            .iter()
            .filter(|call| matches!(call, Call::Schedule(_)))
            .count()
    }

    fn only_pending(&self) -> (String, DateTime<Utc>) {
        assert_eq!(self.pending.len(), 1, "{:?}", self.pending);
        let (id, due) = self.pending.iter().next().unwrap();
        (id.clone(), *due)
    }
}

impl NotificationEffects for FakeCenter {
    fn schedule(&mut self, request: &ScheduleRequest) -> Result<(), NotificationEffectError> {
        self.calls
            .push(Call::Schedule(request.notification_id.clone()));
        if let Some(error) = &self.fail_schedule {
            return Err(error.clone());
        }
        self.pending
            .insert(request.notification_id.clone(), request.fire_at);
        if self.install_then_report_transient {
            return Err(transient("ambiguous"));
        }
        Ok(())
    }

    fn cancel(&mut self, notification_id: &str) -> Result<(), NotificationEffectError> {
        self.calls.push(Call::Cancel(notification_id.to_string()));
        if let Some(error) = &self.fail_cancel {
            return Err(error.clone());
        }
        self.pending.remove(notification_id);
        Ok(())
    }

    fn list_pending(&mut self) -> Result<Vec<PendingRequest>, NotificationEffectError> {
        self.calls.push(Call::List);
        if let Some(error) = &self.fail_list {
            return Err(error.clone());
        }
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

fn transient(code: &str) -> NotificationEffectError {
    NotificationEffectError::new(ErrorClass::Transient, code)
}

// ---- Provider fixtures ----

fn anthropic_profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-anthropic",
        ProviderProtocol::Anthropic,
        ANTHROPIC_MODEL,
    )
    .credential_ref("credential-ref/anthropic-primary")
    .authorized_destination(ANTHROPIC_ORIGIN)
    .capability(
        CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "coordinator-fixtures:reminders_coordinator",
        )
        .with_input_size_limit(500)
        .with_structured_output(StructuredOutputMode::JsonSchema),
    )
    .build()
    .unwrap()
}

fn anthropic_reply(proposal: Value) -> FakeAnthropicStep {
    FakeAnthropicStep::respond_json(
        200,
        &json!({
            "id": "msg_synthetic_01",
            "type": "message",
            "role": "assistant",
            "model": ANTHROPIC_MODEL,
            "content": [{
                "type": "tool_use",
                "id": "toolu_synthetic_01",
                "name": INTERPRETATION_TOOL_NAME,
                "input": proposal
            }],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 42, "output_tokens": 17 },
        }),
    )
}

fn span_of(text: &str, needle: &str) -> Value {
    let byte_start = text.find(needle).expect("needle in text");
    let start = text[..byte_start].chars().count();
    json!({ "start": start, "end": start + needle.chars().count() })
}

fn reminder_proposal(text: &str, phrase: &str, instant: &str) -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [span_of(text, "call them")],
        "reminder_proposal": {
            "instant": instant,
            "timezone_id": "UTC",
            "quality": "explicit",
            "source_span": span_of(text, phrase),
        },
    })
}

struct Providers {
    clock: Arc<ManualClock>,
    anthropic: Arc<FakeAnthropicTransport>,
    registry: AdapterRegistry,
}

impl Providers {
    fn new(steps: Vec<FakeAnthropicStep>) -> Providers {
        let clock = Arc::new(ManualClock::new());
        let anthropic = Arc::new(FakeAnthropicTransport::new(clock.clone(), steps));
        let registry = AdapterRegistry::new().with_adapter(
            ProviderProtocol::Anthropic,
            AnthropicAdapter::new(anthropic.clone(), AnthropicSettings::default()),
        );
        Providers {
            clock,
            anthropic,
            registry,
        }
    }

    fn calls(&self) -> usize {
        self.anthropic.calls().len()
    }

    fn runner<'a>(&'a self, clock: &'a dyn Clock) -> JobRunner<'a> {
        let capabilities = JobCapabilities::new().with(InterpretationCapability::new(
            InterpretationDispatcher::new(&self.registry, self.clock.as_ref()),
        ));
        JobRunner::new(capabilities, clock, RunnerConfig::default())
    }
}

// ---- Fixture ----

struct Fixture {
    db: Database,
    path: String,
    clock: Arc<TestClock>,
    item_id: String,
    profile: ProviderProfile,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn open(path: &str, clock: &Arc<TestClock>) -> Database {
    Database::open(path, clock.clone()).unwrap()
}

impl Fixture {
    /// A saved capture of `text` (captured at `CAPTURED`, UTC) with an approved provider.
    fn new(text: &str) -> Fixture {
        let path = format!(
            "{}/test_reminders_coordinator_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let clock = TestClock::at(CAPTURED);
        let db = open(&path, &clock);
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        db.conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
                rusqlite::params![capture_id, text, CAPTURED, ROUTE_ID, CAPTURED],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, CAPTURED, CAPTURED],
            )
            .unwrap();
        let fixture = Fixture {
            db,
            path,
            clock,
            item_id,
            profile: anthropic_profile(),
        };
        fixture.install_provider();
        fixture
    }

    fn install_provider(&self) {
        let profile = &self.profile;
        let record = serde_json::to_value(profile).unwrap();
        let origins = serde_json::to_string(&[ANTHROPIC_ORIGIN]).unwrap();
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, \
             capabilities, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                profile.profile_version(),
                profile.profile_id(),
                record["protocol"].as_str().unwrap(),
                record["endpoint"].as_str(),
                profile.model(),
                record["credential_ref"].as_str(),
                i64::from(profile.timeout_seconds()),
                record["retry_policy"].to_string(),
                record["authorized_destinations"].to_string(),
                record["capabilities"].to_string(),
                CAPTURED,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
             VALUES (?, 'private notes', 'personal', ?, ?)",
            rusqlite::params![ROUTE_ID, origins, CAPTURED],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, created_at) \
             VALUES (?, ?, 'text_interpretation', ?, ?)",
            rusqlite::params![Uuid::new_v4().to_string(), ROUTE_ID, origins, CAPTURED],
        )
        .unwrap();
    }

    fn enqueue_interpretation(&mut self) {
        enqueue_job(
            &mut self.db,
            Uuid::new_v4().to_string(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            0,
            Some(self.profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            self.clock.now(),
        )
        .unwrap();
    }

    /// Drain ready jobs and settle the OS, as the foreground lifecycle does.
    fn drain(&mut self, providers: &Providers, center: &mut FakeCenter) -> DrainSummary {
        let coordinator = ReminderCoordinator::new(self.clock.as_ref());
        let runner = providers.runner(self.clock.as_ref());
        let outcome =
            coordinator.drain_and_settle(&mut self.db, &runner, &CancelToken::new(), center, &[]);
        DrainSummary {
            results: outcome
                .drain
                .jobs
                .iter()
                .map(|job| job.result.clone())
                .collect(),
            settled_ok: outcome.settled.is_ok(),
        }
    }

    fn settle(&mut self, center: &mut FakeCenter) {
        ReminderCoordinator::new(self.clock.as_ref())
            .settle(&mut self.db, center, &[])
            .expect("settle");
    }

    /// Simulate the app being killed and relaunched: a new connection over the same file.
    fn restart(&mut self) {
        self.db = open(&self.path, &self.clock);
    }

    fn reminder(&mut self) -> ReminderRecord {
        self.try_reminder().expect("reminder exists")
    }

    fn try_reminder(&mut self) -> Option<ReminderRecord> {
        ReminderCoordinator::new(self.clock.as_ref())
            .current_reminder(&mut self.db, &self.item_id)
            .unwrap()
    }

    fn revision(&self) -> i32 {
        open(&self.path, &self.clock)
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn correction(&mut self, command_id: &str, moment: &str) -> UserTimeCorrection {
        UserTimeCorrection {
            command_id: command_id.to_string(),
            item_id: self.item_id.clone(),
            expected_revision: self.revision(),
            expected_reminder_version: self.reminder().state_version,
            instant: instant(moment),
            timezone_id: "UTC".to_string(),
        }
    }

    fn correct(
        &mut self,
        center: &mut FakeCenter,
        correction: &UserTimeCorrection,
    ) -> Result<ohand_core::reminders::coordinator::ReminderEdit, CoordinatorError> {
        ReminderCoordinator::new(self.clock.as_ref()).correct_time(
            &mut self.db,
            center,
            &[],
            correction,
        )
    }
}

struct DrainSummary {
    results: Vec<JobResult>,
    settled_ok: bool,
}

fn completed(summary: &DrainSummary) -> bool {
    summary.settled_ok
        && matches!(
            summary.results.as_slice(),
            [JobResult::Settled(Settlement::Completed)]
        )
}

// ---- Capture to installed notification ----

#[test]
fn a_supported_offline_command_is_installed_without_any_provider_call() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();

    let summary = fixture.drain(&providers, &mut center);

    assert!(completed(&summary), "{:?}", summary.results);
    assert_eq!(providers.calls(), 0, "the fast path needs no remote call");
    let reminder = fixture.reminder();
    assert_eq!(reminder.request_state, RequestState::Resolved);
    assert_eq!(reminder.schedule_state, ScheduleState::Scheduled);
    let (id, due) = center.only_pending();
    assert_eq!(Some(id), reminder.notification_id());
    assert_eq!(due, instant(OFFLINE_DUE));
    assert_eq!(center.schedules(), 1);
}

#[test]
fn offline_routing_is_reported_by_the_dispatcher_as_the_fast_path() {
    // Guards the first test's premise: the job completed by the fast path, not by a provider
    // that happened to need no call.
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    fixture.enqueue_interpretation();
    let job = ohand_core::jobs::queue::claim_job_with_lease(
        &mut fixture.db,
        Duration::seconds(60),
        fixture.clock.now(),
    )
    .unwrap()
    .expect("job");

    let outcome = InterpretationDispatcher::new(&providers.registry, providers.clock.as_ref())
        .run_job(
            &mut fixture.db,
            &job,
            &CancelToken::new(),
            fixture.clock.now(),
        )
        .unwrap();

    assert!(matches!(
        outcome,
        ohand_core::interpretation::dispatch::DispatchOutcome::Interpreted {
            route: InterpretationRoute::FastPath,
            ..
        }
    ));
}

#[test]
fn scheduled_is_recorded_only_after_the_os_accepts_the_request() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter {
        fail_schedule: Some(transient("os_busy")),
        ..FakeCenter::default()
    };
    fixture.enqueue_interpretation();

    fixture.drain(&providers, &mut center);

    let reminder = fixture.reminder();
    assert_eq!(reminder.request_state, RequestState::Resolved);
    assert_ne!(reminder.schedule_state, ScheduleState::Scheduled);
    assert!(center.pending.is_empty());

    center.fail_schedule = None;
    fixture.settle(&mut center);

    assert_eq!(fixture.reminder().schedule_state, ScheduleState::Scheduled);
    assert_eq!(center.only_pending().1, instant(OFFLINE_DUE));
}

#[test]
fn a_definitive_os_rejection_is_reported_not_scheduled_and_not_retried_blindly() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter {
        fail_schedule: Some(NotificationEffectError::new(
            ErrorClass::Unauthorized,
            "not_authorized",
        )),
        ..FakeCenter::default()
    };
    fixture.enqueue_interpretation();

    fixture.drain(&providers, &mut center);
    let attempts = center.schedules();
    fixture.settle(&mut center);

    let reminder = fixture.reminder();
    assert_eq!(reminder.request_state, RequestState::Resolved);
    assert_eq!(reminder.schedule_state, ScheduleState::ScheduleFailed);
    assert_eq!(center.schedules(), attempts, "a rejection is withheld");
    assert!(center.pending.is_empty());
}

#[test]
fn an_approved_asynchronous_result_is_installed_within_its_opportunity() {
    let mut fixture = Fixture::new(FREE_FORM);
    let providers = Providers::new(vec![anthropic_reply(reminder_proposal(
        FREE_FORM,
        "2026-02-20 14:30:00",
        OFFLINE_DUE,
    ))]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();

    let summary = fixture.drain(&providers, &mut center);

    assert!(completed(&summary), "{:?}", summary.results);
    assert_eq!(providers.calls(), 1);
    assert_eq!(fixture.reminder().schedule_state, ScheduleState::Scheduled);
    assert_eq!(center.only_pending().1, instant(OFFLINE_DUE));
}

#[test]
fn a_result_that_arrives_after_its_requested_time_is_not_installed_or_backdated() {
    let mut fixture = Fixture::new(FREE_FORM);
    let providers = Providers::new(vec![anthropic_reply(reminder_proposal(
        FREE_FORM,
        "2026-02-20 14:30:00",
        OFFLINE_DUE,
    ))]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    // The provider only answers after the requested instant has passed.
    fixture.clock.set("2026-02-21T09:00:00Z");

    let summary = fixture.drain(&providers, &mut center);

    assert!(completed(&summary), "{:?}", summary.results);
    let reminder = fixture.reminder();
    assert_eq!(
        reminder.request_state,
        RequestState::Unschedulable(UnschedulableReason::TimeInPast)
    );
    assert_eq!(reminder.resolved_instant, Some(instant(OFFLINE_DUE)));
    assert_ne!(reminder.schedule_state, ScheduleState::Scheduled);
    assert_eq!(center.schedules(), 0);
    assert!(center.pending.is_empty());
}

// ---- Edits, retries and restart ----

#[test]
fn a_time_correction_replaces_the_pending_request_and_a_retry_adds_nothing() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let original_id = center.only_pending().0;
    let correction = fixture.correction("cmd-1", "2026-02-22T08:00:00Z");

    let edit = fixture.correct(&mut center, &correction).unwrap();

    let reminder = edit.reminder.expect("reminder");
    assert!(edit.settled.is_ok());
    assert_eq!(reminder.schedule_state, ScheduleState::Scheduled);
    assert_eq!(
        reminder.resolved_instant,
        Some(instant("2026-02-22T08:00:00Z"))
    );
    let (new_id, due) = center.only_pending();
    assert_ne!(
        new_id, original_id,
        "a new generation never reuses an identifier"
    );
    assert_eq!(Some(new_id), reminder.notification_id());
    assert_eq!(due, instant("2026-02-22T08:00:00Z"));

    center.calls.clear();
    let retried = fixture.correct(&mut center, &correction).unwrap();

    assert_eq!(retried.reminder, Some(reminder));
    assert!(center.mutations().is_empty(), "{:?}", center.calls);
    assert_eq!(center.pending.len(), 1);
}

#[test]
fn a_command_issued_against_an_older_reminder_version_is_rejected_without_effects() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let stale = fixture.correction("cmd-stale", "2026-02-23T08:00:00Z");
    let fresh = fixture.correction("cmd-fresh", "2026-02-22T08:00:00Z");
    fixture.correct(&mut center, &fresh).unwrap();
    center.calls.clear();

    let error = fixture.correct(&mut center, &stale).unwrap_err();

    assert!(
        matches!(
            error,
            CoordinatorError::State(ReminderStateError::StaleReminderVersion { .. })
        ),
        "{error:?}"
    );
    assert!(center.calls.is_empty(), "{:?}", center.calls);
    assert_eq!(center.only_pending().1, instant("2026-02-22T08:00:00Z"));
}

#[test]
fn an_edit_stays_committed_when_the_os_cannot_be_reached_and_the_next_pass_finishes_it() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let correction = fixture.correction("cmd-1", "2026-02-22T08:00:00Z");
    center.fail_list = Some(transient("os_unavailable"));

    let edit = fixture.correct(&mut center, &correction).unwrap();

    assert!(edit.settled.is_err());
    let reminder = edit.reminder.expect("reminder");
    assert_eq!(
        reminder.resolved_instant,
        Some(instant("2026-02-22T08:00:00Z"))
    );
    assert_eq!(reminder.schedule_state, ScheduleState::PendingSchedule);

    center.fail_list = None;
    fixture.restart();
    fixture.settle(&mut center);

    assert_eq!(fixture.reminder().schedule_state, ScheduleState::Scheduled);
    assert_eq!(center.only_pending().1, instant("2026-02-22T08:00:00Z"));
}

#[test]
fn an_install_whose_acknowledgment_was_lost_is_not_duplicated_after_restart() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter {
        install_then_report_transient: true,
        ..FakeCenter::default()
    };
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    assert_ne!(fixture.reminder().schedule_state, ScheduleState::Scheduled);
    let installed = center.only_pending();

    center.install_then_report_transient = false;
    center.calls.clear();
    fixture.restart();
    fixture.settle(&mut center);

    assert_eq!(fixture.reminder().schedule_state, ScheduleState::Scheduled);
    assert_eq!(center.only_pending(), installed);
    assert!(center.mutations().is_empty(), "{:?}", center.calls);
}

#[test]
fn a_restart_over_converged_state_makes_no_os_mutation_and_keeps_one_reminder() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let before = fixture.reminder();
    center.calls.clear();

    fixture.restart();
    fixture.settle(&mut center);
    let summary = fixture.drain(&providers, &mut center);

    assert!(summary.results.is_empty(), "no job to re-run");
    assert!(center.mutations().is_empty(), "{:?}", center.calls);
    assert_eq!(fixture.reminder(), before);
    assert_eq!(center.pending.len(), 1);
}

// ---- Cancellation and completion ----

#[test]
fn cancelling_the_reminder_removes_the_request_and_keeps_the_item_active() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let cancellation = ReminderCancellation {
        command_id: "cmd-cancel".to_string(),
        item_id: fixture.item_id.clone(),
        expected_revision: fixture.revision(),
        expected_reminder_version: fixture.reminder().state_version,
    };

    let edit = ReminderCoordinator::new(fixture.clock.as_ref())
        .cancel(&mut fixture.db, &mut center, &[], &cancellation)
        .unwrap();

    assert!(edit.settled.is_ok());
    assert_eq!(
        edit.reminder.unwrap().request_state,
        RequestState::Cancelled
    );
    assert!(center.pending.is_empty());
    let lifecycle: String = open(&fixture.path, &fixture.clock)
        .conn()
        .query_row(
            "SELECT lifecycle_state FROM items WHERE item_id = ?",
            [&fixture.item_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lifecycle, "active");
}

fn completion_event(item_id: &str, revision: i32) -> Event {
    Event::new(
        format!("evt-complete-{item_id}"),
        item_id.to_string(),
        revision,
        EventType::Completion,
        EventPayload::Completion,
        "2026-01-15T11:00:00Z".to_string(),
    )
    .unwrap()
}

#[test]
fn completing_the_item_removes_its_request_and_a_lost_cancel_is_retried() {
    let mut fixture = Fixture::new(OFFLINE_COMMAND);
    let providers = Providers::new(vec![]);
    let mut center = FakeCenter::default();
    fixture.enqueue_interpretation();
    fixture.drain(&providers, &mut center);
    let revision = fixture.revision();
    let event = completion_event(&fixture.item_id, revision);
    center.fail_cancel = Some(transient("os_busy"));
    let coordinator_clock = fixture.clock.clone();

    let edit = ReminderCoordinator::new(coordinator_clock.as_ref())
        .end_item(&mut fixture.db, &mut center, &[], &event, revision)
        .unwrap();

    assert_eq!(
        edit.reminder.unwrap().request_state,
        RequestState::Cancelled
    );
    assert!(!edit.settled.unwrap().converged(), "the cancel failed");
    assert_eq!(
        center.pending.len(),
        1,
        "the stale request is still pending"
    );

    center.fail_cancel = None;
    fixture.restart();
    fixture.settle(&mut center);

    assert!(center.pending.is_empty());
    fixture.settle(&mut center);
    assert!(center.pending.is_empty());
}
