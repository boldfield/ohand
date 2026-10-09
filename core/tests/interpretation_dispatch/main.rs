//! Integration fixtures for the interpretation dispatcher (I06). Every test drives a claimed job
//! from a raw saved capture in a migrated, file-backed SQLite store through the dispatcher, the
//! real Anthropic/OpenAI adapters (with scripted transports) and the I05 apply boundary, and
//! asserts on durable state re-read through a fresh connection.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::status::{ItemStatus, ProcessingJobStatus, ProcessingState};
use ohand_core::interpretation::apply::{ApplyError, ApplyOutcome};
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, DispatchError, DispatchOutcome, InterpretationDispatcher, InterpretationRoute,
    RetireReason, WaitReason,
};
use ohand_core::jobs::queue::{claim_job_with_lease, enqueue_job, Job};
use ohand_core::privacy::routing::{DenialReason, JOB_TYPE_INTERPRET};
use ohand_core::providers::anthropic::fake::{FakeAnthropicStep, FakeAnthropicTransport};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, HttpRequest, INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    CancelToken, CapabilityMetadata, Clock, FailureKind, ManualClock, ProviderCapability,
    ProviderProfile, ProviderProfileBuilder, ProviderProtocol, RetryPolicy, StructuredOutputMode,
    TransportError,
};
use ohand_core::providers::openai::{HttpTransport, OpenAiAdapter};
use ohand_core::retrieval::index::search_source_direct;
use ohand_core::store::events::ItemScope;
use ohand_core::store::schema::{Clock as StoreClock, Database};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const NOW: &str = "2026-01-15T10:30:00Z";
const LEASE_SECONDS: i64 = 60;
const ROUTE_ID: &str = "route-1";
const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const OPENAI_ORIGIN: &str = "https://api.openai.com";
const ANTHROPIC_MODEL: &str = "synthetic-anthropic-model";
const OPENAI_MODEL: &str = "synthetic-openai-model";
const FREE_FORM: &str = "call the roofer about the leak";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .with_timezone(&Utc)
}

struct FixedClock;

impl StoreClock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        now()
    }
}

fn open(path: &str) -> Database {
    Database::open(path, Arc::new(FixedClock)).unwrap()
}

// ---- Profiles and providers ----

fn text_capability(mode: StructuredOutputMode) -> CapabilityMetadata {
    CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "dispatcher-fixtures:interpretation_dispatch",
    )
    .with_input_size_limit(500)
    .with_structured_output(mode)
}

fn anthropic_profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-anthropic",
        ProviderProtocol::Anthropic,
        ANTHROPIC_MODEL,
    )
    .credential_ref("credential-ref/anthropic-primary")
    .authorized_destination(ANTHROPIC_ORIGIN)
    .capability(text_capability(StructuredOutputMode::JsonSchema))
    .build()
    .unwrap()
}

fn openai_profile() -> ProviderProfile {
    ProviderProfileBuilder::new("synthetic-openai", ProviderProtocol::OpenAi, OPENAI_MODEL)
        .credential_ref("credential-ref/openai-primary")
        .authorized_destination(OPENAI_ORIGIN)
        .capability(text_capability(StructuredOutputMode::JsonObject))
        .build()
        .unwrap()
}

type ScriptedReply = Result<(u16, Vec<u8>), TransportError>;

#[derive(Default)]
struct OpenAiShared {
    replies: Mutex<VecDeque<ScriptedReply>>,
    bodies: Mutex<Vec<Vec<u8>>>,
}

#[derive(Clone, Default)]
struct OpenAiStub {
    shared: Arc<OpenAiShared>,
}

impl OpenAiStub {
    fn replying(proposal: Value) -> OpenAiStub {
        let stub = OpenAiStub::default();
        stub.push(Ok((200, completion(&proposal.to_string()))));
        stub
    }

    fn push(&self, reply: ScriptedReply) {
        self.shared.replies.lock().unwrap().push_back(reply);
    }

    fn call_count(&self) -> usize {
        self.shared.bodies.lock().unwrap().len()
    }
}

impl HttpTransport for OpenAiStub {
    fn post(
        &self,
        _endpoint: &str,
        _credential_ref: &str,
        _headers: &[(&str, &str)],
        body: Vec<u8>,
        _deadline_ms: u64,
        _cancel: &CancelToken,
        _clock: &dyn Clock,
        _max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        self.shared.bodies.lock().unwrap().push(body);
        self.shared
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted OpenAI reply")
    }
}

fn completion(content: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "chatcmpl-synthetic",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content, "refusal": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    }))
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

struct Providers {
    clock: Arc<ManualClock>,
    anthropic: Arc<FakeAnthropicTransport>,
    openai: OpenAiStub,
    registry: AdapterRegistry,
}

impl Providers {
    fn new(anthropic_steps: Vec<FakeAnthropicStep>, openai: OpenAiStub) -> Providers {
        let clock = Arc::new(ManualClock::new());
        let anthropic = Arc::new(FakeAnthropicTransport::new(clock.clone(), anthropic_steps));
        let registry = AdapterRegistry::new()
            .with_adapter(
                ProviderProtocol::Anthropic,
                AnthropicAdapter::new(anthropic.clone(), AnthropicSettings::default()),
            )
            .with_adapter(ProviderProtocol::OpenAi, OpenAiAdapter::new(openai.clone()));
        Providers {
            clock,
            anthropic,
            openai,
            registry,
        }
    }

    fn anthropic(steps: Vec<FakeAnthropicStep>) -> Providers {
        Providers::new(steps, OpenAiStub::default())
    }

    fn without_adapters() -> Providers {
        let mut providers = Providers::anthropic(vec![]);
        providers.registry = AdapterRegistry::new();
        providers
    }

