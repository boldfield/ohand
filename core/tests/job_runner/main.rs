//! Integration fixtures for the core job dispatch loop (J02a). Every test drains jobs from a
//! migrated, file-backed SQLite store through `JobRunner` against a fake clock, using the real
//! J01 queue, V03 configuration guards and I05/I06 apply operations, and asserts on durable state
//! re-read through a fresh connection.

use chrono::{DateTime, Duration, Utc};
use ohand_core::domain::status::{ItemStatus, UnschedulableReason};
use ohand_core::interpretation::dispatch::{
    AdapterRegistry, InterpretationDispatcher, RetireReason,
};
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::queue::{claim_job_with_lease, complete_job, enqueue_job, Job};
use ohand_core::jobs::runner::{
    CapabilityError, CapabilityOutcome, DrainReport, InterpretationCapability, JobCapabilities,
    JobCapability, JobResult, JobRunner, RunnerConfig, Settlement, StopReason,
    CAPABILITY_UNAVAILABLE_REASON, INTERNAL_ERROR_REASON, INTERRUPTED_REASON, NOT_SETTLED_REASON,
    RETRIES_EXHAUSTED_REASON,
};
use ohand_core::privacy::routing::JOB_TYPE_INTERPRET;
use ohand_core::providers::anthropic::fake::{FakeAnthropicStep, FakeAnthropicTransport};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    CancelToken, CapabilityMetadata, ManualClock, ProviderCapability, ProviderProfile,
    ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode, TransportError,
};
use ohand_core::store::schema::{Clock as StoreClock, Database};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const START: &str = "2026-01-15T10:30:00Z";
const ROUTE_ID: &str = "route-1";
const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const ANTHROPIC_MODEL: &str = "synthetic-anthropic-model";
const FREE_FORM: &str = "call the roofer about the leak";
const LEASE_SECONDS: i64 = 60;
const OTHER_JOB_TYPE: &str = "synthetic_other";

fn instant(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn start() -> DateTime<Utc> {
    instant(START)
}

// ---- Fake clock ----

struct TestClock {
    now: Mutex<DateTime<Utc>>,
    reads: Mutex<usize>,
    cancel_on_read: Mutex<Option<(usize, CancelToken)>>,
}

impl TestClock {
    fn at(time: DateTime<Utc>) -> Arc<TestClock> {
        Arc::new(TestClock {
            now: Mutex::new(time),
            reads: Mutex::new(0),
            cancel_on_read: Mutex::new(None),
        })
    }

    /// Fire `cancel` during the `read_number`-th reading of this clock from now on.
    fn cancel_on_read(&self, read_number: usize, cancel: &CancelToken) {
        *self.reads.lock().unwrap() = 0;
        *self.cancel_on_read.lock().unwrap() = Some((read_number, cancel.clone()));
    }

    fn set(&self, time: DateTime<Utc>) {
        *self.now.lock().unwrap() = time;
    }

    fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }
}

impl StoreClock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        let mut reads = self.reads.lock().unwrap();
        *reads += 1;
        if let Some((read_number, cancel)) = self.cancel_on_read.lock().unwrap().as_ref() {
            if *reads == *read_number {
                cancel.cancel();
            }
        }
        *self.now.lock().unwrap()
    }
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
            "job-runner-fixtures:job_runner",
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

fn annotate_action(text: &str, evidence: &str) -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [span_of(text, evidence)],
    })
}

/// A scripted Anthropic transport and the adapter registry that serves it.
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

    fn capabilities(&self) -> JobCapabilities<'_> {
        JobCapabilities::new().with(InterpretationCapability::new(
            InterpretationDispatcher::new(&self.registry, self.clock.as_ref()),
        ))
    }
}

// ---- Scripted generic capability ----

type Behaviour = Box<
    dyn FnMut(
        &mut Database,
        &Job,
        &CancelToken,
        DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError>,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    job_id: String,
    attempt: i32,
    at: DateTime<Utc>,
}

struct Scripted {
    job_type: &'static str,
    behaviour: RefCell<Behaviour>,
    calls: Rc<RefCell<Vec<Call>>>,
}

impl Scripted {
    fn new(
        job_type: &'static str,
        behaviour: impl FnMut(
                &mut Database,
                &Job,
                &CancelToken,
                DateTime<Utc>,
            ) -> Result<CapabilityOutcome, CapabilityError>
            + 'static,
    ) -> (Scripted, Rc<RefCell<Vec<Call>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        (
            Scripted {
                job_type,
                behaviour: RefCell::new(Box::new(behaviour)),
                calls: calls.clone(),
            },
            calls,
        )
    }
}

impl JobCapability for Scripted {
    fn job_type(&self) -> &str {
        self.job_type
    }

    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError> {
        self.calls.borrow_mut().push(Call {
            job_id: claimed.job_id.clone(),
            attempt: claimed.attempt_count,
            at: now,
        });
        (self.behaviour.borrow_mut())(db, claimed, cancel, now)
    }
}

