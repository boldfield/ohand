//! Fixtures for the processing retry and deletion-race suite (T02b). Captures enter through the
//! real foreground ingress import, jobs run through the real `JobRunner`, the interpretation
//! dispatcher, the Anthropic adapter and the apply boundary. Only the HTTP transport is scripted;
//! it can also run a hook just before answering, which stands in for whatever the application
//! does while a provider call is in flight (a deletion, a correction, a policy change, another
//! worker reclaiming the job). All content is synthetic.

use crate::harness::{synthetic_text_record, Workspace, PERSONAL_ROUTE};
use chrono::{DateTime, Duration, Utc};
use ohand_core::ingress::{import_foreground_ingress, ImportDisposition};
use ohand_core::interpretation::dispatch::{AdapterRegistry, InterpretationDispatcher};
use ohand_core::jobs::queue::{enqueue_job, Job};
use ohand_core::jobs::runner::{
    DrainReport, InterpretationCapability, JobCapabilities, JobRunner, RunnerConfig,
};
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::providers::anthropic::fake::{FakeAnthropicStep, FakeAnthropicTransport};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, AnthropicTransport, HttpRequest, HttpResponse,
    INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    CancelToken, CapabilityMetadata, ManualClock, ProviderCapability, ProviderProfile,
    ProviderProfileBuilder, ProviderProtocol, RetryPolicy, StructuredOutputMode, TransportError,
};
use ohand_core::store::schema::Clock as StoreClock;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
pub const FREE_FORM_TEXT: &str = "call the roofer about the leak";
pub const LEASE_SECONDS: i64 = 60;
const ANTHROPIC_MODEL: &str = "synthetic-anthropic-model";
const SETUP_TIME: &str = "2026-10-08T09:30:02Z";

pub fn lease() -> Duration {
    Duration::seconds(LEASE_SECONDS)
}

pub fn runner_config() -> RunnerConfig {
    RunnerConfig {
        lease_duration: lease(),
        max_jobs_per_drain: 25,
        time_budget: None,
        max_attempts: 5,
        base_backoff: Duration::seconds(30),
        max_backoff: Duration::hours(1),
        capability_unavailable_delay: Duration::minutes(15),
    }
}

/// A store clock a test moves by hand, so lease expiry and backoff are exact.
pub struct SteppedClock {
    now: Mutex<DateTime<Utc>>,
}

impl SteppedClock {
    pub fn starting_at(start: DateTime<Utc>) -> Arc<SteppedClock> {
        Arc::new(SteppedClock {
            now: Mutex::new(start),
        })
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += by;
    }

    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.lock().unwrap() = to;
    }
}

impl StoreClock for SteppedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}

pub fn setup_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(SETUP_TIME)
        .unwrap()
        .with_timezone(&Utc)
}

// ---- Providers ----

fn text_capability() -> CapabilityMetadata {
    CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "resilience-fixtures:capture_processing",
    )
    .with_input_size_limit(500)
    .with_structured_output(StructuredOutputMode::JsonSchema)
}

pub fn anthropic_profile(retry: RetryPolicy) -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-anthropic",
        ProviderProtocol::Anthropic,
        ANTHROPIC_MODEL,
    )
    .credential_ref("credential-ref/anthropic-primary")
    .authorized_destination(ANTHROPIC_ORIGIN)
    .capability(text_capability())
    .retry_policy(retry)
    .timeout_seconds(30)
    .build()
    .unwrap()
}

pub fn retry_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        initial_backoff_ms: 500,
        max_backoff_ms: 30_000,
    }
}

pub fn span_of(text: &str, needle: &str) -> Value {
    let byte_start = text.find(needle).expect("needle in text");
    let start = text[..byte_start].chars().count();
    json!({ "start": start, "end": start + needle.chars().count() })
}

pub fn annotate_action(text: &str, evidence: &str) -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [span_of(text, evidence)],
    })
}

pub fn anthropic_reply(proposal: Value) -> FakeAnthropicStep {
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

pub fn good_reply() -> FakeAnthropicStep {
    anthropic_reply(annotate_action(FREE_FORM_TEXT, "call the roofer"))
}

pub fn unavailable() -> FakeAnthropicStep {
    FakeAnthropicStep::fail(TransportError::Unavailable)
}

/// A reply that arrives after the profile's 30 second timeout.
pub fn too_slow() -> FakeAnthropicStep {
    good_reply().after_ms(31_000)
}

type InFlightHook = Box<dyn FnOnce() + Send>;

/// The scripted transport plus an optional action run once, just before the next call is
/// answered, while the dispatcher holds a claimed job and has not yet applied anything.
struct HookedTransport {
    inner: FakeAnthropicTransport,
    before_answer: Mutex<Option<InFlightHook>>,
}

impl AnthropicTransport for HookedTransport {
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        let hook = self.before_answer.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        self.inner.send(request, cancel)
    }
}