    fn calls_to_anthropic(&self) -> Vec<HttpRequest> {
        self.anthropic.calls()
    }

    fn run(&self, fixture: &mut Fixture, job: &Job, at: DateTime<Utc>) -> DispatchOutcome {
        self.run_with(fixture, job, &CancelToken::new(), at)
    }

    fn run_with(
        &self,
        fixture: &mut Fixture,
        job: &Job,
        cancel: &CancelToken,
        at: DateTime<Utc>,
    ) -> DispatchOutcome {
        self.try_run(fixture, job, cancel, at)
            .expect("dispatcher runs the job")
    }

    fn try_run(
        &self,
        fixture: &mut Fixture,
        job: &Job,
        cancel: &CancelToken,
        at: DateTime<Utc>,
    ) -> Result<DispatchOutcome, DispatchError> {
        InterpretationDispatcher::new(&self.registry, self.clock.as_ref()).run_job(
            &mut fixture.db,
            job,
            cancel,
            at,
        )
    }
}

// ---- Fixture ----

struct Fixture {
    db: Database,
    path: String,
    item_id: String,
    capture_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Durable {
    capture_text: String,
    item_type: Option<String>,
    processing_state: String,
    scope: String,
    lifecycle_state: String,
    proposals: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct JobRow {
    status: String,
    failure_reason: Option<String>,
    attempt_count: i32,
    next_attempt_at: Option<String>,
}

impl Fixture {
    fn new(capture_text: &str) -> Fixture {
        let path = format!(
            "{}/test_interpretation_dispatch_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let db = open(&path);
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        db.conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
                rusqlite::params![capture_id, capture_text, NOW, ROUTE_ID, NOW],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, NOW, NOW],
            )
            .unwrap();
        Fixture {
            db,
            path,
            item_id,
            capture_id,
        }
    }

    /// Route `ROUTE_ID` may process text interpretation at `origins`.
    fn authorize(&self, origins: &[&str]) {
        let origins = serde_json::to_string(origins).unwrap();
        self.db
            .conn()
            .execute(
                "INSERT OR IGNORE INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
                 VALUES (?, 'private notes', 'personal', ?, ?)",
                rusqlite::params![ROUTE_ID, origins, NOW],
            )
            .unwrap();
        self.db
            .conn()
            .execute(
                "INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, created_at) \
                 VALUES (?, ?, 'text_interpretation', ?, ?)",
                rusqlite::params![Uuid::new_v4().to_string(), ROUTE_ID, origins, NOW],
            )
            .unwrap();
    }

    fn install(&self, profile: &ProviderProfile) {
        let record = serde_json::to_value(profile).unwrap();
        self.db
            .conn()
            .execute(
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
                    NOW,
                ],
            )
            .unwrap();
    }

    /// A profile installed and authorized for the route, ready for a pinned job.
    fn set_up_provider(&self, profile: &ProviderProfile, origin: &str) {
        self.install(profile);
        self.authorize(&[origin]);
    }

    fn enqueue(
        &mut self,
        profile: Option<&ProviderProfile>,
        source_revision: i32,
        at: DateTime<Utc>,
    ) -> String {
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            source_revision,
            profile.map(|profile| profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            at,
        )
        .unwrap();
        job_id
    }

    fn claim(&mut self, at: DateTime<Utc>) -> Job {
        claim_job_with_lease(&mut self.db, Duration::seconds(LEASE_SECONDS), at)
            .unwrap()
            .expect("a job is eligible")
    }

    fn enqueue_and_claim(&mut self, profile: Option<&ProviderProfile>) -> Job {
        let job_id = self.enqueue(profile, 0, now());
        let job = self.claim(now());
        assert_eq!(job.job_id, job_id);
        job
    }

    fn durable(&self) -> Durable {
        let db = open(&self.path);
        let conn = db.conn();
        let (item_type, processing_state, lifecycle_state, scope, capture_text): (
            Option<String>,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT i.item_type, i.processing_state, i.lifecycle_state, c.item_scope, c.text \
                 FROM items i JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?",
                [&self.item_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        let proposals: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM proposals WHERE item_id = ?",
                [&self.item_id],
                |row| row.get(0),
            )
            .unwrap();
        Durable {
            capture_text,
            item_type,
            processing_state,
            scope,
            lifecycle_state,
            proposals,
        }
    }

    fn job_row(&self, job_id: &str) -> JobRow {
        open(&self.path)
            .conn()
            .query_row(
                "SELECT status, failure_reason, attempt_count, next_attempt_at FROM jobs WHERE job_id = ?",
                [job_id],
                |row| {
                    Ok(JobRow {
                        status: row.get(0)?,
                        failure_reason: row.get(1)?,
                        attempt_count: row.get(2)?,
                        next_attempt_at: row.get(3)?,
                    })
                },
            )
            .unwrap()
    }

    fn status(&mut self) -> ItemStatus {
        let tx = self.db.immediate_transaction().unwrap();
        let status = ItemStatus::load(&tx, &self.item_id).unwrap().unwrap();
        tx.commit().unwrap();
        status
    }

    fn reminder(&self) -> Option<(String, String, Option<String>)> {
        use rusqlite::OptionalExtension;
        open(&self.path)
            .conn()
            .query_row(
                "SELECT request_state, schedule_state, resolved_instant FROM reminders WHERE item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .unwrap()
    }

    fn transient_failures(&self, job_id: &str) -> i64 {
        open(&self.path)
            .conn()
            .query_row(
                "SELECT transient_failure_count FROM jobs WHERE job_id = ?",
                [job_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// A second active item with its own capture, for jobs that must not cross items.
    fn add_other_item(&self, capture_text: &str) -> String {
        let other_item_id = Uuid::new_v4().to_string();
        let other_capture_id = Uuid::new_v4().to_string();
        self.db
            .conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
                rusqlite::params![other_capture_id, capture_text, NOW, ROUTE_ID, NOW],
            )
            .unwrap();
        self.db
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![other_item_id, other_capture_id, NOW, NOW],
            )
            .unwrap();
        other_item_id
    }

    fn stored_proposal_id(&self) -> String {
        open(&self.path)
            .conn()
            .query_row(
                "SELECT proposal_id FROM proposals WHERE item_id = ?",
                [&self.item_id],
                |row| row.get(0),
            )
            .unwrap()
    }
}

/// The backoff after the `failures`-th transient failure under `anthropic_profile`'s default
/// retry policy (500 ms base, 30 s cap): `min(30 000, 500 * 2^failures)` milliseconds.
fn transient_delay(failures: u32) -> Duration {
    Duration::milliseconds(std::cmp::min(30_000, 500i64 << failures))
}

/// `anthropic_profile` with its retry policy replaced.
fn anthropic_profile_with_retry(retry: RetryPolicy) -> ProviderProfile {
    anthropic_profile()
        .to_builder()
        .retry_policy(retry)
        .build()
        .unwrap()
}

fn span_of(text: &str, needle: &str) -> Value {
    let byte_start = text.find(needle).expect("needle in text");
    let start = text[..byte_start].chars().count();
    json!({ "start": start, "end": start + needle.chars().count() })
}

fn annotate_action(text: &str, evidence: &str) -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [span_of(text, evidence)],
    })
}