/// Runs `hook` just before handing the job to the real interpretation capability, to stage what
/// happens elsewhere while the first holder is working.
struct Before<'a> {
    inner: InterpretationCapability<'a>,
    hook: RefCell<Box<dyn FnMut() + 'a>>,
}

impl JobCapability for Before<'_> {
    fn job_type(&self) -> &str {
        self.inner.job_type()
    }

    fn run(
        &self,
        db: &mut Database,
        claimed: &Job,
        cancel: &CancelToken,
        now: DateTime<Utc>,
    ) -> Result<CapabilityOutcome, CapabilityError> {
        (self.hook.borrow_mut())();
        self.inner.run(db, claimed, cancel, now)
    }
}

/// Completes the job through the real queue operation, as a capability applying its result would.
fn completing(
    db: &mut Database,
    job: &Job,
    _cancel: &CancelToken,
    _now: DateTime<Utc>,
) -> Result<CapabilityOutcome, CapabilityError> {
    complete_job(db, &job.job_id, job.attempt_count).unwrap();
    Ok(CapabilityOutcome::Settled(Settlement::Completed))
}

// ---- Fixture ----

struct Fixture {
    db: Database,
    path: String,
    clock: Arc<TestClock>,
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
    revision: i32,
    proposals: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct JobRow {
    status: String,
    failure_reason: Option<String>,
    attempt_count: i32,
    transient_failure_count: i64,
    next_attempt_at: Option<DateTime<Utc>>,
}

fn config() -> RunnerConfig {
    RunnerConfig {
        lease_duration: Duration::seconds(LEASE_SECONDS),
        max_jobs_per_drain: 25,
        time_budget: None,
        max_attempts: 3,
        base_backoff: Duration::seconds(30),
        max_backoff: Duration::hours(1),
        capability_unavailable_delay: Duration::minutes(15),
    }
}

impl Fixture {
    fn new() -> Fixture {
        let path = format!(
            "{}/test_job_runner_{}.db",
            std::env::temp_dir().display(),
            Uuid::new_v4()
        );
        let clock = TestClock::at(start());
        let db = Database::open(&path, clock.clone()).unwrap();
        Fixture { db, path, clock }
    }

    fn reopened(&self) -> Database {
        Database::open(&self.path, self.clock.clone()).unwrap()
    }

