use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use ohand_core::interpretation::instructions::{content_version, M1_INSTRUCTION_TEXT};
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::queue::enqueue_job;
use ohand_core::privacy::routing::{authorize_job, AuthorizationDecision, JOB_TYPE_INTERPRET};
use ohand_core::providers::contracts::fake::{FakeProvider, FakeStep};
use ohand_core::providers::contracts::{
    CallObservations, CancelToken, CapabilityMetadata, DiagnosticAdapterCall, DiagnosticResponse,
    DiagnosticSetting, DiagnosticSupport, DiagnosticTransportFailure, DispatchLimits, ManualClock,
    ProviderAdapter, ProviderCapability, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    RequestedSettings, TextBasis, TokenUsage, TransportError,
};
use ohand_core::review::compare::{ArmLabel, FrozenComparisonSource};
use ohand_core::store::schema::{Database, SystemClock};
use ohand_core::time::TimeContext;
use uuid::Uuid;

use super::*;
use crate::journal::JournalReader;
use crate::records::{AttemptState, Experiment};

const ROUTE: &str = "route-1";
const OPENAI: &str = "https://api.openai.com";
const ANTHROPIC: &str = "https://api.anthropic.com";
const STAMP: &str = "2026-01-15T10:30:00Z";
const BODY_A: &str = r#"{"operation":{"kind":"annotate","note":"CANARY-A"}}"#;
const BODY_B: &str = r#"{"operation":{"kind":"annotate","note":"CANARY-B"}}"#;

fn stamp<T: serde::de::DeserializeOwned>() -> T {
    serde_json::from_value(serde_json::json!(STAMP)).unwrap()
}

fn time_context() -> TimeContext {
    serde_json::from_value(serde_json::json!({
        "timezone": "UTC",
        "locale": "en",
        "reference_time": STAMP,
        "utc_offset_at_capture": 0,
        "calendar": "gregorian",
    }))
    .unwrap()
}

fn hosted_profile(
    id: &str,
    protocol: ProviderProtocol,
    model: &str,
    origin: &str,
) -> ProviderProfile {
    ProviderProfileBuilder::new(id, protocol, model)
        .credential_ref("credential-ref")
        .authorized_destination(origin)
        .capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "synthetic")
                .with_input_size_limit(1000),
        )
        .build()
        .unwrap()
}

fn settings() -> RequestedSettings {
    RequestedSettings::new()
        .with_temperature_milli(0)
        .unwrap()
        .with_max_output_tokens(512)
        .unwrap()
        .with_seed(7)
}

fn support_all() -> DiagnosticSupport {
    DiagnosticSupport::supporting([
        DiagnosticSetting::Temperature,
        DiagnosticSetting::MaxOutputTokens,
        DiagnosticSetting::Seed,
    ])
}