fn body_of(request: &HttpRequest) -> String {
    String::from_utf8(request.body.clone()).expect("request body is UTF-8")
}

// ---- Fast path ----

#[test]
fn a_recognized_command_is_applied_offline_without_any_provider_call() {
    let text = "Remind me 2026-02-20 14:30:00 to call mom";
    let mut fixture = Fixture::new(text);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::FastPath,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    let durable = fixture.durable();
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(durable.capture_text, text);
    let reminder = fixture.reminder().expect("reminder recorded");
    assert_eq!(reminder.0, "resolved");
    assert_eq!(reminder.2.as_deref(), Some("2026-02-20T14:30:00Z"));
    assert_eq!(fixture.job_row(&job.job_id).status, "completed");
}

#[test]
fn an_unpinned_job_uses_only_the_fast_path_and_abstains_honestly() {
    let text = "don't remind me 2026-02-20 14:30:00 to call mom";
    let mut fixture = Fixture::new(text);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(None);

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::FastPath,
                outcome: ApplyOutcome::Abstained {
                    processing_state: ProcessingState::Abstained
                },
            }
        ),
        "{outcome:?}"
    );
    assert!(fixture.reminder().is_none());
    assert_eq!(fixture.durable().capture_text, text);
    assert_eq!(fixture.job_row(&job.job_id).status, "completed");
}

#[test]
fn free_text_without_any_approved_provider_is_reported_as_awaiting_configuration() {
    let mut fixture = Fixture::new(FREE_FORM);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(None);

    let outcome = providers.run(&mut fixture, &job, now());

    match outcome {
        DispatchOutcome::Failed { failure, outcome } => {
            assert_eq!(failure.kind, FailureKind::CapabilityUnavailable);
            assert!(matches!(
                outcome,
                ApplyOutcome::Failed {
                    processing_state: ProcessingState::Unprocessed
                }
            ));
        }
        other => panic!("{other:?}"),
    }
    let row = fixture.job_row(&job.job_id);
    assert_eq!(
        (row.status.as_str(), row.failure_reason.as_deref()),
        ("failed", Some("unsupported"))
    );
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    let durable = fixture.durable();
    assert_eq!(durable.capture_text, FREE_FORM);
    assert_eq!(durable.processing_state, "unprocessed");
}

// ---- Provider interpretation through the selected adapter ----

#[test]
fn a_capture_becomes_an_annotation_through_the_anthropic_adapter() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::Provider,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    let calls = providers.calls_to_anthropic();
    assert_eq!(calls.len(), 1);
    assert!(body_of(&calls[0]).contains(FREE_FORM));
    assert_eq!(
        calls[0].credential.as_ref().unwrap().reference,
        "credential-ref/anthropic-primary"
    );
    assert_eq!(providers.openai.call_count(), 0);
    let durable = fixture.durable();
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(durable.capture_text, FREE_FORM);
    assert_eq!(fixture.job_row(&job.job_id).status, "completed");
}

#[test]
fn the_adapter_is_selected_by_the_pinned_profile_protocol() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = openai_profile();
    fixture.set_up_provider(&profile, OPENAI_ORIGIN);
    let providers = Providers::new(
        vec![],
        OpenAiStub::replying(annotate_action(FREE_FORM, "call the roofer")),
    );
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::Provider,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(providers.openai.call_count(), 1);
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable().item_type.as_deref(), Some("action"));
}

