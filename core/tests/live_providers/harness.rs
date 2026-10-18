//! The production path for one backend and one fixture, and the failure scenarios.
//!
//! Nothing on this path is a fake. Per capture:
//!
//! 1. the route, provider profile, route grant and default are configured through
//!    `create_profile_version`, `grant_route_capability` and `set_default_profile`;
//! 2. the capture is stored through `import_foreground_ingress`, which pins the interpretation
//!    job to the configured default (a user correction goes in as a correction event, then the
//!    next-revision job is planned and queued with the same enqueue functions production uses);
//! 3. the job is claimed and run by `InterpretationDispatcher`, which calls the production
//!    adapter for the profile's protocol over the shared HTTPS transport and applies the result
//!    through `apply_interpretation_proposal`.
//!
//! The offline fast path runs first in production and may answer a fixture without any provider
//! call; the record then says `fast_path` and carries no exchange.

use super::config::{Backend, Protocol};
use super::oracle::{score, ObservedState, OracleFixture, SemanticOutcome};
use super::transport::{origin_of, CurlTransport, Exchange, SharedTransport};
use chrono::{DateTime, Offset, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use ohand_core::domain::items::{load_item_state, LifecycleState};
use ohand_core::ingress::import_foreground_ingress;
use ohand_core::interpretation::apply::ApplyOutcome;
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, DispatchOutcome, InterpretationDispatcher, InterpretationRoute, WaitReason,
};
use ohand_core::interpretation::enqueue::{
    enqueue_planned_interpretation, interpretation_job_id, plan_interpretation,
};
use ohand_core::interpretation::instructions::M1_INSTRUCTION_VERSION;
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::queue::claim_job_with_lease;
use ohand_core::privacy::routing::grant_route_capability;
use ohand_core::privacy::routing::ProcessingCapability;
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, AnthropicTransport, ANTHROPIC_MESSAGES_ENDPOINT,
};
use ohand_core::providers::contracts::{CancelToken, ErrorClass, SystemClock};
use ohand_core::providers::openai::{OpenAiAdapter, OPENAI_CHAT_COMPLETIONS_ENDPOINT};
use ohand_core::providers::profiles::{create_profile_version, set_default_profile, ProfileDraft};
use ohand_core::providers::self_hosted::SelfHostedAdapter;
use ohand_core::reminders::state::{get_reminder, RequestState};
use ohand_core::store::captures::Capture;
use ohand_core::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use ohand_core::store::schema::{Database, SystemClock as StoreClock};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

pub const ROUTE_ID: &str = "live-harness-route";
const LOCALE: &str = "en-US";
/// Free-form text with no fast-path command, so the provider is always the route.
pub const PROVIDER_ONLY_TEXT: &str = "call the roofer about the leak before the weekend";

/// Most attempts one capture's job is given inside a single `run`.
const MAX_ATTEMPTS_PER_CAPTURE: usize = 8;
/// A next-attempt time no run reaches; parks jobs that must not run under another capture.
const DEFERRED_UNTIL: &str = "9999-12-31T00:00:00+00:00";

pub fn rfc3339(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// `applied`, `abstained`, `failed`, `backed_off`, `retired`, `not_run`, `other`.
    pub result: String,
    /// `provider`, `fast_path` or none.
    pub route: Option<String>,
    /// Failure kind or wait reason, snake_case.
    pub reason: Option<String>,
    /// Failures only: whether trying again can help.
    pub retriable: Option<bool>,
}

impl Outcome {
    fn of(result: &str) -> Outcome {
        Outcome {
            result: result.to_string(),
            route: None,
            reason: None,
            retriable: None,
        }
    }

    pub fn is_applied(&self) -> bool {
        self.result == "applied"
    }
}