    fn add_item(&self, capture_text: &str) -> String {
        let item_id = Uuid::new_v4().to_string();
        let capture_id = Uuid::new_v4().to_string();
        self.db
            .conn()
            .execute(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, \
                 utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) \
                 VALUES (?, ?, ?, 'UTC', 0, 'en', 'gregorian', 'personal', ?, 0, ?)",
                rusqlite::params![capture_id, capture_text, START, ROUTE_ID, START],
            )
            .unwrap();
        self.db
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
                 sync_state, processing_state, transcription_state, created_at, updated_at) \
                 VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', ?, ?)",
                rusqlite::params![item_id, capture_id, START, START],
            )
            .unwrap();
        item_id
    }

    fn set_up_provider(&self, profile: &ProviderProfile) {
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
                    START,
                ],
            )
            .unwrap();
        let origins = serde_json::to_string(&[ANTHROPIC_ORIGIN]).unwrap();
        self.db
            .conn()
            .execute(
                "INSERT OR IGNORE INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
                 VALUES (?, 'private notes', 'personal', ?, ?)",
                rusqlite::params![ROUTE_ID, origins, START],
            )
            .unwrap();
        self.db
            .conn()
            .execute(
                "INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, created_at) \
                 VALUES (?, ?, 'text_interpretation', ?, ?)",
                rusqlite::params![Uuid::new_v4().to_string(), ROUTE_ID, origins, START],
            )
            .unwrap();
    }

    fn enqueue(
        &mut self,
        item_id: &str,
        job_type: &str,
        profile: Option<&ProviderProfile>,
        schema_version: i32,
    ) -> String {
        let job_id = Uuid::new_v4().to_string();
        enqueue_job(
            &mut self.db,
            job_id.clone(),
            item_id.to_string(),
            job_type.to_string(),
            0,
            profile.map(|profile| profile.profile_version().to_string()),
            Some(Uuid::new_v4().to_string()),
            schema_version,
            self.clock.now(),
        )
        .unwrap();
        // Distinct creation times keep the queue's oldest-first order deterministic.
        self.clock.advance(Duration::milliseconds(1));
        job_id
    }

    fn enqueue_interpretation(&mut self, item_id: &str, profile: &ProviderProfile) -> String {
        self.enqueue(item_id, JOB_TYPE_INTERPRET, Some(profile), 1)
    }

    fn run(
        &mut self,
        capabilities: JobCapabilities<'_>,
        run_config: RunnerConfig,
        cancel: &CancelToken,
    ) -> DrainReport {
        let clock = self.clock.clone();
        let report =
            JobRunner::new(capabilities, clock.as_ref(), run_config).drain(&mut self.db, cancel);
        report
    }

    fn drain(&mut self, capabilities: JobCapabilities<'_>) -> DrainReport {
        self.run(capabilities, config(), &CancelToken::new())
    }

    fn job(&self, job_id: &str) -> JobRow {
        self.reopened()
            .conn()
            .query_row(
                "SELECT status, failure_reason, attempt_count, transient_failure_count, next_attempt_at \
                 FROM jobs WHERE job_id = ?",
                [job_id],
                |row| {
                    let next_attempt_at: Option<String> = row.get(4)?;
                    Ok(JobRow {
                        status: row.get(0)?,
                        failure_reason: row.get(1)?,
                        attempt_count: row.get(2)?,
                        transient_failure_count: row.get(3)?,
                        next_attempt_at: next_attempt_at.map(|text| instant(&text)),
                    })
                },
            )
            .unwrap()
    }

    fn durable(&self, item_id: &str) -> Durable {
        let db = self.reopened();
        let (capture_text, item_type, processing_state, revision): (
            String,
            Option<String>,
            String,
            i32,
        ) = db
            .conn()
            .query_row(
                "SELECT c.text, i.item_type, i.processing_state, i.revision \
                 FROM items i JOIN captures c ON c.capture_id = i.capture_id WHERE i.item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        let proposals: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM proposals WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .unwrap();
        Durable {
            capture_text,
            item_type,
            processing_state,
            revision,
            proposals,
        }
    }

    fn reminder(&self, item_id: &str) -> Option<(String, Option<String>)> {
        use rusqlite::OptionalExtension;
        self.reopened()
            .conn()
            .query_row(
                "SELECT request_state, resolved_instant FROM reminders WHERE item_id = ?",
                [item_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.reopened()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}

fn results(report: &DrainReport) -> Vec<&JobResult> {
    report.jobs.iter().map(|job| &job.result).collect()
}

// ---- Lease recovery ----

#[test]
fn a_job_whose_holder_vanished_is_recovered_after_its_lease_and_applied_once() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let text = FREE_FORM;
    let item_id = fixture.add_item(text);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);
    let providers = Providers::new(vec![anthropic_reply(annotate_action(text, "roofer"))]);

    // A worker claims the job and then disappears without settling it.
    let abandoned = claim_job_with_lease(
        &mut fixture.db,
        Duration::seconds(LEASE_SECONDS),
        fixture.clock.now(),
    )
    .unwrap()
    .expect("job is claimable");
    assert_eq!(abandoned.attempt_count, 1);

    fixture.clock.advance(Duration::seconds(LEASE_SECONDS - 1));
    let early = fixture.drain(providers.capabilities());
    assert_eq!(early.stop, StopReason::Idle);
    assert!(early.jobs.is_empty());
    assert_eq!(providers.calls(), 0);
    assert_eq!(fixture.job(&job_id).status, "running");

    fixture.clock.advance(Duration::seconds(2));
    let recovered = fixture.drain(providers.capabilities());

    assert_eq!(
        results(&recovered),
        vec![&JobResult::Settled(Settlement::Completed)]
    );
    assert_eq!(recovered.jobs[0].attempt, 2);
    assert_eq!(recovered.stop, StopReason::Idle);
    assert_eq!(providers.calls(), 1);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "completed");
    assert_eq!(row.attempt_count, 2);
    let durable = fixture.durable(&item_id);
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(durable.proposals, 1);
    assert_eq!(durable.capture_text, text);

    fixture.clock.advance(Duration::hours(1));
    let again = fixture.drain(providers.capabilities());
    assert!(again.jobs.is_empty());
    assert_eq!(providers.calls(), 1);
    assert_eq!(fixture.durable(&item_id).proposals, 1);
}

// ---- Duplicate results ----

#[test]
fn a_late_result_from_a_reclaimed_lease_is_rejected_and_the_new_holders_result_stands() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let text = FREE_FORM;
    let item_id = fixture.add_item(text);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);

    let first_providers = Providers::new(vec![anthropic_reply(annotate_action(text, "leak"))]);
    let second_providers = Providers::new(vec![anthropic_reply(annotate_action(text, "roofer"))]);
    let path = fixture.path.clone();
    let second_clock = TestClock::at(start() + Duration::seconds(LEASE_SECONDS * 2));

    // While the first holder is still working, its lease expires, a second worker reclaims the
    // job and applies its own result, and only then does the first holder's result arrive.
    let second_report: Rc<RefCell<Option<DrainReport>>> = Rc::new(RefCell::new(None));
    let racing = Before {
        inner: InterpretationCapability::new(InterpretationDispatcher::new(
            &first_providers.registry,
            first_providers.clock.as_ref(),
        )),
        hook: RefCell::new(Box::new({
            let second_report = second_report.clone();
            let second_providers = &second_providers;
            move || {
                let mut second_db = Database::open(&path, second_clock.clone()).unwrap();
                let report = JobRunner::new(
                    second_providers.capabilities(),
                    second_clock.as_ref(),
                    config(),
                )
                .drain(&mut second_db, &CancelToken::new());
                *second_report.borrow_mut() = Some(report);
            }
        })),
    };
    let report = fixture.drain(JobCapabilities::new().with(racing));

    assert_eq!(results(&report), vec![&JobResult::LeaseLost]);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "completed");
    assert_eq!(row.attempt_count, 2);
    let durable = fixture.durable(&item_id);
    assert_eq!(durable.proposals, 1);
    assert_eq!(durable.item_type.as_deref(), Some("action"));
    assert_eq!(first_providers.calls(), 0);
    assert_eq!(second_providers.calls(), 1);
    let second = second_report.borrow();
    assert_eq!(
        second.as_ref().map(results),
        Some(vec![&JobResult::Settled(Settlement::Completed)])
    );
}

