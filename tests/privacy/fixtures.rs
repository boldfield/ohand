//! Synthetic world and scripted transports for the outbound privacy suite.

use chrono::{DateTime, Duration, Utc};
use ohand_core::ingress::{import_foreground_ingress, ImportDisposition};
use ohand_core::interpretation::dispatch::{AdapterRegistry, InterpretationDispatcher};
use ohand_core::jobs::queue::enqueue_job;
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
    CancelToken, Clock, ManualClock, ProviderCapability, ProviderProfile, ProviderProfileBuilder,
    ProviderProtocol, RetryPolicy, StructuredOutputMode, TransportError,
};
use ohand_core::providers::openai::HttpTransport;
use ohand_core::providers::self_hosted::{
    text_interpretation_capability, SelfHostedAdapter, CHAT_VERIFIED_MODEL,
};
use ohand_core::store::captures::Capture;
use ohand_core::store::schema::{Clock as StoreClock, Database, SystemClock};
use rusqlite::types::ValueRef;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use uuid::Uuid;

pub const PERSONAL_ROUTE: &str = "route-personal";
pub const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
pub const PRIVATE_ORIGIN_A: &str = "https://private-a.tailnet.example";
pub const PRIVATE_ORIGIN_B: &str = "https://private-b.tailnet.example";
pub const OTHER_ORIGIN: &str = "https://unrelated.example";
pub const ANTHROPIC_CREDENTIAL_REF: &str = "credential-ref/anthropic-primary";
pub const PRIVATE_CREDENTIAL_REF: &str = "credential-ref/private-server";
/// Capture content that may leave the device only to an authorized destination.
pub const CONTENT_CANARY: &str = "CANARY-CONTENT-7f3a9c";
/// A provider-side secret that core must never store or echo.
pub const SECRET_CANARY: &str = "CANARY-SECRET-sk-91d2e4";
/// Where a hostile redirect points; nothing may ever be sent there.
pub const REDIRECT_TARGET: &str = "https://redirect-target.example/collect";
pub const CAPTURE_TEXT: &str = "call the roofer about the leak CANARY-CONTENT-7f3a9c";
const SETUP_TIME: &str = "2026-10-08T09:30:02Z";

pub fn setup_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(SETUP_TIME)
        .unwrap()
        .with_timezone(&Utc)
}

pub fn runner_config() -> RunnerConfig {
    RunnerConfig {
        lease_duration: Duration::seconds(60),
        max_jobs_per_drain: 25,
        time_budget: None,
        max_attempts: 5,
        base_backoff: Duration::seconds(30),
        max_backoff: Duration::hours(1),
        capability_unavailable_delay: Duration::minutes(15),
    }
}

/// A store clock a test moves by hand, so backoff is exact.
pub struct SteppedClock {
    now: Mutex<DateTime<Utc>>,
}

impl SteppedClock {
    pub fn after_setup() -> Arc<SteppedClock> {
        Arc::new(SteppedClock {
            now: Mutex::new(setup_time() + Duration::seconds(1)),
        })
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }
}

impl StoreClock for SteppedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}

// ---- Profiles ----

pub fn retry_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        initial_backoff_ms: 500,
        max_backoff_ms: 30_000,
    }
}

pub fn anthropic_profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-anthropic",
        ProviderProtocol::Anthropic,
        "synthetic-anthropic-model",
    )
    .credential_ref(ANTHROPIC_CREDENTIAL_REF)
    .authorized_destination(ANTHROPIC_ORIGIN)
    .capability(
        ohand_core::providers::contracts::CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "privacy-fixtures:outbound",
        )
        .with_input_size_limit(500)
        .with_structured_output(StructuredOutputMode::JsonSchema),
    )
    .retry_policy(retry_policy(5))
    .timeout_seconds(30)
    .build()
    .unwrap()
}