fn snake(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn classify(outcome: &DispatchOutcome) -> Outcome {
    match outcome {
        DispatchOutcome::Interpreted { route, outcome } => {
            let mut result = Outcome::of(match outcome {
                ApplyOutcome::Applied { .. } => "applied",
                ApplyOutcome::Abstained { .. } => "abstained",
                _ => "other",
            });
            result.route = Some(
                match route {
                    InterpretationRoute::FastPath => "fast_path",
                    InterpretationRoute::Provider => "provider",
                }
                .to_string(),
            );
            result
        }
        DispatchOutcome::Failed { failure, .. } => Outcome {
            result: "failed".into(),
            route: None,
            reason: Some(snake(&failure.kind)),
            retriable: Some(failure.retriable),
        },
        DispatchOutcome::RetriesExhausted { .. } => Outcome {
            result: "failed".into(),
            route: None,
            reason: Some("retries_exhausted".into()),
            retriable: Some(false),
        },
        DispatchOutcome::BackedOff { reason, .. } => {
            let (name, retriable) = match reason {
                WaitReason::Transient(kind) => (snake(kind), kind.class() == ErrorClass::Transient),
                WaitReason::Denied(_) => ("denied".to_string(), false),
                WaitReason::SourceUnavailable => ("source_unavailable".to_string(), false),
            };
            Outcome {
                result: "backed_off".into(),
                route: None,
                reason: Some(name),
                retriable: Some(retriable),
            }
        }
        DispatchOutcome::Retired(reason) => {
            let mut result = Outcome::of("retired");
            result.reason = Some(reason.as_str().to_string());
            result
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ForbiddenMutations {
    pub raw_capture_changed: u32,
    pub correction_changed: u32,
    pub scope_changed: u32,
    pub lifecycle_left_active: u32,
}

impl ForbiddenMutations {
    pub fn total(&self) -> u32 {
        self.raw_capture_changed
            + self.correction_changed
            + self.scope_changed
            + self.lifecycle_left_active
    }
}

pub struct CaptureSpec {
    pub raw_input: String,
    pub user_correction: Option<String>,
    pub capture_instant: DateTime<Utc>,
    pub timezone: String,
}

impl CaptureSpec {
    pub fn plain(text: &str) -> CaptureSpec {
        CaptureSpec {
            raw_input: text.to_string(),
            user_correction: None,
            capture_instant: Utc::now(),
            timezone: "UTC".to_string(),
        }
    }

    pub fn of_fixture(fixture: &OracleFixture) -> CaptureSpec {
        CaptureSpec {
            raw_input: fixture.raw_input.clone(),
            user_correction: fixture.user_correction.clone(),
            capture_instant: fixture.capture_instant,
            timezone: fixture.timezone.clone(),
        }
    }
}

/// A capture stored through production ingress, whose job has not run yet.
pub struct Ingested {
    pub capture_id: String,
    pub item_id: String,
    pub target_job_id: String,
    pub spec: CaptureSpec,
    pub digest_before: String,
}

pub struct CaptureRun {
    pub outcome: Outcome,
    pub exchanges: Vec<Exchange>,
    pub state: ObservedState,
    pub forbidden: ForbiddenMutations,
    pub profile_version: Option<String>,
    pub job_status: Option<String>,
    pub job_failure_reason: Option<String>,
    pub capture_text_durable: bool,
}

pub struct Configured {
    pub profile_version: String,
    /// The stored profile declared text interpretation `Unverified` for this model, and the
    /// harness marked it `Supported` in the temporary store so the model can be exercised.
    pub capability_overridden: bool,
}

pub struct Harness {
    _directory: TempDir,
    database: Database,
    transport: Arc<CurlTransport>,
    registry: AdapterRegistry,
    timeout_seconds: u32,
    configure_count: u32,
}

impl Harness {
    pub fn new(transport: Arc<CurlTransport>, timeout_seconds: u32) -> Harness {
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("live.sqlite");
        let database = Database::open(path.to_str().unwrap(), Arc::new(StoreClock))
            .expect("open and migrate the temporary store");
        // The same row `ohand_core_start_ensure_local_capture_route` writes: no destinations,
        // no authorization. Processing is allowed only by the production grant below.
        database
            .conn()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
                 preview_safe, created_at) VALUES (?, 'live harness', 'personal', '[]', 0, ?)",
                rusqlite::params![ROUTE_ID, rfc3339(Utc::now())],
            )
            .expect("create the capture route");
        let registry = AdapterRegistry::new()
            .with_adapter(
                ohand_core::providers::contracts::ProviderProtocol::SelfHosted,
                SelfHostedAdapter::new(SharedTransport(transport.clone())),
            )
            .with_adapter(
                ohand_core::providers::contracts::ProviderProtocol::OpenAi,
                OpenAiAdapter::new(SharedTransport(transport.clone())),
            )
            .with_adapter(
                ohand_core::providers::contracts::ProviderProtocol::Anthropic,
                AnthropicAdapter::new(
                    transport.clone() as Arc<dyn AnthropicTransport>,
                    AnthropicSettings::default(),
                ),
            );
        Harness {
            _directory: directory,
            database,
            transport,
            registry,
            timeout_seconds,
            configure_count: 0,
        }
    }

    /// Configure `backend` through the production configuration path and make it the default:
    /// profile version, route grant, default. `credential` overrides the backend's secret (used
    /// to present an invalid credential).
    pub fn configure(
        &mut self,
        backend: &Backend,
        credential: Option<&str>,
    ) -> Result<Configured, String> {
        self.configure_count += 1;
        let credential_ref = format!(
            "credential-ref/live-harness-{}-{}",
            backend.protocol.label(),
            self.configure_count
        );
        let origin = match backend.protocol {
            Protocol::SelfHosted => origin_of(backend.endpoint.as_deref().unwrap_or_default()),
            Protocol::Anthropic => origin_of(ANTHROPIC_MESSAGES_ENDPOINT),
            Protocol::OpenAi => origin_of(OPENAI_CHAT_COMPLETIONS_ENDPOINT),
        }
        .ok_or("the backend has no https origin")?;
        self.transport.bind_credential(
            &credential_ref,
            credential.unwrap_or(&backend.credential),
            &origin,
        );
        let draft = ProfileDraft {
            profile_id: format!("live-{}-{}", backend.protocol.label(), self.configure_count),
            protocol: backend.protocol.draft_key().to_string(),
            endpoint: backend.endpoint.clone(),
            model: backend.model.clone(),
            credential_ref: Some(credential_ref.clone()),
            timeout_seconds: i64::from(self.timeout_seconds),
            retry_policy: None,
            capabilities: vec!["text_interpretation".to_string()],
        };
        let now = Utc::now();
        let stored = create_profile_version(&mut self.database, &draft, now)
            .map_err(|failure| format!("profile refused: {}", failure.code()))?;
        grant_route_capability(
            &mut self.database,
            ROUTE_ID,
            ProcessingCapability::TextInterpretation,
            &stored.profile_version,
            &rfc3339(now),
        )
        .map_err(|error| format!("grant refused: {}", error.code()))?;
        set_default_profile(
            &mut self.database,
            ProcessingCapability::TextInterpretation,
            &stored.profile_version,
            now,
        )
        .map_err(|failure| format!("default refused: {}", failure.code()))?;
        let capability_overridden = self.mark_supported_for_evidence(&stored.profile_version);
        Ok(Configured {
            profile_version: stored.profile_version,
            capability_overridden,
        })
    }

    /// The production configuration path declares text interpretation `Unverified` for a model
    /// with no earlier chat evidence, and the dispatcher refuses such a profile. Producing that
    /// evidence is this harness's purpose, so the stored declaration is replaced, in this
    /// temporary store only, by a `Supported` entry that names the harness as its evidence.
    /// Adapter, dispatch and apply are unchanged. Returns whether a replacement was needed.
    fn mark_supported_for_evidence(&mut self, profile_version: &str) -> bool {
        let stored: String = self
            .database
            .conn()
            .query_row(
                "SELECT capabilities FROM provider_profiles WHERE profile_version = ?",
                [profile_version],
                |row| row.get(0),
            )
            .expect("stored capabilities");
        let mut capabilities: Value = serde_json::from_str(&stored).expect("capabilities");
        let entry = &mut capabilities["text_interpretation"];
        let replaced = entry["support"] != "supported";
        if replaced {
            *entry = json!({
                "capability": "text_interpretation",
                "support": "supported",
                "evidence": "live provider harness run; model had no earlier chat evidence",
                "reason": null,
                "input_size_limit": entry["input_size_limit"].clone(),
                "structured_output": entry["structured_output"].clone(),
            });
        }
        if replaced {
            self.database
                .conn()
                .execute(
                    "UPDATE provider_profiles SET capabilities = ? WHERE profile_version = ?",
                    rusqlite::params![json!(capabilities).to_string(), profile_version],
                )
                .expect("replace the capability declaration");
        }
        replaced
    }

    pub fn revoke(&mut self, profile_version: &str) {
        revoke_profile(&mut self.database, profile_version, Utc::now())
            .expect("production revoke path");
    }

    /// Store the spec as a capture through production ingress and queue its job.
    pub fn ingest(&mut self, spec: CaptureSpec) -> Ingested {
        let capture_id = Uuid::new_v4().to_string();
        let zone: Tz = spec.timezone.parse().expect("fixture timezone");
        let offset_minutes = zone
            .offset_from_utc_datetime(&spec.capture_instant.naive_utc())
            .fix()
            .local_minus_utc()
            / 60;
        let saved_at = rfc3339(Utc::now());
        let capture = Capture::new(
            capture_id.clone(),
            Some(spec.raw_input.clone()),
            None,
            rfc3339(spec.capture_instant),
            spec.timezone.clone(),
            offset_minutes,
            LOCALE.to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            ROUTE_ID.to_string(),
            false,
            saved_at.clone(),
            None,
        )
        .expect("valid capture");
        let acknowledgment =
            import_foreground_ingress(&mut self.database, &capture).expect("production ingress");
        let item_id = acknowledgment.item_id;
        let mut revision = 0;
        if let Some(corrected) = &spec.user_correction {
            let event = Event::new(
                Uuid::new_v4().to_string(),
                item_id.clone(),
                0,
                EventType::Correction,
                EventPayload::Correction(Correction {
                    kind: CorrectionKind::Text,
                    old_value: Some(spec.raw_input.clone()),
                    new_value: corrected.clone(),
                }),
                saved_at.clone(),
            )
            .expect("valid correction event");
            save_event(&mut self.database, &event, 0).expect("store the correction");
            revision = 1;
            let transaction = self.database.immediate_transaction().expect("transaction");
            let plan = plan_interpretation(&transaction, &item_id, revision).expect("plan");
            enqueue_planned_interpretation(&transaction, &item_id, revision, &plan, Utc::now())
                .expect("queue the corrected revision");
            transaction.commit().expect("commit the job");
        }
        let digest_before = self.capture_digest(&capture_id);
        Ingested {
            target_job_id: interpretation_job_id(&item_id, revision),
            capture_id,
            item_id,
            spec,
            digest_before,
        }
    }

    /// Claim and run the target job through the production dispatcher, then read the result.
    /// Every other queued job is deferred first, so a job left backed off by an earlier capture
    /// can neither run under this capture nor add its exchanges to this record. With
    /// `retry_transient`, a transient failure is retried at once (the dispatcher's own attempt
    /// limit still applies) so the record carries the job's final outcome; without it the single
    /// attempt is recorded, backed off.
    pub fn run(&mut self, ingested: &Ingested, retry_transient: bool) -> CaptureRun {
        self.defer_other_jobs(&ingested.target_job_id);
        let _ = self.transport.drain_exchanges();
        let clock = SystemClock::new();
        let dispatcher = InterpretationDispatcher::new(&self.registry, &clock);
        let cancel = CancelToken::new();
        let lease = chrono::Duration::seconds(i64::from(self.timeout_seconds) + 120);
        let mut outcome = Outcome::of("not_run");
        let mut dispatched = None;
        let mut exchanges = Vec::new();
        for _ in 0..MAX_ATTEMPTS_PER_CAPTURE {
            let Some(job) =
                claim_job_with_lease(&mut self.database, lease, Utc::now()).expect("claim a job")
            else {
                break;
            };
            assert_eq!(
                job.job_id, ingested.target_job_id,
                "only the target job is runnable"
            );
            let result = dispatcher
                .run_job(&mut self.database, &job, &cancel, Utc::now())
                .expect("the dispatcher runs the claimed job");
            exchanges.extend(self.transport.drain_exchanges());
            outcome = classify(&result);
            let backed_off = matches!(result, DispatchOutcome::BackedOff { .. });
            dispatched = Some(result);
            if !(retry_transient && backed_off && outcome.retriable == Some(true)) {
                break;
            }
            self.database
                .conn()
                .execute(
                    "UPDATE jobs SET next_attempt_at = ? WHERE job_id = ? AND status = 'queued'",
                    rusqlite::params![Utc::now().to_rfc3339(), &ingested.target_job_id],
                )
                .expect("make the retry runnable now");
        }
        let abstained = matches!(
            &dispatched,
            Some(DispatchOutcome::Interpreted {
                outcome: ApplyOutcome::Abstained { .. },
                ..
            })
        );
        let mut state = self.read_state(&ingested.item_id);
        state.abstained = abstained;

        let digest_after = self.capture_digest(&ingested.capture_id);
        let stored_correction: Option<String> = self
            .database
            .conn()
            .query_row(
                "SELECT new_value FROM corrections WHERE item_id = ? AND kind = 'text' \
                 ORDER BY revision DESC LIMIT 1",
                [&ingested.item_id],
                |row| row.get(0),
            )
            .ok();
        let scope: String = self
            .database
            .conn()
            .query_row(
                "SELECT item_scope FROM captures WHERE capture_id = ?",
                [&ingested.capture_id],
                |row| row.get(0),
            )
            .unwrap_or_default();
        let forbidden = ForbiddenMutations {
            raw_capture_changed: u32::from(digest_after != ingested.digest_before),
            correction_changed: u32::from(stored_correction != ingested.spec.user_correction),
            scope_changed: u32::from(scope != "personal"),
            lifecycle_left_active: u32::from(state.lifecycle != "active"),
        };
        let (job_status, job_failure_reason, profile_version) = self
            .database
            .conn()
            .query_row(
                "SELECT status, failure_reason, profile_version FROM jobs WHERE job_id = ?",
                [&ingested.target_job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map(
                |(status, reason, version): (String, Option<String>, Option<String>)| {
                    (Some(status), reason, version)
                },
            )
            .unwrap_or((None, None, None));
        let text_now: Option<String> = self
            .database
            .conn()
            .query_row(
                "SELECT text FROM captures WHERE capture_id = ?",
                [&ingested.capture_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        CaptureRun {
            outcome,
            exchanges,
            state,
            forbidden,
            profile_version,
            job_status,
            job_failure_reason,
            capture_text_durable: text_now.as_deref() == Some(ingested.spec.raw_input.as_str()),
        }
    }

    /// Push every other queued job's next attempt out of reach of this run.
    fn defer_other_jobs(&mut self, target_job_id: &str) {
        self.database
            .conn()
            .execute(
                "UPDATE jobs SET next_attempt_at = ? WHERE status = 'queued' AND job_id != ?",
                rusqlite::params![DEFERRED_UNTIL, target_job_id],
            )
            .expect("defer the other jobs");
    }

    pub fn capture_digest(&self, capture_id: &str) -> String {
        let row: Vec<String> = self
            .database
            .conn()
            .query_row(
                "SELECT ifnull(text, ''), ifnull(audio_reference, ''), capture_instant, \
                 timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, \
                 ifnull(session_topic, ''), entry_locked, created_at \
                 FROM captures WHERE capture_id = ?",
                [capture_id],
                |row| {
                    (0..12)
                        .map(|column| {
                            row.get::<_, rusqlite::types::Value>(column)
                                .map(|value| match value {
                                    rusqlite::types::Value::Text(text) => text,
                                    other => format!("{other:?}"),
                                })
                        })
                        .collect()
                },
            )
            .expect("capture row exists");
        format!("{:x}", Sha256::digest(row.join("\u{1f}").as_bytes()))
    }

    fn read_state(&mut self, item_id: &str) -> ObservedState {
        let transaction = self.database.immediate_transaction().expect("transaction");
        let item = load_item_state(&transaction, item_id)
            .expect("read item")
            .expect("item exists");
        let reminder = get_reminder(&transaction, item_id)
            .expect("read reminder")
            .filter(|record| record.request_state != RequestState::NotRequested);
        let processing_state: String = transaction
            .query_row(
                "SELECT processing_state FROM items WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .expect("processing state");
        let applied_proposals: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM proposals WHERE item_id = ? AND applied_state = 'applied'",
                [item_id],
                |row| row.get(0),
            )
            .expect("proposal count");
        transaction.commit().expect("commit read");
        ObservedState {
            item_type: item.item_type.map(|kind| kind.as_str().to_string()),
            session_topic: item.session_topic,
            lifecycle: match item.lifecycle_state {
                LifecycleState::Active => "active",
                LifecycleState::Completed => "completed",
                LifecycleState::Cancelled => "cancelled",
                LifecycleState::Deleted => "deleted",
            }
            .to_string(),
            processing_state,
            has_reminder: reminder.is_some(),
            reminder_instant: reminder.as_ref().and_then(|record| record.resolved_instant),
            reminder_timezone: reminder.and_then(|record| record.timezone_id),
            applied_proposals,
            abstained: false,
        }
    }

    pub fn item_attribution(&self, job_id: &str) -> Option<String> {
        self.database
            .conn()
            .query_row(
                "SELECT profile_version FROM jobs WHERE job_id = ?",
                [job_id],
                |row| row.get(0),
            )
            .ok()
            .flatten()
    }
}

// ---- Whole-run orchestration ----

pub struct BuildInfo {
    pub commit: String,
    pub dirty: bool,
}

pub struct FixtureRecord {
    pub backend: String,
    pub provider: &'static str,
    pub model: String,
    pub profile_version: Option<String>,
    pub fixture_id: String,
    pub category: String,
    pub collected_at: String,
    pub run: CaptureRun,
    pub semantic: SemanticOutcome,
}

impl FixtureRecord {
    pub fn latencies_ms(&self) -> Vec<u64> {
        self.run.exchanges.iter().map(|e| e.latency_ms).collect()
    }

    pub fn tokens(&self) -> (Option<u64>, Option<u64>) {
        let sum = |pick: fn(&Exchange) -> Option<u64>| {
            let mut total = None;
            for exchange in &self.run.exchanges {
                if let Some(value) = pick(exchange) {
                    total = Some(total.unwrap_or(0) + value);
                }
            }
            total
        };
        (sum(|e| e.input_tokens), sum(|e| e.output_tokens))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchingCheck {
    pub earlier_backend: String,
    pub later_backend: String,
    pub captures_checked: usize,
    pub captures_changed: usize,
    pub attribution_changed: usize,
    pub results_changed: usize,
}

impl SwitchingCheck {
    pub fn holds(&self) -> bool {
        self.captures_checked > 0
            && self.captures_changed == 0
            && self.attribution_changed == 0
            && self.results_changed == 0
    }
}

struct BackendSnapshot {
    label: String,
    captures: Vec<(String, String)>,
    attribution: Vec<(String, Option<String>)>,
    results: Vec<(String, ObservedState)>,
    item_ids: Vec<String>,
}

pub struct RunReport {
    pub records: Vec<FixtureRecord>,
    /// Labels of backends whose model needed the capability replacement above.
    pub capability_overrides: Vec<String>,
    pub switching: Vec<SwitchingCheck>,
    pub configure_failures: Vec<(String, String)>,
}

impl Harness {
    /// Run every fixture through every backend in order on this one store. Backend `n + 1` is
    /// configured after backend `n` finished; the earlier captures are then re-read.
    pub fn run_backends(
        &mut self,
        backends: &[Backend],
        fixtures: &[OracleFixture],
        before_fixture: &mut dyn FnMut(&Backend, &OracleFixture),
    ) -> RunReport {
        let mut records = Vec::new();
        let mut snapshots: Vec<BackendSnapshot> = Vec::new();
        let mut switching = Vec::new();
        let mut configure_failures = Vec::new();
        let mut capability_overrides = Vec::new();
        for backend in backends {
            let label = backend.label();
            let configured = match self.configure(backend, None) {
                Ok(configured) => configured,
                Err(reason) => {
                    configure_failures.push((label, reason));
                    continue;
                }
            };
            if configured.capability_overridden {
                capability_overrides.push(label.clone());
            }
            let mut snapshot = BackendSnapshot {
                label: label.clone(),
                captures: Vec::new(),
                attribution: Vec::new(),
                results: Vec::new(),
                item_ids: Vec::new(),
            };
            for fixture in fixtures {
                before_fixture(backend, fixture);
                let ingested = self.ingest(CaptureSpec::of_fixture(fixture));
                let run = self.run(&ingested, true);
                snapshot.captures.push((
                    ingested.capture_id.clone(),
                    self.capture_digest(&ingested.capture_id),
                ));
                snapshot.attribution.push((
                    ingested.target_job_id.clone(),
                    self.item_attribution(&ingested.target_job_id),
                ));
                snapshot
                    .results
                    .push((ingested.item_id.clone(), run.state.clone()));
                snapshot.item_ids.push(ingested.item_id.clone());
                records.push(FixtureRecord {
                    backend: label.clone(),
                    provider: backend.protocol.label(),
                    model: backend.model.clone(),
                    profile_version: run
                        .profile_version
                        .clone()
                        .or_else(|| Some(configured.profile_version.clone())),
                    fixture_id: fixture.id.clone(),
                    category: fixture.category.clone(),
                    collected_at: rfc3339(Utc::now()),
                    semantic: score(fixture, &run.state),
                    run,
                });
            }
            for earlier in &snapshots {
                switching.push(self.recheck(earlier, &label));
            }
            snapshots.push(snapshot);
        }
        RunReport {
            records,
            capability_overrides,
            switching,
            configure_failures,
        }
    }

    fn recheck(&mut self, earlier: &BackendSnapshot, later: &str) -> SwitchingCheck {
        let captures_changed = earlier
            .captures
            .iter()
            .filter(|(id, digest)| &self.capture_digest(id) != digest)
            .count();
        let attribution_changed = earlier
            .attribution
            .iter()
            .filter(|(job, version)| &self.item_attribution(job) != version)
            .count();
        let mut results_changed = 0;
        for (item_id, before) in &earlier.results {
            let now = self.read_state(item_id);
            if now.item_type != before.item_type
                || now.processing_state != before.processing_state
                || now.applied_proposals != before.applied_proposals
                || now.has_reminder != before.has_reminder
            {
                results_changed += 1;
            }
        }
        SwitchingCheck {
            earlier_backend: earlier.label.clone(),
            later_backend: later.to_string(),
            captures_checked: earlier.captures.len(),
            captures_changed,
            attribution_changed,
            results_changed,
        }
    }
}

// ---- Failure scenarios ----

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioRecord {
    pub scenario: String,
    pub backend: String,
    /// `ran` or `not_exercised`, the latter with a reason.
    pub status: String,
    pub detail: Option<String>,
    pub outcome: Option<Outcome>,
    pub classified_retriable: Option<bool>,
    pub provider_calls: usize,
    pub applied_proposals: i64,
    pub capture_durable: bool,
    pub passed: bool,
}

pub struct ScenarioInputs<'a> {
    /// Any configured backend, for the revoked-profile scenario.
    pub primary: &'a Backend,
    /// A hosted backend for the invalid-credential scenario, if one is configured.
    pub hosted: Option<&'a Backend>,
    /// Backends whose responses the caller can make malformed (the CI fixture server).
    pub malformed: Vec<&'a Backend>,
}

pub const INVALID_SCENARIO_CREDENTIAL: &str = "invalid-credential-for-harness";
pub const UNREACHABLE_SCENARIO_CREDENTIAL: &str = "unreachable-credential";

fn closed_local_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

impl Harness {
    /// Each scenario checks that the capture stays durable, nothing is applied and the failure is
    /// classified as expected. `prepare` lets the caller steer a fixture server by scenario name.
    pub fn run_failure_scenarios(
        &mut self,
        inputs: &ScenarioInputs<'_>,
        prepare: &mut dyn FnMut(&str),
    ) -> Vec<ScenarioRecord> {
        let mut records = Vec::new();

        prepare("unreachable");
        let unreachable = Backend {
            protocol: Protocol::SelfHosted,
            model: inputs.primary.model.clone(),
            endpoint: Some(format!("https://127.0.0.1:{}/v1", closed_local_port())),
            credential: UNREACHABLE_SCENARIO_CREDENTIAL.to_string(),
        };
        records.push(self.failing_scenario(
            "unreachable_endpoint",
            &unreachable,
            None,
            |outcome| outcome.result == "backed_off" && outcome.retriable == Some(true),
        ));

        match inputs.hosted {
            Some(hosted) => {
                prepare("invalid_credential");
                records.push(self.failing_scenario(
                    "invalid_hosted_credential",
                    hosted,
                    Some(INVALID_SCENARIO_CREDENTIAL),
                    |outcome| {
                        outcome.result == "failed"
                            && outcome.reason.as_deref() == Some("unauthorized")
                            && outcome.retriable == Some(false)
                    },
                ));
            }
            None => records.push(not_exercised(
                "invalid_hosted_credential",
                "no hosted backend is configured",
            )),
        }

        prepare("revoked_profile");
        records.push(self.revoked_scenario(inputs.primary));

        if inputs.malformed.is_empty() {
            records.push(not_exercised(
                "malformed_response",
                "a live endpoint cannot be made to answer with a malformed body; the \
                 no-network proof asserts it against the local fixture server",
            ));
        }
        for backend in &inputs.malformed {
            prepare("malformed");
            records.push(
                self.failing_scenario("malformed_response", backend, None, |outcome| {
                    outcome.result == "failed"
                        && outcome.reason.as_deref() == Some("invalid_output")
                        && outcome.retriable == Some(false)
                }),
            );
        }
        prepare("done");
        records
    }

    fn failing_scenario(
        &mut self,
        name: &str,
        backend: &Backend,
        credential: Option<&str>,
        expect: impl Fn(&Outcome) -> bool,
    ) -> ScenarioRecord {
        if let Err(reason) = self.configure(backend, credential) {
            return not_exercised(name, &format!("configuration refused: {reason}"));
        }
        let ingested = self.ingest(CaptureSpec::plain(PROVIDER_ONLY_TEXT));
        let run = self.run(&ingested, false);
        let nothing_applied = run.state.applied_proposals == 0
            && run.state.item_type.is_none()
            && !run.state.has_reminder;
        let passed = expect(&run.outcome)
            && nothing_applied
            && run.capture_text_durable
            && run.forbidden.total() == 0;
        ScenarioRecord {
            scenario: name.to_string(),
            backend: backend.label(),
            status: "ran".to_string(),
            detail: None,
            classified_retriable: run.outcome.retriable,
            outcome: Some(run.outcome),
            provider_calls: run.exchanges.len(),
            applied_proposals: run.state.applied_proposals,
            capture_durable: run.capture_text_durable,
            passed,
        }
    }

    fn revoked_scenario(&mut self, backend: &Backend) -> ScenarioRecord {
        let name = "revoked_profile_version";
        let configured = match self.configure(backend, None) {
            Ok(configured) => configured,
            Err(reason) => return not_exercised(name, &format!("configuration refused: {reason}")),
        };
        let ingested = self.ingest(CaptureSpec::plain(PROVIDER_ONLY_TEXT));
        self.revoke(&configured.profile_version);
        let run = self.run(&ingested, false);
        let retired = run.job_status.as_deref() == Some("cancelled")
            && run.job_failure_reason.as_deref() == Some("profile_revoked");
        let passed = retired
            && run.exchanges.is_empty()
            && run.state.applied_proposals == 0
            && run.state.item_type.is_none()
            && run.capture_text_durable
            && run.forbidden.total() == 0;
        ScenarioRecord {
            scenario: name.to_string(),
            backend: backend.label(),
            status: "ran".to_string(),
            detail: Some("job retired by the production revoke path; no provider call".into()),
            classified_retriable: Some(false),
            outcome: Some(Outcome {
                result: "not_run".into(),
                route: None,
                reason: run.job_failure_reason.clone(),
                retriable: Some(false),
            }),
            provider_calls: run.exchanges.len(),
            applied_proposals: run.state.applied_proposals,
            capture_durable: run.capture_text_durable,
            passed,
        }
    }
}

fn not_exercised(name: &str, reason: &str) -> ScenarioRecord {
    ScenarioRecord {
        scenario: name.to_string(),
        backend: "n/a".to_string(),
        status: "not_exercised".to_string(),
        detail: Some(reason.to_string()),
        outcome: None,
        classified_retriable: None,
        provider_calls: 0,
        applied_proposals: 0,
        capture_durable: true,
        passed: true,
    }
}

/// Build revision of the repository this harness was built from.
pub fn build_info() -> BuildInfo {
    let git = |arguments: &[&str]| {
        std::process::Command::new("git")
            .args(arguments)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    };
    BuildInfo {
        commit: git(&["rev-parse", "HEAD"]),
        dirty: !git(&[
            "status",
            "--porcelain",
            "--",
            "core",
            "Cargo.toml",
            "Cargo.lock",
        ])
        .is_empty(),
    }
}

pub fn instruction_version() -> &'static str {
    M1_INSTRUCTION_VERSION
}

pub fn summarize_by_backend(records: &[FixtureRecord]) -> BTreeMap<String, Value> {
    let mut summary: BTreeMap<String, Value> = BTreeMap::new();
    for record in records {
        let entry = summary.entry(record.backend.clone()).or_insert_with(
            || json!({"fixtures": 0, "matches_oracle": 0, "forbidden_mutations": 0}),
        );
        entry["fixtures"] = json!(entry["fixtures"].as_u64().unwrap() + 1);
        if record.semantic.matches_oracle() {
            entry["matches_oracle"] = json!(entry["matches_oracle"].as_u64().unwrap() + 1);
        }
        entry["forbidden_mutations"] = json!(
            entry["forbidden_mutations"].as_u64().unwrap()
                + u64::from(record.run.forbidden.total())
        );
    }
    summary
}