/// A real database of routes and profiles, so authorization decisions come from durable state.
struct World {
    db: Database,
    _dir: tempfile::TempDir,
    item_id: String,
    capture_id: String,
    stored: HashSet<String>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.db");
        let db = Database::open(path.to_str().unwrap(), Arc::new(SystemClock)).unwrap();
        let world = World {
            db,
            _dir: dir,
            item_id: Uuid::new_v4().to_string(),
            capture_id: Uuid::new_v4().to_string(),
            stored: HashSet::new(),
        };
        let conn = world.db.conn();
        conn.execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
             VALUES (?, 'default', 'personal', ?, ?)",
            (ROUTE, format!("[\"{OPENAI}\",\"{ANTHROPIC}\"]"), STAMP),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
             utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
             VALUES (?, 'Call the dentist', ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
            (&world.capture_id, STAMP, ROUTE, STAMP),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
             'not_applicable', ?, ?)",
            (&world.item_id, &world.capture_id, STAMP, STAMP),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, \
             authorized_destinations, created_at) \
             VALUES ('grant-1', ?, 'text_interpretation', ?, ?)",
            (ROUTE, format!("[\"{OPENAI}\",\"{ANTHROPIC}\"]"), STAMP),
        )
        .unwrap();
        world
    }

    fn authorize(&mut self, profile: &ProviderProfile) -> AuthorizationDecision {
        if self.stored.insert(profile.profile_version().to_string()) {
            let provider_type = match profile.protocol() {
                ProviderProtocol::OpenAi => "open_ai",
                ProviderProtocol::Anthropic => "anthropic",
                ProviderProtocol::SelfHosted => "self_hosted",
            };
            self.db
                .conn()
                .execute(
                    "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, \
                     endpoint, model, timeout_seconds, retry_policy, authorized_destinations, \
                     capabilities, created_at) VALUES (?, ?, ?, ?, 'model', 30, '{}', ?, '{}', ?)",
                    (
                        profile.profile_version(),
                        profile.profile_id(),
                        provider_type,
                        profile.endpoint(),
                        serde_json::to_string(profile.authorized_destinations()).unwrap(),
                        STAMP,
                    ),
                )
                .unwrap();
        }
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            0,
            Some(profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            stamp(),
        )
        .unwrap();
        authorize_job(self.db.conn(), &job_id).unwrap()
    }

    fn item_state(&self) -> (i64, String, i64) {
        self.db
            .conn()
            .query_row(
                "SELECT revision, processing_state, \
                 (SELECT COUNT(*) FROM reminders) FROM items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }
}

struct WorldAuthorizer {
    world: Rc<RefCell<World>>,
    profile_a: ProviderProfile,
    profile_b: ProviderProfile,
    asked: RefCell<Vec<ArmLabel>>,
}

impl ArmAuthorizer for WorldAuthorizer {
    fn authorize(&self, query: &AuthorizationQuery<'_>) -> AuthorizationDecision {
        self.asked.borrow_mut().push(query.arm);
        let profile = match query.arm {
            ArmLabel::A => &self.profile_a,
            ArmLabel::B => &self.profile_b,
        };
        assert_eq!(query.profile_version, profile.profile_version());
        self.world.borrow_mut().authorize(profile)
    }
}

#[derive(Default)]
struct Telemetry {
    in_flight: Cell<u32>,
    max_in_flight: Cell<u32>,
    order: RefCell<Vec<&'static str>>,
}

/// Wraps a scripted fake and watches what the executor does around each call.
struct Probe {
    name: &'static str,
    inner: FakeProvider,
    telemetry: Rc<Telemetry>,
    before: RefCell<Option<Box<dyn Fn()>>>,
    supported: bool,
}

impl Probe {
    fn new(
        name: &'static str,
        clock: &Arc<ManualClock>,
        telemetry: &Rc<Telemetry>,
        script: Vec<FakeStep>,
    ) -> Probe {
        Probe {
            name,
            inner: FakeProvider::new(clock.clone(), script).with_diagnostic_support(support_all()),
            telemetry: telemetry.clone(),
            before: RefCell::new(None),
            supported: true,
        }
    }

    fn before_call(&self, hook: impl Fn() + 'static) {
        *self.before.borrow_mut() = Some(Box::new(hook));
    }

    fn calls(&self) -> usize {
        self.inner.diagnostic_calls().len()
    }
}

impl ProviderAdapter for Probe {
    fn invoke(
        &self,
        call: &ohand_core::providers::contracts::AdapterCall<'_>,
    ) -> Result<Vec<u8>, TransportError> {
        self.inner.invoke(call)
    }

    fn diagnostic_support(&self) -> DiagnosticSupport {
        if self.supported {
            self.inner.diagnostic_support()
        } else {
            DiagnosticSupport::unsupported()
        }
    }

    fn invoke_diagnostic(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, TransportError> {
        let telemetry = &self.telemetry;
        telemetry.in_flight.set(telemetry.in_flight.get() + 1);
        telemetry
            .max_in_flight
            .set(telemetry.max_in_flight.get().max(telemetry.in_flight.get()));
        telemetry.order.borrow_mut().push(self.name);
        if let Some(hook) = self.before.borrow().as_ref() {
            hook();
        }
        let result = self.inner.invoke_diagnostic(call);
        telemetry.in_flight.set(telemetry.in_flight.get() - 1);
        result
    }

    fn invoke_diagnostic_observed(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, DiagnosticTransportFailure> {
        self.invoke_diagnostic(call).map_err(Into::into)
    }
}

struct Harness {
    dir: tempfile::TempDir,
    world: Rc<RefCell<World>>,
    profile_a: ProviderProfile,
    profile_b: ProviderProfile,
    authorizer: WorldAuthorizer,
    clock: Arc<ManualClock>,
    telemetry: Rc<Telemetry>,
    cancel: CancelToken,
}

impl Harness {
    fn new() -> Harness {
        let world = Rc::new(RefCell::new(World::new()));
        let profile_a = hosted_profile("openai-arm", ProviderProtocol::OpenAi, "model-a", OPENAI);
        let profile_b = hosted_profile(
            "anthropic-arm",
            ProviderProtocol::Anthropic,
            "model-b",
            ANTHROPIC,
        );
        Harness {
            dir: tempfile::tempdir().unwrap(),
            authorizer: WorldAuthorizer {
                world: world.clone(),
                profile_a: profile_a.clone(),
                profile_b: profile_b.clone(),
                asked: RefCell::new(Vec::new()),
            },
            world,
            profile_a,
            profile_b,
            clock: Arc::new(ManualClock::new()),
            telemetry: Rc::new(Telemetry::default()),
            cancel: CancelToken::default(),
        }
    }

    fn journal_path(&self) -> PathBuf {
        self.dir.path().join("run.ndjson")
    }

    fn experiment(&self) -> Experiment {
        Experiment::new(
            "corpus-1",
            "context-1",
            content_version(M1_INSTRUCTION_TEXT),
            self.profile_a.profile_id(),
            self.profile_a.profile_version(),
            self.profile_b.profile_id(),
            self.profile_b.profile_version(),
            "build-1",
        )
        .unwrap()
    }

    fn open(&self) -> Executor {
        Executor::open(self.journal_path(), self.experiment()).unwrap()
    }

    fn probe(&self, name: &'static str, script: Vec<FakeStep>) -> Probe {
        Probe::new(name, &self.clock, &self.telemetry, script)
    }

    fn source(
        &self,
        text: &str,
        instructions: &str,
        requested: RequestedSettings,
    ) -> FrozenComparisonSource {
        FrozenComparisonSource::new(
            self.world.borrow().capture_id.clone(),
            0,
            TextBasis::Original { item_revision: 0 },
            text,
            Uuid::new_v4().to_string(),
            time_context(),
            ROUTE,
            instructions,
            content_version(instructions),
            requested,
        )
        .unwrap()
    }

    fn cases(&self, count: usize) -> Vec<PlannedCase> {
        (0..count)
            .map(|index| {
                let text = format!("Call the dentist about visit {index}");
                PlannedCase::new(
                    format!("case-{index}"),
                    text.clone(),
                    self.source(&text, M1_INSTRUCTION_TEXT, settings()),
                )
                .unwrap()
            })
            .collect()
    }

    fn run(
        &self,
        executor: &mut Executor,
        cases: &[PlannedCase],
        arm_a: &Probe,
        arm_b: &Probe,
        limits: ExecutorLimits,
        reruns: &[SideKey],
    ) -> Result<RunReport, ExecutorError> {
        executor.run(RunRequest {
            cases,
            arm_a: ArmBinding {
                profile: &self.profile_a,
                adapter: arm_a,
                authorizer: &self.authorizer,
            },
            arm_b: ArmBinding {
                profile: &self.profile_b,
                adapter: arm_b,
                authorizer: &self.authorizer,
            },
            controls: RunControls {
                clock: &*self.clock,
                cancel: &self.cancel,
                dispatch_limits: DispatchLimits::default(),
            },
            limits,
            reruns,
        })
    }

    fn revoke(&self, profile: &ProviderProfile) {
        let mut world = self.world.borrow_mut();
        world.authorize(profile);
        revoke_profile(&mut world.db, profile.profile_version(), stamp()).unwrap();
    }
}

fn limits(attempts_per_side: u32, request_cap: u32) -> ExecutorLimits {
    ExecutorLimits::new(attempts_per_side, request_cap).unwrap()
}

fn ok(body: &str) -> FakeStep {
    FakeStep::respond(body)
}

fn oks(body: &str, count: usize) -> Vec<FakeStep> {
    (0..count).map(|_| ok(body)).collect()
}

fn side(report: &RunReport, case: usize, arm: ArmLabel) -> &SideReport {
    report.cases[case]
        .sides
        .iter()
        .find(|side| side.arm == arm)
        .unwrap()
}

fn journal(path: &Path) -> JournalReader {
    JournalReader::open(path).unwrap()
}

fn raw(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn envelope_of(attempt: &crate::records::Attempt) -> serde_json::Value {
    attempt.provider_output.clone().expect("executor envelope")
}

#[test]
fn arms_run_one_at_a_time_in_counterbalanced_recorded_order() {
    let harness = Harness::new();
    let cases = harness.cases(4);
    let arm_a = harness.probe("a", oks(BODY_A, 4));
    let arm_b = harness.probe("b", oks(BODY_B, 4));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 20), &[])
        .unwrap();

    assert_eq!(harness.telemetry.max_in_flight.get(), 1);
    assert_eq!(
        *harness.telemetry.order.borrow(),
        ["a", "b", "b", "a", "a", "b", "b", "a"]
    );
    for (index, case) in report.cases.iter().enumerate() {
        let expected = if index % 2 == 0 {
            [ArmLabel::A, ArmLabel::B]
        } else {
            [ArmLabel::B, ArmLabel::A]
        };
        assert_eq!(case.arm_order, expected);
        assert!(case
            .sides
            .iter()
            .all(|side| side.kind == SideKind::Completed));
        assert_eq!(case.configuration, ConfigurationStatus::Verified);
    }
    let journal = journal(&harness.journal_path());
    for attempt in journal.attempts() {
        let envelope = envelope_of(attempt);
        let order: Vec<&str> = envelope["arm_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arm| arm.as_str().unwrap())
            .collect();
        assert!(order == ["a", "b"] || order == ["b", "a"]);
    }
    assert_eq!(report.reserved_requests, 8);
}

#[test]
fn arms_share_the_frozen_case_but_never_each_others_responses() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let arm_a = harness.probe("a", oks(BODY_A, 2));
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();

    harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 10), &[])
        .unwrap();

    let calls_a = arm_a.inner.diagnostic_calls();
    let calls_b = arm_b.inner.diagnostic_calls();
    assert_eq!(calls_a.len(), 2);
    assert_eq!(calls_b.len(), 2);
    for (a, b) in calls_a.iter().zip(&calls_b) {
        assert_eq!(a.instructions, b.instructions);
        assert_eq!(a.context, b.context);
        assert_eq!(a.requested_settings, b.requested_settings);
        assert!(!a.wire_request.contains("CANARY"));
        assert!(!b.wire_request.contains("CANARY"));
    }
}