#[test]
fn a_provider_reminder_candidate_that_agrees_with_the_resolver_is_scheduled() {
    let text = "The leak is getting worse. Remind me 2026-02-20 14:30:00 to call them";
    let mut fixture = Fixture::new(text);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let mut proposal = annotate_action(text, "call them");
    proposal["reminder_proposal"] = json!({
        "instant": "2026-02-20T14:30:00Z",
        "timezone_id": "UTC",
        "quality": "explicit",
        "source_span": span_of(text, "2026-02-20 14:30:00"),
    });
    let providers = Providers::anthropic(vec![anthropic_reply(proposal)]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::Provider,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(providers.calls_to_anthropic().len(), 1);
    let reminder = fixture
        .reminder()
        .unwrap_or_else(|| panic!("reminder recorded: {outcome:?}"));
    assert_eq!(reminder.0, "resolved");
    assert_eq!(reminder.2.as_deref(), Some("2026-02-20T14:30:00Z"));
}

#[test]
fn a_fast_path_abstention_does_not_suppress_provider_interpretation() {
    let text = "don't remind me 2026-02-20 14:30:00 to call mom";
    let mut fixture = Fixture::new(text);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(json!({
        "operation": { "kind": "annotate" },
        "item_type": "note",
        "source_spans": [span_of(text, "call mom")],
    }))]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::Provider,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(providers.calls_to_anthropic().len(), 1);
    assert!(fixture.reminder().is_none());
    assert_eq!(fixture.durable().item_type.as_deref(), Some("note"));
}

#[test]
fn a_provider_abstention_is_recorded_as_abstained() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(json!({
        "operation": { "kind": "annotate" },
        "abstention": "Ambiguous",
    }))]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    match outcome {
        DispatchOutcome::Interpreted {
            route: InterpretationRoute::Provider,
            outcome:
                ApplyOutcome::Abstained {
                    processing_state: ProcessingState::Abstained,
                },
        } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fixture.durable().item_type, None);
    assert_eq!(fixture.durable().capture_text, FREE_FORM);
}

#[test]
fn a_corrected_text_is_what_the_provider_sees_and_what_spans_refer_to() {
    let mut fixture = Fixture::new("cal the rufer");
    let corrected = "call the roofer today";
    let correction_event = Uuid::new_v4();
    fixture
        .db
        .conn()
        .execute(
            "INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at) \
             VALUES (?, ?, 1, 'text', 'cal the rufer', ?, ?)",
            rusqlite::params![
                format!("{correction_event}-correction"),
                fixture.item_id,
                corrected,
                NOW
            ],
        )
        .unwrap();
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET revision = 1 WHERE item_id = ?",
            [&fixture.item_id],
        )
        .unwrap();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        corrected,
        "call the roofer",
    ))]);
    fixture.enqueue(Some(&profile), 1, now());
    let job = fixture.claim(now());

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                outcome: ApplyOutcome::Applied { .. },
                ..
            }
        ),
        "{outcome:?}"
    );
    let calls = providers.calls_to_anthropic();
    assert_eq!(calls.len(), 1);
    assert!(body_of(&calls[0]).contains(corrected));
    assert!(!body_of(&calls[0]).contains("cal the rufer"));
    let basis: String = open(&fixture.path)
        .conn()
        .query_row(
            "SELECT text_basis_kind FROM proposals WHERE item_id = ?",
            [&fixture.item_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(basis, "correction");
}

// ---- Failure handling ----

#[test]
fn a_transient_outage_backs_off_visibly_and_the_retry_succeeds() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![
        FakeAnthropicStep::fail(TransportError::Unavailable),
        anthropic_reply(annotate_action(FREE_FORM, "call the roofer")),
    ]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();

    let outcome = providers.run(&mut fixture, &job, now());

    let retry_at = match outcome {
        DispatchOutcome::BackedOff {
            reason: WaitReason::Transient(FailureKind::Unavailable),
            retry_at,
        } => retry_at,
        other => panic!("{other:?}"),
    };
    assert!(retry_at > now());
    assert_eq!(fixture.durable(), before);
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some("unavailable"));
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::RetryingAfterTransient)
    );

    // The same job is claimed again after the backoff and the outage is over.
    let retry = fixture.claim(retry_at + Duration::seconds(1));
    assert_eq!(retry.job_id, job.job_id);
    let outcome = providers.run(&mut fixture, &retry, retry_at + Duration::seconds(1));
    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                outcome: ApplyOutcome::Applied { .. },
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(fixture.durable().processing_state, "processed");
    assert_eq!(fixture.status().processing_job_status, None);
    assert_eq!(fixture.job_row(&job.job_id).status, "completed");
}