#[test]
fn a_capability_that_returns_without_settling_the_job_spends_a_retry() {
    let mut fixture = Fixture::new();
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, |_, _, _, _| {
        Ok(CapabilityOutcome::Settled(Settlement::Completed))
    });

    let report = fixture.drain(JobCapabilities::new().with(capability));

    assert_eq!(calls.borrow().len(), 1);
    match results(&report)[..] {
        [JobResult::RetryScheduled { retry_at, failures }] => {
            assert_eq!(*failures, 1);
            assert_eq!(*retry_at, fixture.job(&job_id).next_attempt_at.unwrap());
        }
        ref other => panic!("{other:?}"),
    }
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some(NOT_SETTLED_REASON));
}

// ---- Retry limits ----

#[test]
fn transient_failures_back_off_exponentially_and_stop_at_the_limit() {
    let mut fixture = Fixture::new();
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1);
    let before = fixture.durable(&item_id);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, |_, _, _, _| {
        Ok(CapabilityOutcome::TransientFailure {
            reason: "unavailable".to_string(),
        })
    });
    let capabilities = JobCapabilities::new().with(capability);
    let runner_clock = fixture.clock.clone();
    let runner = JobRunner::new(capabilities, runner_clock.as_ref(), config());

    let first_at = fixture.clock.now();
    let first = runner.drain(&mut fixture.db, &CancelToken::new());
    assert_eq!(
        results(&first),
        vec![&JobResult::RetryScheduled {
            retry_at: first_at + Duration::seconds(60),
            failures: 1
        }]
    );
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some("unavailable"));
    assert_eq!(row.transient_failure_count, 1);

    let waiting = runner.drain(&mut fixture.db, &CancelToken::new());
    assert!(waiting.jobs.is_empty());
    assert_eq!(calls.borrow().len(), 1);

    fixture.clock.set(first_at + Duration::seconds(60));
    let second_at = fixture.clock.now();
    let second = runner.drain(&mut fixture.db, &CancelToken::new());
    assert_eq!(
        results(&second),
        vec![&JobResult::RetryScheduled {
            retry_at: second_at + Duration::seconds(120),
            failures: 2
        }]
    );

    fixture.clock.set(second_at + Duration::seconds(120));
    let third = runner.drain(&mut fixture.db, &CancelToken::new());
    assert_eq!(results(&third), vec![&JobResult::RetriesExhausted]);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some(RETRIES_EXHAUSTED_REASON)
    );
    assert_eq!(row.transient_failure_count, 3);

    fixture.clock.advance(Duration::days(30));
    let after = runner.drain(&mut fixture.db, &CancelToken::new());
    assert!(after.jobs.is_empty());
    assert_eq!(calls.borrow().len(), 3);
    assert_eq!(fixture.durable(&item_id), before);
}

#[test]
fn provider_outages_stop_at_the_pinned_limit_and_keep_the_capture() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    let max_attempts = profile.retry_policy().max_attempts;
    fixture.set_up_provider(&profile);
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);
    let providers = Providers::new(
        (0..max_attempts)
            .map(|_| FakeAnthropicStep::fail(TransportError::Unavailable))
            .collect(),
    );

    for attempt in 1..=max_attempts {
        let report = fixture.drain(providers.capabilities());
        assert_eq!(report.jobs.len(), 1, "attempt {attempt}");
        let result = &report.jobs[0].result;
        if attempt < max_attempts {
            assert!(
                matches!(result, JobResult::Settled(Settlement::BackedOff { .. })),
                "{result:?}"
            );
            let row = fixture.job(&job_id);
            assert_eq!(row.status, "queued");
            fixture
                .clock
                .set(row.next_attempt_at.expect("retry time") + Duration::seconds(1));
        } else {
            assert_eq!(result, &JobResult::Settled(Settlement::Failed));
        }
    }

    assert_eq!(providers.calls(), max_attempts as usize);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "failed");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some(RETRIES_EXHAUSTED_REASON)
    );
    let durable = fixture.durable(&item_id);
    assert_eq!(durable.capture_text, FREE_FORM);
    assert_eq!(durable.processing_state, "uninterpreted");
    assert_eq!(durable.item_type, None);
    assert_eq!(durable.proposals, 0);

    fixture.clock.advance(Duration::days(1));
    assert!(fixture.drain(providers.capabilities()).jobs.is_empty());
    assert_eq!(providers.calls(), max_attempts as usize);
}