#[test]
fn reservation_is_journaled_before_dispatch_and_authorization_precedes_every_attempt() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe(
        "a",
        vec![FakeStep::fail(TransportError::Unavailable), ok(BODY_A)],
    );
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let path = harness.journal_path();
    let reservations_seen = Rc::new(RefCell::new(Vec::new()));
    {
        let (path, seen) = (path.clone(), reservations_seen.clone());
        arm_a.before_call(move || {
            let journal = journal(&path);
            let open: Vec<_> = journal
                .attempts()
                .into_iter()
                .filter(|attempt| attempt.state == AttemptState::Unknown)
                .map(envelope_of)
                .collect();
            seen.borrow_mut().push(open);
        });
    }
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(2, 10), &[])
        .unwrap();

    let seen = reservations_seen.borrow();
    assert_eq!(seen.len(), 2);
    for open in seen.iter() {
        assert_eq!(open.len(), 1, "the in-flight request is already reserved");
        assert_eq!(open[0]["phase"], "reserved");
        assert_eq!(open[0]["dispatched"], true);
    }
    assert_eq!(seen[1][0]["side_attempt"], 2);
    assert_eq!(side(&report, 0, ArmLabel::A).dispatched_attempts, 2);
    let asked = harness.authorizer.asked.borrow();
    let a_asks = asked.iter().filter(|arm| **arm == ArmLabel::A).count();
    assert_eq!(a_asks, 1 + 2, "once to derive, once before each attempt");
    assert_eq!(asked.iter().filter(|arm| **arm == ArmLabel::B).count(), 2);
}