#[test]
fn transient_failures_stop_at_the_pinned_attempt_limit_and_keep_the_source() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let max_attempts = profile.retry_policy().max_attempts;
    assert_eq!(max_attempts, 3);
    let providers = Providers::anthropic(
        (0..max_attempts)
            .map(|_| FakeAnthropicStep::fail(TransportError::Unavailable))
            .collect(),
    );
    let mut job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();
    let mut at = now();

    for attempt in 1..max_attempts {
        assert_eq!(job.attempt_count, attempt as i32);
        let outcome = providers.run(&mut fixture, &job, at);
        let retry_at = match outcome {
            DispatchOutcome::BackedOff {
                reason: WaitReason::Transient(FailureKind::Unavailable),
                retry_at,
            } => retry_at,
            other => panic!("attempt {attempt}: {other:?}"),
        };
        assert_eq!(retry_at, at + transient_delay(attempt));
        assert_eq!(fixture.job_row(&job.job_id).status, "queued");
        at = retry_at + Duration::seconds(1);
        job = fixture.claim(at);
    }

    assert_eq!(job.attempt_count, max_attempts as i32);
    let outcome = providers.run(&mut fixture, &job, at);

    match outcome {
        DispatchOutcome::Failed { failure, outcome } => {
            assert_eq!(failure.kind, FailureKind::Unavailable);
            assert!(matches!(
                outcome,
                ApplyOutcome::Failed {
                    processing_state: ProcessingState::Uninterpreted
                }
            ));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(providers.calls_to_anthropic().len(), max_attempts as usize);
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
    let after = fixture.durable();
    assert_eq!(after.capture_text, before.capture_text);
    assert_eq!(after.item_type, None);
    assert_eq!(after.proposals, 0);
    assert_eq!(after.processing_state, "uninterpreted");
    assert_eq!(fixture.status().processing_job_status, None);
}

#[test]
fn a_non_integral_second_backoff_maximum_is_never_exceeded() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile_with_retry(RetryPolicy {
        max_attempts: 3,
        initial_backoff_ms: 1_500,
        max_backoff_ms: 1_500,
    });
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![
        FakeAnthropicStep::fail(TransportError::Unavailable),
        FakeAnthropicStep::fail(TransportError::Unavailable),
    ]);
    let mut job = fixture.enqueue_and_claim(Some(&profile));
    let mut at = now();

    for attempt in 1..=2 {
        let outcome = providers.run(&mut fixture, &job, at);
        let retry_at = match outcome {
            DispatchOutcome::BackedOff {
                reason: WaitReason::Transient(FailureKind::Unavailable),
                retry_at,
            } => retry_at,
            other => panic!("attempt {attempt}: {other:?}"),
        };
        assert_eq!(retry_at, at + Duration::milliseconds(1_500));
        let stored = fixture.job_row(&job.job_id).next_attempt_at.unwrap();
        assert_eq!(
            DateTime::parse_from_rfc3339(&stored).unwrap(),
            retry_at,
            "the stored retry time keeps millisecond precision"
        );
        // Not eligible before the configured maximum has elapsed; eligible once it has.
        assert!(claim_job_with_lease(
            &mut fixture.db,
            Duration::seconds(LEASE_SECONDS),
            retry_at - Duration::milliseconds(1)
        )
        .unwrap()
        .is_none());
        at = retry_at;
        job = fixture.claim(at);
    }
    assert_eq!(providers.calls_to_anthropic().len(), 2);
}

#[test]
fn the_largest_accepted_backoff_policy_requeues_durably_instead_of_panicking() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile_with_retry(RetryPolicy {
        max_attempts: 3,
        initial_backoff_ms: u64::MAX,
        max_backoff_ms: u64::MAX,
    });
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers =
        Providers::anthropic(vec![FakeAnthropicStep::fail(TransportError::Unavailable)]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();

    let outcome = providers.run(&mut fixture, &job, now());

    let retry_at = match outcome {
        DispatchOutcome::BackedOff {
            reason: WaitReason::Transient(FailureKind::Unavailable),
            retry_at,
        } => retry_at,
        other => panic!("{other:?}"),
    };
    // The unrepresentable delay is shortened to the latest time the queue can store and order.
    assert_eq!(retry_at.to_rfc3339(), "9999-12-31T23:59:59.999+00:00");
    assert_eq!(fixture.durable(), before);
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some("unavailable"));
    assert_eq!(
        row.next_attempt_at.as_deref(),
        Some("9999-12-31T23:59:59.999+00:00")
    );
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::RetryingAfterTransient)
    );
    // Stored as a four-digit year, the retry still sorts after now: nothing is claimable early.
    assert!(claim_job_with_lease(
        &mut fixture.db,
        Duration::seconds(LEASE_SECONDS),
        now() + Duration::days(365 * 1000)
    )
    .unwrap()
    .is_none());
    assert_eq!(providers.calls_to_anthropic().len(), 1);
}

#[test]
fn exhausted_retries_preserve_an_earlier_result() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let mut steps = vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))];
    steps.extend((0..3).map(|_| FakeAnthropicStep::fail(TransportError::Unavailable)));
    let providers = Providers::anthropic(steps);
    let first = fixture.enqueue_and_claim(Some(&profile));
    providers.run(&mut fixture, &first, now());
    let after_first = fixture.durable();
    assert_eq!(after_first.processing_state, "processed");

    fixture.enqueue(Some(&profile), 0, now());
    let mut job = fixture.claim(now());
    let mut at = now();
    let outcome = loop {
        match providers.run(&mut fixture, &job, at) {
            DispatchOutcome::BackedOff { retry_at, .. } => {
                at = retry_at + Duration::seconds(1);
                job = fixture.claim(at);
            }
            other => break other,
        }
    };

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Failed {
                outcome: ApplyOutcome::Failed {
                    processing_state: ProcessingState::Processed
                },
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(fixture.durable(), after_first);
    assert_eq!(
        fixture.job_row(&job.job_id).failure_reason.as_deref(),
        Some("retries_exhausted")
    );
}

#[test]
fn a_cancelled_call_requeues_immediately_without_touching_the_item() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();
    let cancel = CancelToken::new();
    cancel.cancel();

    let outcome = providers.run_with(&mut fixture, &job, &cancel, now());

    match outcome {
        DispatchOutcome::BackedOff {
            reason: WaitReason::Transient(FailureKind::Cancelled),
            retry_at,
        } => assert_eq!(retry_at, now()),
        other => panic!("{other:?}"),
    }
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_row(&job.job_id).status, "queued");
}