/// A scripted Anthropic provider behind the real adapter registry.
pub struct Providers {
    clock: Arc<ManualClock>,
    transport: Arc<HookedTransport>,
    registry: AdapterRegistry,
}

impl Providers {
    pub fn new(steps: Vec<FakeAnthropicStep>) -> Providers {
        let clock = Arc::new(ManualClock::new());
        let transport = Arc::new(HookedTransport {
            inner: FakeAnthropicTransport::new(clock.clone(), steps),
            before_answer: Mutex::new(None),
        });
        let registry = AdapterRegistry::new().with_adapter(
            ProviderProtocol::Anthropic,
            AnthropicAdapter::new(transport.clone(), AnthropicSettings::default()),
        );
        Providers {
            clock,
            transport,
            registry,
        }
    }

    /// Run `hook` once while the next provider call is in flight.
    pub fn during_next_call(&self, hook: impl FnOnce() + Send + 'static) {
        *self.transport.before_answer.lock().unwrap() = Some(Box::new(hook));
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.transport.inner.calls()
    }

    pub fn call_count(&self) -> usize {
        self.requests().len()
    }

    pub fn dispatcher(&self) -> InterpretationDispatcher<'_> {
        InterpretationDispatcher::new(&self.registry, self.clock.as_ref())
    }

    fn capabilities(&self) -> JobCapabilities<'_> {
        JobCapabilities::new().with(InterpretationCapability::new(self.dispatcher()))
    }
}

pub fn request_body(request: &HttpRequest) -> String {
    String::from_utf8(request.body.clone()).expect("request body is UTF-8")
}

// ---- The world ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub capture_text: String,
    pub capture_scope: String,
    pub capture_route: String,
    pub item_type: Option<String>,
    pub processing_state: String,
    pub lifecycle_state: String,
    pub revision: i64,
    pub proposals: i64,
    pub corrections: i64,
    pub search_rows: i64,
    pub reminders: i64,
    pub route_destinations: String,
    pub grants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub status: String,
    pub failure_reason: Option<String>,
    pub attempt_count: i64,
    pub transient_failure_count: i64,
}

/// One imported capture with a profile installed and its route authorized to use it.
pub struct ProcessingWorld {
    pub workspace: Workspace,
    pub capture_id: String,
    pub item_id: String,
    pub profile: ProviderProfile,
}

impl ProcessingWorld {
    pub fn new(profile: ProviderProfile) -> ProcessingWorld {
        ProcessingWorld::with_text(FREE_FORM_TEXT, profile)
    }