#[test]
fn revocation_during_a_run_stops_the_arm_without_spending_allowance() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    {
        let (world, profile_b) = (harness.world.clone(), harness.profile_b.clone());
        arm_a.before_call(move || {
            let mut world = world.borrow_mut();
            revoke_profile(&mut world.db, profile_b.profile_version(), stamp()).unwrap();
        });
    }
    harness.world.borrow_mut().authorize(&harness.profile_b);
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    assert_eq!(arm_b.calls(), 0);
    let skipped = side(&report, 0, ArmLabel::B);
    assert_eq!(skipped.kind, SideKind::Skipped);
    assert_eq!(skipped.class.as_deref(), Some("not_authorized"));
    assert_eq!(skipped.dispatched_attempts, 0);
    assert_eq!(report.reserved_requests, 1);
    assert_eq!(side(&report, 0, ArmLabel::A).kind, SideKind::Completed);
}

#[test]
fn a_profile_revoked_before_the_run_makes_the_case_ineligible() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    harness.revoke(&harness.profile_a);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 0);
    assert_eq!(report.reserved_requests, 0);
    for arm in [ArmLabel::A, ArmLabel::B] {
        let skipped = side(&report, 0, arm);
        assert_eq!(skipped.kind, SideKind::Skipped);
        assert_eq!(skipped.class.as_deref(), Some("not_eligible"));
    }
    assert!(matches!(
        report.cases[0].configuration,
        ConfigurationStatus::Unverified { .. }
    ));
}

#[test]
fn an_adapter_without_diagnostic_support_is_not_sent_anything() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let mut arm_b = harness.probe("b", oks(BODY_B, 1));
    arm_b.supported = false;
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 0);
    assert_eq!(
        side(&report, 0, ArmLabel::A).class.as_deref(),
        Some("not_eligible")
    );
}