#[test]
fn cancellations_do_not_spend_the_transient_retry_budget() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let max_attempts = profile.retry_policy().max_attempts;
    let providers = Providers::anthropic(
        (0..max_attempts)
            .map(|_| FakeAnthropicStep::fail(TransportError::Unavailable))
            .collect(),
    );
    let mut job = fixture.enqueue_and_claim(Some(&profile));
    let cancel = CancelToken::new();
    cancel.cancel();
    for _ in 0..2 {
        let outcome = providers.run_with(&mut fixture, &job, &cancel, now());
        assert!(
            matches!(outcome, DispatchOutcome::BackedOff { .. }),
            "{outcome:?}"
        );
        job = fixture.claim(now());
    }
    assert!(job.attempt_count > max_attempts as i32 - 1);
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.transient_failures(&job.job_id), 0);

    let mut at = now();
    for attempt in 1..=max_attempts {
        let outcome = providers.run(&mut fixture, &job, at);
        if attempt < max_attempts {
            let retry_at = match outcome {
                DispatchOutcome::BackedOff {
                    reason: WaitReason::Transient(FailureKind::Unavailable),
                    retry_at,
                } => retry_at,
                other => panic!("attempt {attempt}: {other:?}"),
            };
            // The delay is the one a job with no earlier waits gets for this failure.
            assert_eq!(retry_at, at + transient_delay(attempt));
            at = retry_at + Duration::seconds(1);
            job = fixture.claim(at);
        } else {
            assert!(
                matches!(
                    outcome,
                    DispatchOutcome::Failed {
                        outcome: ApplyOutcome::Failed {
                            processing_state: ProcessingState::Uninterpreted
                        },
                        ..
                    }
                ),
                "{outcome:?}"
            );
        }
    }

    assert_eq!(providers.calls_to_anthropic().len(), max_attempts as usize);
    assert_eq!(
        fixture.transient_failures(&job.job_id),
        i64::from(max_attempts)
    );
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
    assert_eq!(fixture.durable().capture_text, FREE_FORM);
}

#[test]
fn configuration_waits_do_not_spend_the_transient_retry_budget() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.install(&profile);
    let max_attempts = profile.retry_policy().max_attempts;
    let providers = Providers::anthropic(
        (0..max_attempts)
            .map(|_| FakeAnthropicStep::fail(TransportError::Unavailable))
            .collect(),
    );
    let mut job = fixture.enqueue_and_claim(Some(&profile));
    let mut at = now();
    for _ in 0..max_attempts {
        let outcome = providers.run(&mut fixture, &job, at);
        let retry_at = match outcome {
            DispatchOutcome::BackedOff {
                reason: WaitReason::Denied(DenialReason::RouteNotConfigured),
                retry_at,
            } => retry_at,
            other => panic!("{other:?}"),
        };
        at = retry_at + Duration::seconds(1);
        job = fixture.claim(at);
    }
    assert!(providers.calls_to_anthropic().is_empty());
    assert!(job.attempt_count > max_attempts as i32);

    fixture.authorize(&[ANTHROPIC_ORIGIN]);
    for attempt in 1..=max_attempts {
        let outcome = providers.run(&mut fixture, &job, at);
        if attempt < max_attempts {
            let retry_at = match outcome {
                DispatchOutcome::BackedOff {
                    reason: WaitReason::Transient(FailureKind::Unavailable),
                    retry_at,
                } => retry_at,
                other => panic!("attempt {attempt}: {other:?}"),
            };
            // The delay is the one a job with no earlier waits gets for this failure.
            assert_eq!(retry_at, at + transient_delay(attempt));
            at = retry_at + Duration::seconds(1);
            job = fixture.claim(at);
        } else {
            assert!(
                matches!(outcome, DispatchOutcome::Failed { .. }),
                "{outcome:?}"
            );
        }
    }

    assert_eq!(providers.calls_to_anthropic().len(), max_attempts as usize);
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
}