pub fn private_profile(profile_id: &str, origin: &str) -> ProviderProfile {
    ProviderProfileBuilder::new(
        profile_id,
        ProviderProtocol::SelfHosted,
        CHAT_VERIFIED_MODEL,
    )
    .credential_ref(PRIVATE_CREDENTIAL_REF)
    .endpoint(format!("{origin}/v1"))
    .authorized_destination(origin)
    .capability(text_interpretation_capability(CHAT_VERIFIED_MODEL, 500))
    .retry_policy(retry_policy(5))
    .timeout_seconds(30)
    .build()
    .unwrap()
}

// ---- Scripted transports ----

pub fn anthropic_reply(proposal: Value) -> FakeAnthropicStep {
    FakeAnthropicStep::respond_json(
        200,
        &json!({
            "id": "msg_synthetic_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-anthropic-model",
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

pub fn annotate_action_proposal() -> Value {
    let start = 0;
    let end = "call the roofer".chars().count();
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [{ "start": start, "end": end }],
    })
}

pub fn anthropic_redirect() -> FakeAnthropicStep {
    let mut step = FakeAnthropicStep::respond(307, Vec::new());
    step.action =
        ohand_core::providers::anthropic::fake::FakeAnthropicAction::Respond(HttpResponse {
            status: 307,
            headers: vec![("location".to_string(), REDIRECT_TARGET.to_string())],
            body: format!("moved to {REDIRECT_TARGET}").into_bytes(),
        });
    step
}

pub fn anthropic_auth_failure() -> FakeAnthropicStep {
    FakeAnthropicStep::respond_json(
        401,
        &json!({
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": format!("invalid x-api-key {SECRET_CANARY}"),
            }
        }),
    )
}

type InFlightHook = Box<dyn FnOnce() + Send>;

/// The scripted Anthropic transport plus an optional action run once, just before the next call
/// is answered: the application doing something while a provider call is in flight.
struct HookedAnthropic {
    inner: FakeAnthropicTransport,
    before_answer: Mutex<Option<InFlightHook>>,
}

impl AnthropicTransport for HookedAnthropic {
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

/// What a private-server call handed to the native HTTP seam.
#[derive(Debug, Clone)]
pub struct PostedRequest {
    pub endpoint: String,
    pub credential_ref: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

struct RecordingPost {
    script: Mutex<Vec<(u16, Vec<u8>)>>,
    posts: Mutex<Vec<PostedRequest>>,
    before_answer: Mutex<Option<InFlightHook>>,
}

struct PrivateTransport(Arc<RecordingPost>);

impl HttpTransport for PrivateTransport {
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        _deadline_ms: u64,
        _cancel: &CancelToken,
        _clock: &dyn Clock,
        _max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let hook = self.0.before_answer.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        self.0.posts.lock().unwrap().push(PostedRequest {
            endpoint: endpoint.to_string(),
            credential_ref: credential_ref.to_string(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: String::from_utf8(body).expect("body is UTF-8"),
        });
        let mut script = self.0.script.lock().unwrap();
        if script.is_empty() {
            return Err(TransportError::Unavailable);
        }
        Ok(script.remove(0))
    }
}

pub fn private_redirect() -> (u16, Vec<u8>) {
    (302, format!("moved to {REDIRECT_TARGET}").into_bytes())
}

pub fn private_auth_failure() -> (u16, Vec<u8>) {
    (
        401,
        format!("{{\"error\":\"bad key {SECRET_CANARY}\"}}").into_bytes(),
    )
}

pub fn private_reply(proposal: &Value) -> (u16, Vec<u8>) {
    (
        200,
        json!({
            "choices": [{
                "message": { "role": "assistant", "content": proposal.to_string() },
                "finish_reason": "stop"
            }]
        })
        .to_string()
        .into_bytes(),
    )
}

/// Both protocols behind the real adapter registry, each with a recording transport.
pub struct Providers {
    clock: Arc<ManualClock>,
    anthropic: Arc<HookedAnthropic>,
    private: Arc<RecordingPost>,
    registry: AdapterRegistry,
}

impl Providers {
    pub fn new(
        anthropic_steps: Vec<FakeAnthropicStep>,
        private_steps: Vec<(u16, Vec<u8>)>,
    ) -> Providers {
        Providers::with_anthropic_endpoint(anthropic_steps, private_steps, None)
    }

    /// `endpoint` overrides the adapter's Messages URL, standing in for a misconfigured adapter.
    pub fn with_anthropic_endpoint(
        anthropic_steps: Vec<FakeAnthropicStep>,
        private_steps: Vec<(u16, Vec<u8>)>,
        endpoint: Option<&str>,
    ) -> Providers {
        let clock = Arc::new(ManualClock::new());
        let anthropic = Arc::new(HookedAnthropic {
            inner: FakeAnthropicTransport::new(clock.clone(), anthropic_steps),
            before_answer: Mutex::new(None),
        });
        let private = Arc::new(RecordingPost {
            script: Mutex::new(private_steps),
            posts: Mutex::new(Vec::new()),
            before_answer: Mutex::new(None),
        });
        let mut settings = AnthropicSettings::default();
        if let Some(endpoint) = endpoint {
            settings.endpoint = endpoint.to_string();
        }
        let registry = AdapterRegistry::new()
            .with_adapter(
                ProviderProtocol::Anthropic,
                AnthropicAdapter::new(anthropic.clone(), settings),
            )
            .with_adapter(
                ProviderProtocol::SelfHosted,
                SelfHostedAdapter::new(PrivateTransport(private.clone())),
            );
        Providers {
            clock,
            anthropic,
            private,
            registry,
        }
    }

    /// Run `hook` once while the next Anthropic call is in flight.
    pub fn during_next_anthropic_call(&self, hook: impl FnOnce() + Send + 'static) {
        *self.anthropic.before_answer.lock().unwrap() = Some(Box::new(hook));
    }

    pub fn during_next_private_call(&self, hook: impl FnOnce() + Send + 'static) {
        *self.private.before_answer.lock().unwrap() = Some(Box::new(hook));
    }

    pub fn anthropic_requests(&self) -> Vec<HttpRequest> {
        self.anthropic.inner.calls()
    }

    pub fn private_requests(&self) -> Vec<PostedRequest> {
        self.private.posts.lock().unwrap().clone()
    }

    /// Every transport call of either protocol.
    pub fn call_count(&self) -> usize {
        self.anthropic_requests().len() + self.private_requests().len()
    }

    fn capabilities(&self) -> JobCapabilities<'_> {
        JobCapabilities::new().with(InterpretationCapability::new(
            InterpretationDispatcher::new(&self.registry, self.clock.as_ref()),
        ))
    }
}

// ---- The world ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub status: String,
    pub failure_reason: Option<String>,
    pub profile_version: Option<String>,
}

/// One imported capture whose route starts with no remote authorization at all.
pub struct World {
    _directory: TempDir,
    root: PathBuf,
    pub item_id: String,
}

pub fn open_database_at(root: &Path) -> Database {
    let path = root.join("ohand.db").display().to_string();
    Database::open(&path, Arc::new(SystemClock)).unwrap()
}

impl World {
    pub fn new() -> World {
        let directory = TempDir::new().unwrap();
        let root = directory.path().to_path_buf();
        let mut database = open_database_at(&root);
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
                 VALUES (?, ?, 'personal', '[]', '2026-10-08T09:00:00Z')",
                rusqlite::params![PERSONAL_ROUTE, PERSONAL_ROUTE],
            )
            .unwrap();
        let capture = Capture {
            capture_id: Uuid::new_v4().to_string(),
            text: Some(CAPTURE_TEXT.to_string()),
            audio_reference: None,
            capture_instant: "2026-10-08T09:30:00Z".to_string(),
            timezone_id: "America/New_York".to_string(),
            utc_offset_minutes: -240,
            locale: "en_US".to_string(),
            calendar: "gregorian".to_string(),
            item_scope: "personal".to_string(),
            route_id: PERSONAL_ROUTE.to_string(),
            entry_locked: false,
            created_at: "2026-10-08T09:30:01Z".to_string(),
            session_topic: None,
        };
        let acknowledgment = import_foreground_ingress(&mut database, &capture).unwrap();
        assert_eq!(acknowledgment.disposition, ImportDisposition::Imported);
        // These scenarios enqueue their own pinned jobs, so drop the one the import queued.
        database
            .conn()
            .execute(
                "DELETE FROM jobs WHERE item_id = ? AND job_type = 'interpret'",
                [&acknowledgment.item_id],
            )
            .unwrap();
        World {
            _directory: directory,
            root,
            item_id: acknowledgment.item_id,
        }
    }

    pub fn database(&self) -> Database {
        open_database_at(&self.root)
    }

    pub fn install_profile(&self, profile: &ProviderProfile) {
        let record = serde_json::to_value(profile).unwrap();
        self.database()
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

    /// Store raw route policy: the route ceiling and, optionally, one capability grant.
    pub fn set_policy(&self, ceiling: &[&str], grant: Option<(&str, &[&str])>) {
        let database = self.database();
        database
            .conn()
            .execute(
                "UPDATE routes SET processing_destinations = ? WHERE route_id = ?",
                rusqlite::params![serde_json::to_string(ceiling).unwrap(), PERSONAL_ROUTE],
            )
            .unwrap();
        database
            .conn()
            .execute(
                "DELETE FROM route_authorizations WHERE route_id = ?",
                [PERSONAL_ROUTE],
            )
            .unwrap();
        if let Some((capability, destinations)) = grant {
            database
                .conn()
                .execute(
                    "INSERT INTO route_authorizations (auth_id, route_id, capability, \
                     authorized_destinations, created_at) VALUES (?, ?, ?, ?, ?)",
                    rusqlite::params![
                        Uuid::new_v4().to_string(),
                        PERSONAL_ROUTE,
                        capability,
                        serde_json::to_string(destinations).unwrap(),
                        SETUP_TIME
                    ],
                )
                .unwrap();
        }
    }

    pub fn enqueue(&self, profile: Option<&ProviderProfile>) -> String {
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.database(),
            job_id.clone(),
            self.item_id.clone(),
            JOB_TYPE_INTERPRET.to_string(),
            0,
            profile.map(|profile| profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            1,
            setup_time(),
        )
        .unwrap();
        job_id
    }

    /// Drain the queue the way a freshly started process would: a new connection each time.
    pub fn drain(&self, providers: &Providers, clock: &SteppedClock) -> DrainReport {
        let mut database = self.database();
        JobRunner::new(providers.capabilities(), clock, runner_config())
            .drain(&mut database, &CancelToken::new())
    }

    pub fn job_row(&self, job_id: &str) -> JobRow {
        self.database()
            .conn()
            .query_row(
                "SELECT status, failure_reason, profile_version FROM jobs WHERE job_id = ?",
                [job_id],
                |row| {
                    Ok(JobRow {
                        status: row.get(0)?,
                        failure_reason: row.get(1)?,
                        profile_version: row.get(2)?,
                    })
                },
            )
            .unwrap()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.database()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    /// Every value of every table, rendered as text, for asserting that something is absent
    /// from durable state altogether. Captured text is expected in `captures`, `search_index`
    /// and their relatives, so callers search for secrets, not content.
    pub fn dump_state(&self) -> String {
        let database = self.database();
        let conn = database.conn();
        let mut tables = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        let names: Vec<String> = tables
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut dump = String::new();
        for name in names {
            let Ok(mut statement) = conn.prepare(&format!("SELECT * FROM \"{name}\"")) else {
                continue;
            };
            let columns = statement.column_count();
            let Ok(mut rows) = statement.query([]) else {
                continue;
            };
            while let Ok(Some(row)) = rows.next() {
                for column in 0..columns {
                    match row.get_ref(column) {
                        Ok(ValueRef::Text(text)) => {
                            dump.push_str(&String::from_utf8_lossy(text));
                        }
                        Ok(ValueRef::Blob(bytes)) => {
                            dump.push_str(&String::from_utf8_lossy(bytes));
                        }
                        Ok(ValueRef::Integer(value)) => dump.push_str(&value.to_string()),
                        _ => {}
                    }
                    dump.push('\n');
                }
            }
        }
        dump
    }
}