#[test]
fn the_request_cap_bounds_reservations_including_retries() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let arm_a = harness.probe(
        "a",
        vec![
            FakeStep::fail(TransportError::Unavailable),
            ok(BODY_A),
            ok(BODY_A),
        ],
    );
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(2, 3), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 3);
    assert_eq!(report.reserved_requests, 3);
    assert!(report.budget_exhausted);
    assert!(!report.cancelled);
    assert_eq!(side(&report, 0, ArmLabel::A).dispatched_attempts, 2);
    for arm in [ArmLabel::A, ArmLabel::B] {
        let left = side(&report, 1, arm);
        assert_eq!(left.kind, SideKind::Skipped);
        assert_eq!(left.class.as_deref(), Some("budget_exhausted"));
        assert_eq!(left.dispatched_attempts, 0);
    }
}

#[test]
fn a_zero_cap_sends_nothing() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(3, 0), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 0);
    assert!(report.budget_exhausted);
}

#[test]
fn limits_are_validated() {
    assert!(ExecutorLimits::new(0, 5).is_err());
    assert!(ExecutorLimits::new(MAX_ATTEMPTS_PER_SIDE + 1, 5).is_err());
    assert!(ExecutorLimits::new(1, MAX_REQUEST_CAP + 1).is_err());
    assert!(ExecutorLimits::new(MAX_ATTEMPTS_PER_SIDE, MAX_REQUEST_CAP).is_ok());
    assert_eq!(MAX_CONCURRENT_REQUESTS, 1);
}

#[test]
fn retries_are_bounded_and_limited_to_retriable_failures() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe(
        "a",
        (0..5)
            .map(|_| FakeStep::fail(TransportError::Unavailable))
            .collect(),
    );
    let arm_b = harness.probe(
        "b",
        vec![FakeStep::fail(TransportError::Rejected), ok(BODY_B)],
    );
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(3, 20), &[])
        .unwrap();

    assert_eq!(arm_a.calls(), 3);
    let failed = side(&report, 0, ArmLabel::A);
    assert_eq!(failed.kind, SideKind::Failed);
    assert_eq!(failed.class.as_deref(), Some("unavailable"));
    assert_eq!(arm_b.calls(), 1, "a rejection is not retried");
    assert_eq!(side(&report, 0, ArmLabel::B).kind, SideKind::Failed);

    let journal = journal(&harness.journal_path());
    let chain: Vec<_> = journal
        .attempts()
        .into_iter()
        .filter(|attempt| attempt.arm == "a" && attempt.state == AttemptState::Failed)
        .collect();
    assert_eq!(chain.len(), 3);
    assert!(envelope_of(chain[0]).get("retry_of").is_none());
    assert_eq!(envelope_of(chain[1])["retry_of"], chain[0].id);
    assert_eq!(envelope_of(chain[2])["retry_of"], chain[1].id);
}

#[test]
fn a_timeout_is_reported_as_timed_out_with_the_unknown_outcome_explained() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", vec![FakeStep::fail(TransportError::Timeout)]);
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    let timed_out = side(&report, 0, ArmLabel::A);
    assert_eq!(timed_out.kind, SideKind::TimedOut);
    assert!(timed_out.reason.as_deref().unwrap().contains("unknown"));
    assert_eq!(side(&report, 0, ArmLabel::B).kind, SideKind::Completed);
}

#[test]
fn cancellation_before_the_run_dispatches_nothing() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    harness.cancel.cancel();
    let arm_a = harness.probe("a", oks(BODY_A, 2));
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(2, 10), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 0);
    assert!(report.cancelled);
    assert_eq!(report.reserved_requests, 0);
    for case in &report.cases {
        for side in &case.sides {
            assert_eq!(side.kind, SideKind::Cancelled);
        }
    }
}

#[test]
fn cancellation_mid_run_stops_every_remaining_side() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let arm_a = harness.probe("a", vec![FakeStep::cancel_then_respond(BODY_A), ok(BODY_A)]);
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(3, 10), &[])
        .unwrap();

    assert_eq!(arm_a.calls() + arm_b.calls(), 1);
    assert!(report.cancelled);
    assert_eq!(report.reserved_requests, 1);
    assert_eq!(side(&report, 0, ArmLabel::A).kind, SideKind::Cancelled);
    assert_eq!(side(&report, 0, ArmLabel::B).kind, SideKind::Cancelled);
    assert_eq!(side(&report, 1, ArmLabel::A).kind, SideKind::Cancelled);
}