#[test]
fn a_job_found_at_its_attempt_limit_is_ended_without_another_provider_call() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let max_attempts = profile.retry_policy().max_attempts;
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let abandoned = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();
    // The state a restart finds if the last attempt was counted but its holder never settled
    // the job: budget spent, lease still out.
    fixture
        .db
        .conn()
        .execute(
            "UPDATE jobs SET transient_failure_count = ? WHERE job_id = ?",
            rusqlite::params![i64::from(max_attempts), &abandoned.job_id],
        )
        .unwrap();
    let later = now() + Duration::seconds(LEASE_SECONDS + 1);
    let job = fixture.claim(later);
    assert_eq!(job.job_id, abandoned.job_id);

    let outcome = providers.run(&mut fixture, &job, later);

    assert!(
        matches!(
            outcome,
            DispatchOutcome::RetriesExhausted {
                outcome: ApplyOutcome::Failed {
                    processing_state: ProcessingState::Uninterpreted
                }
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
    let after = fixture.durable();
    assert_eq!(after.capture_text, before.capture_text);
    assert_eq!(after.item_type, None);
    assert_eq!(after.proposals, 0);
    assert_eq!(after.processing_state, "uninterpreted");
}

#[test]
fn the_failure_that_spends_the_last_attempt_ends_the_job_in_the_same_step() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let max_attempts = profile.retry_policy().max_attempts;
    let providers =
        Providers::anthropic(vec![FakeAnthropicStep::fail(TransportError::Unavailable)]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    fixture
        .db
        .conn()
        .execute(
            "UPDATE jobs SET transient_failure_count = ? WHERE job_id = ?",
            rusqlite::params![i64::from(max_attempts) - 1, &job.job_id],
        )
        .unwrap();

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(outcome, DispatchOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(providers.calls_to_anthropic().len(), 1);
    // A fresh connection sees the counted failure and the terminal job together.
    assert_eq!(
        fixture.transient_failures(&job.job_id),
        i64::from(max_attempts)
    );
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.failure_reason.as_deref(), Some("retries_exhausted"));
    assert_eq!(row.attempt_count, job.attempt_count);
}

#[test]
fn a_reclaimed_stale_lease_makes_no_provider_call_and_changes_nothing() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let stale = fixture.enqueue_and_claim(Some(&profile));
    let later = now() + Duration::seconds(LEASE_SECONDS + 1);
    let current = fixture.claim(later);
    assert_eq!(current.job_id, stale.job_id);
    assert_eq!(current.attempt_count, stale.attempt_count + 1);
    let before = fixture.durable();
    let row_before = fixture.job_row(&stale.job_id);

    let result = providers.try_run(&mut fixture, &stale, &CancelToken::new(), later);

    assert!(
        matches!(
            result,
            Err(DispatchError::Apply(ApplyError::StaleLease { .. }))
        ),
        "{result:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_row(&stale.job_id), row_before);
    assert_eq!(fixture.transient_failures(&stale.job_id), 0);

    // The live lease still runs normally.
    let outcome = providers.run(&mut fixture, &current, later);
    assert!(
        matches!(outcome, DispatchOutcome::Interpreted { .. }),
        "{outcome:?}"
    );
    assert_eq!(providers.calls_to_anthropic().len(), 1);
}

#[test]
fn an_expired_unreclaimed_lease_makes_no_provider_call() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();
    let row_before = fixture.job_row(&job.job_id);

    let expired = now() + Duration::seconds(LEASE_SECONDS + 1);
    let result = providers.try_run(&mut fixture, &job, &CancelToken::new(), expired);

    assert!(
        matches!(
            result,
            Err(DispatchError::Apply(ApplyError::StaleLease { .. }))
        ),
        "{result:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_row(&job.job_id), row_before);
}

#[test]
fn a_job_that_is_not_running_in_durable_state_makes_no_provider_call() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let cancel = CancelToken::new();
    cancel.cancel();
    providers.run_with(&mut fixture, &job, &cancel, now());
    assert_eq!(fixture.job_row(&job.job_id).status, "queued");
    let before = fixture.durable();

    let result = providers.try_run(&mut fixture, &job, &CancelToken::new(), now());

    assert!(
        matches!(
            result,
            Err(DispatchError::Apply(ApplyError::JobNotRunning { .. }))
        ),
        "{result:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
}

#[test]
fn a_job_altered_by_the_caller_makes_no_provider_call_and_changes_nothing() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let other_item_id = fixture.add_other_item("buy stamps and envelopes");
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();
    let row_before = fixture.job_row(&job.job_id);

    let mut other_item = job.clone();
    other_item.item_id = other_item_id.clone();
    let mut other_revision = job.clone();
    other_revision.source_revision += 1;
    let mut other_request = job.clone();
    other_request.request_version = Some("a-different-request".to_string());
    let mut other_profile = job.clone();
    other_profile.profile_version = Some(openai_profile().profile_version().to_string());
    let mut dropped_profile = job.clone();
    dropped_profile.profile_version = None;

    for (altered, expected_field) in [
        (other_item, "item_id"),
        (other_revision, "source_revision"),
        (other_request, "request_version"),
        (other_profile, "profile_version"),
        (dropped_profile, "profile_version"),
    ] {
        let result = providers.try_run(&mut fixture, &altered, &CancelToken::new(), now());
        match result {
            Err(DispatchError::Apply(ApplyError::JobBindingMismatch { field })) => {
                assert_eq!(field, expected_field)
            }
            other => panic!("{expected_field}: {other:?}"),
        }
    }

    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
    assert_eq!(fixture.job_row(&job.job_id), row_before);
    let other_processing: String = open(&fixture.path)
        .conn()
        .query_row(
            "SELECT processing_state FROM items WHERE item_id = ?",
            [&other_item_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_processing, "unprocessed");
}

#[test]
fn invalid_provider_output_keeps_the_raw_capture_usable() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(json!({
        "operation": { "kind": "annotate" },
        "scope": "work",
    }))]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    match outcome {
        DispatchOutcome::Failed { failure, outcome } => {
            assert_eq!(failure.kind, FailureKind::InvalidOutput);
            assert!(matches!(
                outcome,
                ApplyOutcome::Failed {
                    processing_state: ProcessingState::Uninterpreted
                }
            ));
        }
        other => panic!("{other:?}"),
    }
    let durable = fixture.durable();
    assert_eq!(durable.capture_text, FREE_FORM);
    assert_eq!(durable.scope, "personal");
    assert_eq!(durable.item_type, None);
    assert_eq!(durable.processing_state, "uninterpreted");
    assert_eq!(durable.proposals, 0);
    assert_eq!(
        fixture.job_row(&job.job_id).failure_reason.as_deref(),
        Some("invalid_output")
    );
    let hits = search_source_direct(fixture.db.conn(), "roofer", &[ItemScope::Personal]).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the failed item stays searchable by its text"
    );
}

#[test]
fn an_out_of_bounds_span_in_provider_output_is_rejected_without_effect() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [{ "start": 0, "end": 4000 }],
    }))]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Failed {
                ref failure,
                ..
            } if failure.kind == FailureKind::InvalidOutput
        ),
        "{outcome:?}"
    );
    let durable = fixture.durable();
    assert_eq!(durable.item_type, None);
    assert_eq!(durable.proposals, 0);
}

#[test]
fn a_later_model_failure_preserves_the_earlier_result() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![
        anthropic_reply(annotate_action(FREE_FORM, "call the roofer")),
        anthropic_reply(json!({ "operation": { "kind": "annotate" }, "route": "elsewhere" })),
    ]);
    let first = fixture.enqueue_and_claim(Some(&profile));
    providers.run(&mut fixture, &first, now());
    let after_first = fixture.durable();
    assert_eq!(after_first.processing_state, "processed");

    fixture.enqueue(Some(&profile), 0, now());
    let second = fixture.claim(now());
    let outcome = providers.run(&mut fixture, &second, now());

    match outcome {
        DispatchOutcome::Failed { outcome, .. } => assert!(matches!(
            outcome,
            ApplyOutcome::Failed {
                processing_state: ProcessingState::Processed
            }
        )),
        other => panic!("{other:?}"),
    }
    assert_eq!(fixture.durable(), after_first);
    assert_eq!(fixture.job_row(&second.job_id).status, "failed");
}