#[test]
fn internal_faults_and_permanent_failures_are_settled_by_the_runner() {
    let mut fixture = Fixture::new();
    let item_id = fixture.add_item(FREE_FORM);
    let faulty = fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1);
    let other_item = fixture.add_item("another capture");
    let fatal = fixture.enqueue(&other_item, "synthetic_fatal", None, 1);
    let (faulty_capability, _) = Scripted::new(OTHER_JOB_TYPE, |_, _, _, _| {
        Err(CapabilityError::Internal(
            "synthetic storage fault".to_string(),
        ))
    });
    let (fatal_capability, _) = Scripted::new("synthetic_fatal", |_, _, _, _| {
        Ok(CapabilityOutcome::PermanentFailure {
            reason: "unreadable_input".to_string(),
        })
    });

    let report = fixture.drain(
        JobCapabilities::new()
            .with(faulty_capability)
            .with(fatal_capability),
    );

    assert_eq!(report.jobs.len(), 2);
    assert!(matches!(
        report.jobs[0].result,
        JobResult::RetryScheduled { failures: 1, .. }
    ));
    assert_eq!(
        report.jobs[1].result,
        JobResult::FailedPermanently {
            reason: "unreadable_input".to_string()
        }
    );
    let faulty_row = fixture.job(&faulty);
    assert_eq!(faulty_row.status, "queued");
    assert_eq!(
        faulty_row.failure_reason.as_deref(),
        Some(INTERNAL_ERROR_REASON)
    );
    let fatal_row = fixture.job(&fatal);
    assert_eq!(fatal_row.status, "failed");
    assert_eq!(
        fatal_row.failure_reason.as_deref(),
        Some("unreadable_input")
    );
    assert_eq!(fatal_row.transient_failure_count, 0);
}

// ---- Cancellation and checkpoints ----

#[test]
fn a_drain_cancelled_before_it_starts_claims_nothing() {
    let mut fixture = Fixture::new();
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, completing);
    let cancel = CancelToken::new();
    cancel.cancel();

    let report = fixture.run(JobCapabilities::new().with(capability), config(), &cancel);

    assert_eq!(report.stop, StopReason::Cancelled);
    assert!(report.jobs.is_empty());
    assert!(calls.borrow().is_empty());
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.attempt_count, 0);
}

#[test]
fn cancelling_mid_job_checkpoints_it_for_free_and_a_later_drain_resumes_it() {
    let mut fixture = Fixture::new();
    let first_item = fixture.add_item(FREE_FORM);
    let second_item = fixture.add_item("pick up the parcel");
    let first = fixture.enqueue(&first_item, OTHER_JOB_TYPE, None, 1);
    let second = fixture.enqueue(&second_item, OTHER_JOB_TYPE, None, 1);
    let mut interrupted_once = false;
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, move |db, job, cancel, now| {
        if !interrupted_once {
            interrupted_once = true;
            cancel.cancel();
            return Ok(CapabilityOutcome::Interrupted);
        }
        completing(db, job, cancel, now)
    });
    let cancel = CancelToken::new();
    let checkpoint_at = fixture.clock.now();

    let report = fixture.run(JobCapabilities::new().with(capability), config(), &cancel);

    assert_eq!(results(&report), vec![&JobResult::Interrupted]);
    assert_eq!(report.stop, StopReason::Cancelled);
    let row = fixture.job(&first);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some(INTERRUPTED_REASON));
    assert_eq!(row.transient_failure_count, 0);
    assert_eq!(row.next_attempt_at, Some(checkpoint_at));
    let untouched = fixture.job(&second);
    assert_eq!(untouched.status, "queued");
    assert_eq!(untouched.attempt_count, 0);

    let (resumed, resumed_calls) = Scripted::new(OTHER_JOB_TYPE, completing);
    let report = fixture.drain(JobCapabilities::new().with(resumed));
    assert_eq!(report.stop, StopReason::Idle);
    assert_eq!(report.jobs.len(), 2);
    assert_eq!(resumed_calls.borrow().len(), 2);
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(fixture.job(&first).status, "completed");
    assert_eq!(fixture.job(&first).attempt_count, 2);
    assert_eq!(fixture.job(&second).status, "completed");
}

#[test]
fn an_interrupted_job_ends_the_drain_even_when_the_token_never_fires() {
    let mut fixture = Fixture::new();
    let first_item = fixture.add_item(FREE_FORM);
    let second_item = fixture.add_item("pick up the parcel");
    let first = fixture.enqueue(&first_item, OTHER_JOB_TYPE, None, 1);
    let second = fixture.enqueue(&second_item, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, |_, _, _, _| {
        Ok(CapabilityOutcome::Interrupted)
    });
    let cancel = CancelToken::new();
    let mut limits = config();
    limits.max_jobs_per_drain = 3;

    let report = fixture.run(JobCapabilities::new().with(capability), limits, &cancel);

    assert!(!cancel.is_cancelled());
    assert_eq!(results(&report), vec![&JobResult::Interrupted]);
    assert_eq!(report.stop, StopReason::Interrupted);
    assert_eq!(calls.borrow().len(), 1);
    let interrupted = fixture.job(&first);
    assert_eq!(interrupted.status, "queued");
    assert_eq!(interrupted.attempt_count, 1);
    assert_eq!(interrupted.transient_failure_count, 0);
    let waiting = fixture.job(&second);
    assert_eq!(waiting.status, "queued");
    assert_eq!(waiting.attempt_count, 0);
}