    pub fn with_text(text: &str, profile: ProviderProfile) -> ProcessingWorld {
        let workspace = Workspace::new();
        let mut record = synthetic_text_record(0);
        record.text = Some(text.to_string());
        let mut database = workspace.open_database();
        let acknowledgment =
            import_foreground_ingress(&mut database, &record.core_capture()).unwrap();
        assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);
        let world = ProcessingWorld {
            workspace,
            capture_id: acknowledgment.capture_id,
            item_id: acknowledgment.item_id,
            profile,
        };
        world.install_profile();
        world.grant_route(&[ANTHROPIC_ORIGIN]);
        world
    }

    pub fn root(&self) -> &Path {
        self.workspace.root()
    }

    fn install_profile(&self) {
        let profile = &self.profile;
        let record = serde_json::to_value(profile).unwrap();
        self.workspace
            .open_database()
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
                    SETUP_TIME,
                ],
            )
            .unwrap();
    }

    /// Route `PERSONAL_ROUTE` may process text interpretation at `origins`.
    pub fn grant_route(&self, origins: &[&str]) {
        grant_route_at(self.root(), origins);
    }

    pub fn enqueue(&self, source_revision: i32) -> String {
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.workspace.open_database(),
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            source_revision,
            Some(self.profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            setup_time(),
        )
        .unwrap();
        job_id
    }

    /// Drain the queue the way a freshly started process would: a new connection each time.
    pub fn drain(&self, providers: &Providers, clock: &SteppedClock) -> DrainReport {
        let mut database = self.workspace.open_database();
        JobRunner::new(providers.capabilities(), clock, runner_config())
            .drain(&mut database, &CancelToken::new())
    }

    pub fn snapshot(&self) -> Snapshot {
        let database = self.workspace.open_database();
        let conn = database.conn();
        let (item_type, processing_state, lifecycle_state, revision): (
            Option<String>,
            String,
            String,
            i64,
        ) = conn
            .query_row(
                "SELECT item_type, processing_state, lifecycle_state, revision
                   FROM items WHERE item_id = ?",
                [&self.item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let (capture_text, capture_scope, capture_route): (String, String, String) = conn
            .query_row(
                "SELECT text, item_scope, route_id FROM captures WHERE capture_id = ?",
                [&self.capture_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let count = |sql: &str| -> i64 {
            conn.query_row(sql, [&self.item_id], |row| row.get(0))
                .unwrap()
        };
        let route_destinations: String = conn
            .query_row(
                "SELECT processing_destinations FROM routes WHERE route_id = ?",
                [PERSONAL_ROUTE],
                |row| row.get(0),
            )
            .unwrap();
        let mut statement = conn
            .prepare(
                "SELECT capability || ':' || authorized_destinations
                   FROM route_authorizations WHERE route_id = ? ORDER BY auth_id",
            )
            .unwrap();
        let grants = statement
            .query_map([PERSONAL_ROUTE], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        Snapshot {
            capture_text,
            capture_scope,
            capture_route,
            item_type,
            processing_state,
            lifecycle_state,
            revision,
            proposals: count("SELECT COUNT(*) FROM proposals WHERE item_id = ?"),
            corrections: count("SELECT COUNT(*) FROM corrections WHERE item_id = ?"),
            search_rows: conn
                .query_row("SELECT COUNT(*) FROM search_index", [], |row| row.get(0))
                .unwrap(),
            reminders: count("SELECT COUNT(*) FROM reminders WHERE item_id = ?"),
            route_destinations,
            grants,
        }
    }

    pub fn job_row(&self, job_id: &str) -> JobRow {
        job_row_at(self.root(), job_id)
    }
}

pub fn grant_route_at(root: &Path, origins: &[&str]) {
    let encoded = serde_json::to_string(origins).unwrap();
    let database = crate::harness::open_database_at(root);
    database
        .conn()
        .execute(
            "UPDATE routes SET processing_destinations = ? WHERE route_id = ?",
            rusqlite::params![encoded, PERSONAL_ROUTE],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "DELETE FROM route_authorizations WHERE route_id = ? AND capability = 'text_interpretation'",
            [PERSONAL_ROUTE],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, created_at) \
             VALUES (?, ?, 'text_interpretation', ?, ?)",
            rusqlite::params![Uuid::new_v4().to_string(), PERSONAL_ROUTE, encoded, SETUP_TIME],
        )
        .unwrap();
}

pub fn job_row_at(root: &Path, job_id: &str) -> JobRow {
    crate::harness::open_database_at(root)
        .conn()
        .query_row(
            "SELECT status, failure_reason, attempt_count, transient_failure_count
               FROM jobs WHERE job_id = ?",
            [job_id],
            |row| {
                Ok(JobRow {
                    status: row.get(0)?,
                    failure_reason: row.get(1)?,
                    attempt_count: row.get(2)?,
                    transient_failure_count: row.get(3)?,
                })
            },
        )
        .unwrap()
}

/// What the child process of a kill scenario does with the prepared database: drain the queue
/// with the real runner against a scripted provider. `block_in_flight` stops inside the provider
/// call (nothing has been applied yet); otherwise the child waits after the result is applied.
pub fn run_child_drain(
    root: &Path,
    block_in_flight: bool,
    say: impl Fn(&str) + Send + Sync + 'static,
    wait: impl Fn() + Send + Sync + 'static,
) {
    let providers = Providers::new(vec![good_reply()]);
    let wait = Arc::new(wait);
    let say = Arc::new(say);
    if block_in_flight {
        let (say, wait) = (say.clone(), wait.clone());
        providers.during_next_call(move || {
            say("provider call in flight");
            wait();
        });
    }
    let mut database = crate::harness::open_database_at(root);
    let clock = ohand_core::store::schema::SystemClock;
    let report = JobRunner::new(providers.capabilities(), &clock, runner_config())
        .drain(&mut database, &CancelToken::new());
    say(&format!("drained {} job(s)", report.jobs.len()));
    wait();
}

/// A receipt as the claiming worker held it, for presenting a late or duplicate delivery.
pub fn receipt_of(root: &Path, job_id: &str, attempt: i32) -> Job {
    let database = crate::harness::open_database_at(root);
    let mut job = ohand_core::jobs::queue::get_job(&database, job_id)
        .unwrap()
        .unwrap();
    job.status = ohand_core::jobs::queue::JobStatus::Running;
    job.attempt_count = attempt;
    job
}