#[test]
fn a_rejected_provider_credential_waits_for_configuration() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers =
        Providers::anthropic(vec![FakeAnthropicStep::fail(TransportError::Unauthorized)]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Failed {
                ref failure,
                ..
            } if failure.kind == FailureKind::Unauthorized
        ),
        "{outcome:?}"
    );
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    let durable = fixture.durable();
    assert_eq!(durable.processing_state, "unprocessed");
    assert_eq!(durable.capture_text, FREE_FORM);
}

#[test]
fn a_profile_whose_protocol_has_no_registered_adapter_waits_for_configuration() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::without_adapters();
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Failed {
                ref failure,
                ..
            } if failure.kind == FailureKind::CapabilityUnavailable
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    assert_eq!(fixture.durable().capture_text, FREE_FORM);
}

#[test]
fn a_stored_profile_that_fails_validation_is_not_dispatched() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    fixture
        .db
        .conn()
        .execute(
            "UPDATE provider_profiles SET capabilities = '{}', retry_policy = '{}' WHERE profile_version = ?",
            [profile.profile_version()],
        )
        .unwrap();
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Failed {
                ref failure,
                ..
            } if failure.kind == FailureKind::CapabilityUnavailable
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
}

// ---- Privacy boundary ----

#[test]
fn an_unauthorized_route_is_never_sent_to_a_provider() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.install(&profile);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::BackedOff {
                reason: WaitReason::Denied(DenialReason::RouteNotConfigured),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
    assert_eq!(
        fixture.status().processing_job_status,
        Some(ProcessingJobStatus::AwaitingConfiguration)
    );
    assert_eq!(
        fixture.job_row(&job.job_id).failure_reason.as_deref(),
        Some("unauthorized")
    );
}

#[test]
fn a_supported_command_applies_locally_while_the_remote_grant_is_missing() {
    let text = "Remind me 2026-02-20 14:30:00 to call mom";
    let mut fixture = Fixture::new(text);
    let profile = anthropic_profile();
    fixture.install(&profile);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Interpreted {
                route: InterpretationRoute::FastPath,
                outcome: ApplyOutcome::Applied { .. },
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    let durable = fixture.durable();
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(durable.processing_state, "processed");
    assert_eq!(durable.capture_text, text);
    let reminder = fixture.reminder().expect("reminder recorded");
    assert_eq!(reminder.2.as_deref(), Some("2026-02-20T14:30:00Z"));
    assert_eq!(fixture.job_row(&job.job_id).status, "completed");
}

#[test]
fn free_text_still_waits_when_the_remote_grant_is_missing() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.install(&profile);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    let before = fixture.durable();

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::BackedOff {
                reason: WaitReason::Denied(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.durable(), before);
}

// ---- Stale work and identity ----

#[test]
fn a_job_for_a_revised_item_is_retired_without_a_provider_call() {
    let mut fixture = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET revision = 1 WHERE item_id = ?",
            [&fixture.item_id],
        )
        .unwrap();

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Retired(RetireReason::SourceRevised)
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    let row = fixture.job_row(&job.job_id);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.failure_reason.as_deref(), Some("source_revised"));
}

#[test]
fn a_completed_item_retires_its_job() {
    let mut fixture = Fixture::new(FREE_FORM);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(None);
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'completed' WHERE item_id = ?",
            [&fixture.item_id],
        )
        .unwrap();

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::Retired(RetireReason::ItemNotActive)
        ),
        "{outcome:?}"
    );
    assert_eq!(fixture.job_row(&job.job_id).status, "cancelled");
}

#[test]
fn an_item_without_text_waits_instead_of_calling_a_provider() {
    let mut fixture = Fixture::new(FREE_FORM);
    fixture
        .db
        .conn()
        .execute(
            "UPDATE captures SET text = NULL, audio_reference = 'audio/synthetic.m4a' WHERE capture_id = ?",
            [&fixture.capture_id],
        )
        .unwrap();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![]);
    let job = fixture.enqueue_and_claim(Some(&profile));

    let outcome = providers.run(&mut fixture, &job, now());

    assert!(
        matches!(
            outcome,
            DispatchOutcome::BackedOff {
                reason: WaitReason::SourceUnavailable,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(providers.calls_to_anthropic().is_empty());
    assert_eq!(fixture.job_row(&job.job_id).status, "queued");
}

#[test]
fn the_proposal_identity_is_stable_for_a_job_across_routes() {
    let mut fast = Fixture::new("Remind me 2026-02-20 14:30:00 to call mom");
    let mut provider = Fixture::new(FREE_FORM);
    let profile = anthropic_profile();
    provider.set_up_provider(&profile, ANTHROPIC_ORIGIN);
    let providers = Providers::anthropic(vec![anthropic_reply(annotate_action(
        FREE_FORM,
        "call the roofer",
    ))]);
    let fast_job = fast.enqueue_and_claim(None);
    let provider_job = provider.enqueue_and_claim(Some(&profile));

    providers.run(&mut fast, &fast_job, now());
    providers.run(&mut provider, &provider_job, now());

    for (fixture, job) in [(&fast, &fast_job), (&provider, &provider_job)] {
        let id = fixture.stored_proposal_id();
        let parsed = Uuid::parse_str(&id).unwrap();
        assert_eq!(parsed.get_version_num(), 5);
        let again = Uuid::parse_str(&fixture.stored_proposal_id()).unwrap();
        assert_eq!(parsed, again, "job {}", job.job_id);
    }
    assert_ne!(fast.stored_proposal_id(), provider.stored_proposal_id());
}