#[test]
fn a_self_settled_interruption_also_ends_the_drain() {
    let mut fixture = Fixture::new();
    let first_item = fixture.add_item(FREE_FORM);
    let second_item = fixture.add_item("pick up the parcel");
    let first = fixture.enqueue(&first_item, OTHER_JOB_TYPE, None, 1);
    let second = fixture.enqueue(&second_item, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, |db, job, _, now| {
        // Put the job back under its own lease, as the interpretation dispatcher does on cancel.
        db.conn()
            .execute(
                "UPDATE jobs SET status = 'queued', next_attempt_at = ?, lease_expires_at = NULL \
                 WHERE job_id = ? AND status = 'running' AND attempt_count = ?",
                rusqlite::params![now.to_rfc3339(), job.job_id, job.attempt_count],
            )
            .expect("requeue under lease");
        Ok(CapabilityOutcome::Settled(Settlement::Interrupted))
    });
    let mut limits = config();
    limits.max_jobs_per_drain = 3;

    let report = fixture.run(
        JobCapabilities::new().with(capability),
        limits,
        &CancelToken::new(),
    );

    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Interrupted)]
    );
    assert_eq!(report.stop, StopReason::Interrupted);
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(fixture.job(&first).attempt_count, 1);
    assert_eq!(fixture.job(&second).attempt_count, 0);
}

#[test]
fn cancellation_during_a_job_stops_the_drain_before_the_next_claim() {
    let mut fixture = Fixture::new();
    let first_item = fixture.add_item(FREE_FORM);
    let second_item = fixture.add_item("pick up the parcel");
    fixture.enqueue(&first_item, OTHER_JOB_TYPE, None, 1);
    let second = fixture.enqueue(&second_item, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, |db, job, cancel, now| {
        cancel.cancel();
        completing(db, job, cancel, now)
    });

    let report = fixture.run(
        JobCapabilities::new().with(capability),
        config(),
        &CancelToken::new(),
    );

    assert_eq!(report.stop, StopReason::Cancelled);
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(report.jobs.len(), 1);
    assert_eq!(fixture.job(&second).attempt_count, 0);
}

#[test]
fn a_job_claimed_just_as_cancellation_arrives_is_put_back_without_running() {
    let mut fixture = Fixture::new();
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, completing);
    let cancel = CancelToken::new();
    // A drain reads the clock to start its budget (1), to claim (2) and to run the claimed job
    // (3); cancelling during the third reading is cancellation arriving after the claim.
    let claimed_at = fixture.clock.now();
    fixture.clock.cancel_on_read(3, &cancel);

    let report = fixture.run(JobCapabilities::new().with(capability), config(), &cancel);

    assert_eq!(results(&report), vec![&JobResult::Interrupted]);
    assert_eq!(report.stop, StopReason::Cancelled);
    assert!(calls.borrow().is_empty());
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.failure_reason.as_deref(), Some(INTERRUPTED_REASON));
    assert_eq!(row.attempt_count, 1);
    assert_eq!(row.transient_failure_count, 0);
    assert_eq!(row.next_attempt_at, Some(claimed_at));
}

#[test]
fn a_cancelled_provider_call_requeues_at_once_and_leaves_the_item_alone() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);
    let before = fixture.durable(&item_id);
    let providers = Providers::new(vec![
        anthropic_reply(annotate_action(FREE_FORM, "roofer")).cancelling_in_flight()
    ]);
    let checkpoint_at = fixture.clock.now();
    let cancel = CancelToken::new();

    let report = fixture.run(providers.capabilities(), config(), &cancel);

    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Interrupted)]
    );
    // The fake transport fires the shared token mid-call, which is what ended the call.
    assert!(cancel.is_cancelled());
    assert_eq!(report.stop, StopReason::Cancelled);
    assert_eq!(fixture.job(&job_id).attempt_count, 1);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "queued");
    assert_eq!(row.transient_failure_count, 0);
    assert_eq!(row.next_attempt_at, Some(checkpoint_at));
    assert_eq!(fixture.durable(&item_id), before);

    let resumed = Providers::new(vec![anthropic_reply(annotate_action(FREE_FORM, "roofer"))]);
    let report = fixture.drain(resumed.capabilities());
    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Completed)]
    );
    assert_eq!(fixture.durable(&item_id).proposals, 1);
}

// ---- Bounded work ----