#[test]
fn a_run_writes_only_its_journal_and_leaves_items_and_reminders_alone() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let before = harness.world.borrow().item_state();
    let arm_a = harness.probe("a", oks(BODY_A, 2));
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 10), &[])
        .unwrap();

    assert_eq!(harness.world.borrow().item_state(), before);
    let files: Vec<_> = std::fs::read_dir(harness.dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(files, [std::ffi::OsString::from("run.ndjson")]);
    let output = side(&report, 0, ArmLabel::A).output.as_ref().unwrap();
    assert_eq!(output["operation"]["kind"], "annotate");
}

#[test]
fn resume_keeps_completed_sides_and_sends_only_the_missing_ones() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let first_a = harness.probe("a", oks(BODY_A, 2));
    let first_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();
    let first = harness
        .run(&mut executor, &cases, &first_a, &first_b, limits(1, 3), &[])
        .unwrap();
    assert!(first.budget_exhausted);
    let completed_before = side(&first, 0, ArmLabel::A).attempt_id.clone();
    assert_eq!(side(&first, 1, ArmLabel::A).kind, SideKind::Skipped);
    drop(executor);

    let second_a = harness.probe("a", oks(BODY_A, 2));
    let second_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();
    let second = harness
        .run(
            &mut executor,
            &cases,
            &second_a,
            &second_b,
            limits(1, 10),
            &[],
        )
        .unwrap();

    assert_eq!(second.run, 2);
    assert_eq!(second_a.calls() + second_b.calls(), 1);
    assert_eq!(side(&second, 0, ArmLabel::A).attempt_id, completed_before);
    assert_eq!(side(&second, 0, ArmLabel::A).dispatched_this_run, 0);
    assert_eq!(side(&second, 1, ArmLabel::A).kind, SideKind::Completed);
    assert_eq!(second.cases[1].arm_order, first.cases[1].arm_order);
    assert_eq!(second.reserved_requests, 1);
}

#[test]
fn a_crash_after_reservation_is_unknown_and_never_dispatched_again_silently() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    arm_b.before_call(|| panic!("simulated crash after the request was sent"));
    let mut executor = harness.open();
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        let _ = harness.run(&mut executor, &cases, &arm_a, &arm_b, limits(3, 10), &[]);
    }));
    assert!(crashed.is_err());
    drop(executor);

    let fresh_a = harness.probe("a", oks(BODY_A, 1));
    let fresh_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();
    let resumed = harness
        .run(
            &mut executor,
            &cases,
            &fresh_a,
            &fresh_b,
            limits(3, 10),
            &[],
        )
        .unwrap();

    assert_eq!(
        fresh_a.calls() + fresh_b.calls(),
        0,
        "nothing is sent again"
    );
    assert_eq!(resumed.reserved_requests, 0);
    let unknown = side(&resumed, 0, ArmLabel::B);
    assert_eq!(unknown.kind, SideKind::Unknown);
    assert!(unknown.reason.as_deref().unwrap().contains("unknown"));
    assert_eq!(unknown.dispatched_attempts, 1);
    assert_eq!(side(&resumed, 0, ArmLabel::A).kind, SideKind::Completed);

    let unknown_id = unknown.attempt_id.clone().unwrap();
    let rerun_key = [SideKey {
        case_id: "case-0".to_string(),
        arm: ArmLabel::B,
    }];
    let rerun = harness
        .run(
            &mut executor,
            &cases,
            &fresh_a,
            &fresh_b,
            limits(1, 1),
            &rerun_key,
        )
        .unwrap();
    assert_eq!(fresh_b.calls(), 1);
    let rerun_side = side(&rerun, 0, ArmLabel::B);
    assert_eq!(rerun_side.kind, SideKind::Completed);
    assert_eq!(rerun_side.rerun_of.as_deref(), Some(unknown_id.as_str()));
    assert_eq!(rerun_side.dispatched_attempts, 2);
    assert_eq!(rerun_side.dispatched_this_run, 1);
}