#[test]
fn one_drain_never_claims_more_than_its_job_limit() {
    let mut fixture = Fixture::new();
    let mut job_ids = Vec::new();
    for index in 0..3 {
        let item_id = fixture.add_item(&format!("capture {index}"));
        job_ids.push(fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1));
    }
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, completing);
    let limited = RunnerConfig {
        max_jobs_per_drain: 2,
        ..config()
    };

    let report = fixture.run(
        JobCapabilities::new().with(capability),
        limited,
        &CancelToken::new(),
    );

    assert_eq!(report.stop, StopReason::JobLimit);
    assert_eq!(calls.borrow().len(), 2);
    assert_eq!(fixture.job(&job_ids[2]).status, "queued");
    assert_eq!(fixture.job(&job_ids[2]).attempt_count, 0);
}

#[test]
fn one_drain_stops_claiming_once_its_time_budget_is_spent() {
    let mut fixture = Fixture::new();
    let mut job_ids = Vec::new();
    for index in 0..4 {
        let item_id = fixture.add_item(&format!("capture {index}"));
        job_ids.push(fixture.enqueue(&item_id, OTHER_JOB_TYPE, None, 1));
    }
    let clock = fixture.clock.clone();
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, move |db, job, cancel, now| {
        clock.advance(Duration::seconds(20));
        completing(db, job, cancel, now)
    });
    let budgeted = RunnerConfig {
        time_budget: Some(Duration::seconds(30)),
        // The lease must outlive the budget so a long job is not mistaken for an abandoned one.
        lease_duration: Duration::seconds(120),
        ..config()
    };

    let report = fixture.run(
        JobCapabilities::new().with(capability),
        budgeted,
        &CancelToken::new(),
    );

    assert_eq!(report.stop, StopReason::TimeBudget);
    assert_eq!(calls.borrow().len(), 2);
    assert_eq!(fixture.job(&job_ids[2]).attempt_count, 0);
    assert_eq!(fixture.job(&job_ids[3]).attempt_count, 0);
}

// ---- Configuration and version guards ----

#[test]
fn a_job_without_a_registered_capability_waits_without_blocking_other_work() {
    let mut fixture = Fixture::new();
    let first_item = fixture.add_item(FREE_FORM);
    let second_item = fixture.add_item("pick up the parcel");
    let unhandled = fixture.enqueue(&first_item, "transcription_attachment", None, 1);
    let handled = fixture.enqueue(&second_item, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, completing);
    let waiting_since = fixture.clock.now();

    let report = fixture.drain(JobCapabilities::new().with(capability));

    assert_eq!(report.jobs.len(), 2);
    assert_eq!(
        report.jobs[0].result,
        JobResult::CapabilityUnavailable {
            retry_at: waiting_since + Duration::minutes(15)
        }
    );
    assert_eq!(calls.borrow().len(), 1);
    let row = fixture.job(&unhandled);
    assert_eq!(row.status, "queued");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some(CAPABILITY_UNAVAILABLE_REASON)
    );
    assert_eq!(row.transient_failure_count, 0);
    assert_eq!(fixture.job(&handled).status, "completed");
    assert_eq!(fixture.durable(&first_item).capture_text, FREE_FORM);
}

#[test]
fn work_that_can_never_run_is_retired_before_any_capability_sees_it() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);

    let unsupported_item = fixture.add_item("unsupported version");
    let unsupported = fixture.enqueue(&unsupported_item, OTHER_JOB_TYPE, None, 999);
    let revised_item = fixture.add_item("revised before it ran");
    let revised = fixture.enqueue(&revised_item, OTHER_JOB_TYPE, None, 1);
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET revision = revision + 1 WHERE item_id = ?",
            [&revised_item],
        )
        .unwrap();
    let deleted_item = fixture.add_item("deleted before it ran");
    let deleted = fixture.enqueue(&deleted_item, OTHER_JOB_TYPE, None, 1);
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
            [&deleted_item],
        )
        .unwrap();
    let revoked_item = fixture.add_item("profile revoked before it ran");
    let revoked = fixture.enqueue(&revoked_item, OTHER_JOB_TYPE, Some(&profile), 1);
    let now = fixture.clock.now();
    revoke_profile(&mut fixture.db, profile.profile_version(), now).unwrap();
    let live_item = fixture.add_item("still runnable");
    let live = fixture.enqueue(&live_item, OTHER_JOB_TYPE, None, 1);
    let (capability, calls) = Scripted::new(OTHER_JOB_TYPE, completing);

    let report = fixture.drain(JobCapabilities::new().with(capability));

    assert_eq!(report.stop, StopReason::Idle);
    assert_eq!(report.jobs.len(), 1);
    assert_eq!(calls.borrow().len(), 1);
    assert_eq!(calls.borrow()[0].job_id, live);
    assert_eq!(fixture.job(&live).status, "completed");
    let expectations = [
        (&unsupported, "failed", "unsupported_job_version"),
        (&revised, "cancelled", "stale_revision"),
        (&deleted, "cancelled", "item_deleted"),
        (&revoked, "cancelled", "profile_revoked"),
    ];
    for (job_id, status, reason) in expectations {
        let row = fixture.job(job_id);
        assert_eq!(row.status, status, "{reason}");
        assert_eq!(row.failure_reason.as_deref(), Some(reason));
        assert_eq!(row.attempt_count, 0, "{reason}");
    }
}

#[test]
fn an_item_revised_while_its_job_waited_is_retired_by_the_dispatcher_without_a_provider_call() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let item_id = fixture.add_item(FREE_FORM);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);
    let providers = Providers::new(vec![anthropic_reply(annotate_action(FREE_FORM, "roofer"))]);
    let before = fixture.durable(&item_id);
    // The item is revised after the job was claimed (the claim guard has already passed).
    let claiming = Before {
        inner: InterpretationCapability::new(InterpretationDispatcher::new(
            &providers.registry,
            providers.clock.as_ref(),
        )),
        hook: RefCell::new(Box::new({
            let path = fixture.path.clone();
            let clock = fixture.clock.clone();
            let item_id = item_id.clone();
            move || {
                Database::open(&path, clock.clone())
                    .unwrap()
                    .conn()
                    .execute(
                        "UPDATE items SET revision = revision + 1 WHERE item_id = ?",
                        [&item_id],
                    )
                    .unwrap();
            }
        })),
    };

    let report = fixture.drain(JobCapabilities::new().with(claiming));

    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Retired)]
    );
    assert_eq!(providers.calls(), 0);
    let row = fixture.job(&job_id);
    assert_eq!(row.status, "cancelled");
    assert_eq!(
        row.failure_reason.as_deref(),
        Some(RetireReason::SourceRevised.as_str())
    );
    let after = fixture.durable(&item_id);
    assert_eq!(after.capture_text, before.capture_text);
    assert_eq!(after.proposals, 0);
}

// ---- Reminder opportunity ----

#[test]
fn a_result_arriving_after_the_reminder_time_keeps_the_source_and_schedules_nothing() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let text = "The leak is getting worse. Remind me 2026-01-15 11:00:00 to call them";
    let item_id = fixture.add_item(text);
    let job_id = fixture.enqueue_interpretation(&item_id, &profile);
    let mut proposal = annotate_action(text, "call them");
    proposal["reminder_proposal"] = json!({
        "instant": "2026-01-15T11:00:00Z",
        "timezone_id": "UTC",
        "quality": "explicit",
        "source_span": span_of(text, "2026-01-15 11:00:00"),
    });
    let providers = Providers::new(vec![anthropic_reply(proposal)]);

    // The device was offline: the result is only applied an hour after the requested time.
    fixture.clock.set(instant("2026-01-15T12:00:00Z"));
    let report = fixture.drain(providers.capabilities());

    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Completed)]
    );
    assert_eq!(providers.calls(), 1);
    assert_eq!(fixture.job(&job_id).status, "completed");
    let durable = fixture.durable(&item_id);
    assert_eq!(durable.capture_text, text);
    assert_eq!(durable.item_type.as_deref(), Some("action"));

    let (request_state, resolved_instant) = fixture
        .reminder(&item_id)
        .expect("expired opportunity is recorded");
    assert_eq!(request_state, "unschedulable");
    assert_eq!(resolved_instant.as_deref(), Some("2026-01-15T11:00:00Z"));
    assert_eq!(fixture.count("SELECT COUNT(*) FROM reminder_operations"), 0);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM reminders WHERE schedule_generation != 0"),
        0
    );

    let mut db = fixture.reopened();
    let tx = db.immediate_transaction().unwrap();
    let status = ItemStatus::load(&tx, &item_id).unwrap().unwrap();
    tx.commit().unwrap();
    assert_eq!(
        status.unschedulable_reason,
        Some(UnschedulableReason::TimeInPast)
    );
}

#[test]
fn the_same_result_before_the_reminder_time_is_scheduled() {
    let mut fixture = Fixture::new();
    let profile = anthropic_profile();
    fixture.set_up_provider(&profile);
    let text = "The leak is getting worse. Remind me 2026-01-15 11:00:00 to call them";
    let item_id = fixture.add_item(text);
    fixture.enqueue_interpretation(&item_id, &profile);
    let mut proposal = annotate_action(text, "call them");
    proposal["reminder_proposal"] = json!({
        "instant": "2026-01-15T11:00:00Z",
        "timezone_id": "UTC",
        "quality": "explicit",
        "source_span": span_of(text, "2026-01-15 11:00:00"),
    });
    let providers = Providers::new(vec![anthropic_reply(proposal)]);

    let report = fixture.drain(providers.capabilities());

    assert_eq!(
        results(&report),
        vec![&JobResult::Settled(Settlement::Completed)]
    );
    let (request_state, resolved_instant) = fixture.reminder(&item_id).expect("reminder");
    assert_eq!(request_state, "resolved");
    assert_eq!(resolved_instant.as_deref(), Some("2026-01-15T11:00:00Z"));
}