#[test]
fn an_explicit_rerun_spends_a_new_allowance_and_keeps_the_original_attempt() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();
    let first = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();
    let original = side(&first, 0, ArmLabel::A).attempt_id.clone().unwrap();
    let key = [SideKey {
        case_id: "case-0".to_string(),
        arm: ArmLabel::A,
    }];

    let blocked_a = harness.probe("a", oks(BODY_A, 1));
    let blocked = harness
        .run(
            &mut executor,
            &cases,
            &blocked_a,
            &arm_b,
            limits(1, 0),
            &key,
        )
        .unwrap();
    assert_eq!(blocked_a.calls(), 0);
    assert!(blocked.budget_exhausted);
    assert_eq!(side(&blocked, 0, ArmLabel::A).kind, SideKind::Completed);
    assert_eq!(
        side(&blocked, 0, ArmLabel::A).attempt_id.as_deref(),
        Some(original.as_str())
    );

    let rerun_a = harness.probe("a", oks(BODY_A, 1));
    let rerun = harness
        .run(&mut executor, &cases, &rerun_a, &arm_b, limits(1, 1), &key)
        .unwrap();
    assert_eq!(rerun_a.calls(), 1);
    assert_eq!(arm_b.calls(), 1, "the other side is not sent again");
    assert_eq!(rerun.reserved_requests, 1);
    let rerun_side = side(&rerun, 0, ArmLabel::A);
    assert_eq!(rerun_side.rerun_of.as_deref(), Some(original.as_str()));
    assert_ne!(rerun_side.attempt_id.as_deref(), Some(original.as_str()));

    let journal = journal(&harness.journal_path());
    let kept = journal
        .attempts()
        .into_iter()
        .rfind(|attempt| attempt.id == original)
        .unwrap();
    assert_eq!(kept.state, AttemptState::Completed);
}

#[test]
fn rerun_targets_must_name_a_planned_side_that_was_attempted() {
    let harness = Harness::new();
    let cases = harness.cases(2);
    let arm_a = harness.probe("a", oks(BODY_A, 2));
    let arm_b = harness.probe("b", oks(BODY_B, 2));
    let mut executor = harness.open();
    harness
        .run(
            &mut executor,
            &cases[..1],
            &arm_a,
            &arm_b,
            limits(1, 5),
            &[],
        )
        .unwrap();
    let before = raw(&harness.journal_path());

    let unknown = [SideKey {
        case_id: "case-missing".to_string(),
        arm: ArmLabel::A,
    }];
    assert!(matches!(
        harness.run(
            &mut executor,
            &cases,
            &arm_a,
            &arm_b,
            limits(1, 5),
            &unknown
        ),
        Err(ExecutorError::UnknownRerunTarget { .. })
    ));
    let never_attempted = [SideKey {
        case_id: "case-1".to_string(),
        arm: ArmLabel::A,
    }];
    assert!(matches!(
        harness.run(
            &mut executor,
            &cases,
            &arm_a,
            &arm_b,
            limits(1, 5),
            &never_attempted
        ),
        Err(ExecutorError::NothingToRerun { .. })
    ));
    assert_eq!(raw(&harness.journal_path()), before);
}

#[test]
fn results_of_a_changed_experiment_are_never_merged() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();
    harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();
    drop(executor);
    let before = raw(&harness.journal_path());

    let mut changed = harness.experiment();
    changed.profile_b_version = "another-version".to_string();
    changed.corpus_version = "corpus-2".to_string();
    match Executor::open(harness.journal_path(), changed) {
        Err(ExecutorError::ConfigurationChanged(ConfigurationChange::Experiment { fields })) => {
            assert_eq!(fields, ["corpus_version", "profile_b_version"]);
        }
        other => panic!(
            "expected a configuration change, got {:?}",
            other.map(|_| ())
        ),
    }
    assert_eq!(raw(&harness.journal_path()), before);
}

#[test]
fn changed_requests_cases_instructions_and_bindings_are_refused_without_writing() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 3));
    let arm_b = harness.probe("b", oks(BODY_B, 3));
    let mut executor = harness.open();
    harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();
    let before = raw(&harness.journal_path());
    let key = [SideKey {
        case_id: "case-0".to_string(),
        arm: ArmLabel::A,
    }];

    let other_settings = settings().with_seed(8);
    let text = "Call the dentist about visit 0";
    let changed_settings = [PlannedCase::new(
        "case-0",
        text,
        harness.source(text, M1_INSTRUCTION_TEXT, other_settings),
    )
    .unwrap()];
    assert_eq!(
        harness
            .run(
                &mut executor,
                &changed_settings,
                &arm_a,
                &arm_b,
                limits(1, 5),
                &key
            )
            .map(|_| ()),
        Err(ExecutorError::ConfigurationChanged(
            ConfigurationChange::Request {
                case_id: "case-0".to_string()
            }
        ))
    );

    let other_text = "Call the dentist about something else";
    let changed_content = [PlannedCase::new(
        "case-0",
        other_text,
        harness.source(other_text, M1_INSTRUCTION_TEXT, settings()),
    )
    .unwrap()];
    assert_eq!(
        harness
            .run(
                &mut executor,
                &changed_content,
                &arm_a,
                &arm_b,
                limits(1, 5),
                &key
            )
            .map(|_| ()),
        Err(ExecutorError::ConfigurationChanged(
            ConfigurationChange::CaseContent {
                case_id: "case-0".to_string()
            }
        ))
    );

    let changed_instructions = [PlannedCase::new(
        "case-0",
        text,
        harness.source(text, "Interpret the capture differently.", settings()),
    )
    .unwrap()];
    assert_eq!(
        harness
            .run(
                &mut executor,
                &changed_instructions,
                &arm_a,
                &arm_b,
                limits(1, 5),
                &key
            )
            .map(|_| ()),
        Err(ExecutorError::InstructionVersionMismatch)
    );

    let swapped = executor.run(RunRequest {
        cases: &cases,
        arm_a: ArmBinding {
            profile: &harness.profile_b,
            adapter: &arm_b,
            authorizer: &harness.authorizer,
        },
        arm_b: ArmBinding {
            profile: &harness.profile_a,
            adapter: &arm_a,
            authorizer: &harness.authorizer,
        },
        controls: RunControls {
            clock: &*harness.clock,
            cancel: &harness.cancel,
            dispatch_limits: DispatchLimits::default(),
        },
        limits: limits(1, 5),
        reruns: &key,
    });
    assert_eq!(
        swapped.map(|_| ()),
        Err(ExecutorError::BindingMismatch("arm a"))
    );

    let duplicated = [cases[0].clone(), cases[0].clone()];
    assert_eq!(
        harness
            .run(
                &mut executor,
                &duplicated,
                &arm_a,
                &arm_b,
                limits(1, 5),
                &[]
            )
            .map(|_| ()),
        Err(ExecutorError::DuplicateCase("case-0".to_string()))
    );

    assert_eq!(
        arm_a.calls() + arm_b.calls(),
        2,
        "no refused run sent anything"
    );
    assert_eq!(raw(&harness.journal_path()), before);
}

#[test]
fn a_truncated_journal_tail_is_repaired_and_completed_sides_survive() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let arm_a = harness.probe("a", oks(BODY_A, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();
    harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();
    drop(executor);
    let mut damaged = raw(&harness.journal_path());
    damaged.extend_from_slice(br#"{"kind":"attempt","id":"half-writ"#);
    std::fs::write(harness.journal_path(), damaged).unwrap();

    let fresh_a = harness.probe("a", oks(BODY_A, 1));
    let fresh_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();
    let report = harness
        .run(&mut executor, &cases, &fresh_a, &fresh_b, limits(1, 5), &[])
        .unwrap();

    assert_eq!(fresh_a.calls() + fresh_b.calls(), 0);
    assert!(report.cases[0]
        .sides
        .iter()
        .all(|side| side.kind == SideKind::Completed));
}

#[test]
fn usage_is_recorded_only_when_the_provider_reported_it() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let reported = CallObservations {
        usage: Some(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(4),
        }),
        ..CallObservations::default()
    };
    let arm_a = harness.probe("a", vec![ok(BODY_A).with_observations(reported)]);
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();

    harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    let journal = journal(&harness.journal_path());
    let usage_of = |arm: &str| {
        journal
            .attempts()
            .into_iter()
            .find(|attempt| attempt.arm == arm && attempt.state == AttemptState::Completed)
            .unwrap()
            .usage_tokens
            .clone()
    };
    let reported = usage_of("a").expect("usage was reported");
    assert_eq!(
        (reported.input_tokens, reported.output_tokens),
        (Some(10), Some(4))
    );
    assert!(
        usage_of("b").is_none(),
        "unreported usage is not recorded as zero"
    );
}

#[test]
fn output_the_journal_guard_rejects_becomes_a_failed_side_without_the_content() {
    let harness = Harness::new();
    let cases = harness.cases(1);
    let leaking =
        r#"{"operation":{"kind":"annotate","note":"see https://private.internal.test/path"}}"#;
    let arm_a = harness.probe("a", oks(leaking, 1));
    let arm_b = harness.probe("b", oks(BODY_B, 1));
    let mut executor = harness.open();

    let report = harness
        .run(&mut executor, &cases, &arm_a, &arm_b, limits(1, 5), &[])
        .unwrap();

    let rejected = side(&report, 0, ArmLabel::A);
    assert_eq!(rejected.kind, SideKind::Failed);
    assert_eq!(rejected.class.as_deref(), Some("output_rejected"));
    assert_eq!(side(&report, 0, ArmLabel::B).kind, SideKind::Completed);
    let text = String::from_utf8(raw(&harness.journal_path())).unwrap();
    assert!(!text.contains("private.internal.test"));
}
