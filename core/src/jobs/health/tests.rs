//! Behavior tests for processing health (J03). Each test drives the real J01 queue and J02a
//! settlements against a fake clock and reads health from the same store.

use super::*;
use crate::jobs::configuration::revoke_profile;
use crate::jobs::queue::{claim_job_with_lease, complete_job, enqueue_job, fail_job_with_backoff};
use crate::jobs::runner::{JobCapabilities, JobRunner, RunnerConfig};
use crate::providers::contracts::CancelToken;
use crate::store::schema::Clock;
use std::sync::{Arc, Mutex};

const START: &str = "2026-01-15T10:30:00Z";
const JOB_TYPE: &str = "interpret";
const CAPTURED_TEXT: &str = "call the roofer about the leak";

fn instant(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn start() -> DateTime<Utc> {
    instant(START)
}

struct TestClock(Mutex<DateTime<Utc>>);

impl TestClock {
    fn set(&self, now: DateTime<Utc>) {
        *self.0.lock().unwrap() = now;
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

struct Fixture {
    db: Database,
    clock: Arc<TestClock>,
}

impl Fixture {
    fn new() -> Fixture {
        let clock = Arc::new(TestClock(Mutex::new(start())));
        let db = Database::open(":memory:", clock.clone() as Arc<dyn Clock>).unwrap();
        Fixture { db, clock }
    }

    fn item(&mut self, item_id: &str) {
        let tx = self.db.transaction().unwrap();
        let capture_id = format!("capture-{item_id}");
        tx.execute(
            "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id,
                utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at)
             VALUES (?, ?, NULL, ?, 'UTC', 0, 'en', 'gregorian', 'private', 'route-1', 0, ?)",
            rusqlite::params![capture_id, CAPTURED_TEXT, START, START],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                sync_state, processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed',
                     'not_applicable', ?, ?)",
            rusqlite::params![item_id, capture_id, START, START],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    fn enqueue(&mut self, job_id: &str, item_id: &str, profile: Option<&str>) {
        enqueue_job(
            &mut self.db,
            job_id.to_string(),
            item_id.to_string(),
            JOB_TYPE.to_string(),
            0,
            profile.map(str::to_string),
            None,
            1,
            self.clock.now(),
        )
        .unwrap();
    }

    fn queued_job(&mut self, job_id: &str) {
        let item_id = format!("item-{job_id}");
        self.item(&item_id);
        self.enqueue(job_id, &item_id, None);
    }

    fn profile(&mut self, profile_version: &str) {
        self.db
            .conn()
            .execute(
                "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, model,
                    timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at)
                 VALUES (?, 'profile', 'synthetic', 'model', 30, '{}', '[]', '[]', ?)",
                rusqlite::params![profile_version, START],
            )
            .unwrap();
    }

    fn advance_to(&self, offset_minutes: i64) -> DateTime<Utc> {
        let now = start() + Duration::minutes(offset_minutes);
        self.clock.set(now);
        now
    }

    fn health_at(&self, offset_minutes: i64) -> ProcessingHealth {
        let now = self.advance_to(offset_minutes);
        read_processing_health(&self.db, &HealthConfig::default(), now).unwrap()
    }

    fn force_job(&self, job_id: &str, status: &str, reason: Option<&str>) {
        self.db
            .conn()
            .execute(
                "UPDATE jobs SET status = ?, failure_reason = ?, lease_expires_at = NULL WHERE job_id = ?",
                rusqlite::params![status, reason, job_id],
            )
            .unwrap();
    }

    fn job_rows(&self) -> Vec<String> {
        let mut statement = self
            .db
            .conn()
            .prepare(
                "SELECT job_id || '|' || status || '|' || IFNULL(failure_reason, '') || '|' ||
                        attempt_count || '|' || IFNULL(next_attempt_at, '') || '|' ||
                        IFNULL(lease_expires_at, '')
                   FROM jobs ORDER BY job_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    }
}

fn error_of(health: &ProcessingHealth, kind: RecoverableErrorKind) -> &RecoverableError {
    health
        .errors
        .iter()
        .find(|error| error.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} error in {:?}", health.errors))
}

#[test]
fn empty_queue_is_idle_with_no_history() {
    let fixture = Fixture::new();
    let health = fixture.health_at(0);
    assert_eq!(health.summary, HealthSummary::Idle);
    assert!(!health.stalled);
    assert_eq!(health.pending_jobs, 0);
    assert_eq!(health.oldest_pending_age_seconds, None);
    assert_eq!(health.last_interpretation_success_at, None);
    assert!(health.errors.is_empty());
}

#[test]
fn forced_stall_is_detected_without_any_model_call() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");

    let fresh = fixture.health_at(5);
    assert_eq!(fresh.summary, HealthSummary::Working);
    assert!(!fresh.stalled);
    assert_eq!(fresh.overdue_jobs, 1);
    assert_eq!(fresh.oldest_pending_age_seconds, Some(300));

    let stalled = fixture.health_at(16);
    assert_eq!(stalled.summary, HealthSummary::Stalled);
    assert!(stalled.stalled);
    assert_eq!(stalled.stall_reasons, vec![StallReason::Overdue]);
    assert_eq!(stalled.oldest_overdue_seconds, Some(16 * 60));
    assert_eq!(
        stalled.last_interpretation_success_at, None,
        "a stall is reported although no model ever answered"
    );
}

#[test]
fn a_failing_model_is_retrying_not_stalled() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");
    let claimed = claim_job_with_lease(&mut fixture.db, Duration::minutes(5), start())
        .unwrap()
        .unwrap();
    fail_job_with_backoff(
        &mut fixture.db,
        "job-1",
        "provider_unavailable".to_string(),
        3600,
        7200,
        start(),
        claimed.attempt_count,
    )
    .unwrap();

    let waiting = fixture.health_at(40);
    assert!(!waiting.stalled, "backoff in progress is not a stall");
    assert_eq!(waiting.summary, HealthSummary::Waiting);
    let retry = error_of(&waiting, RecoverableErrorKind::RetryScheduled);
    assert_eq!(retry.reason, "provider_unavailable");
    assert_eq!(retry.recovery, RecoveryPath::Automatic);
    assert_eq!(retry.job_count, 1);
    assert!(retry.next_attempt_at.unwrap() > fixture.clock.now());

    let overdue = fixture.health_at(120 + 16);
    assert!(
        overdue.stalled,
        "once the retry is due and unclaimed for too long it is a stall"
    );
}

#[test]
fn offline_and_missing_capability_waits_are_reported_honestly() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-offline");
    fixture.queued_job("job-capability");
    for (job_id, reason) in [
        ("job-offline", OFFLINE_DEFERRED_REASON),
        ("job-capability", CAPABILITY_UNAVAILABLE_REASON),
    ] {
        fixture
            .db
            .conn()
            .execute(
                "UPDATE jobs SET failure_reason = ?, next_attempt_at = ? WHERE job_id = ?",
                rusqlite::params![
                    reason,
                    (start() + Duration::minutes(30)).to_rfc3339(),
                    job_id
                ],
            )
            .unwrap();
    }

    let health = fixture.health_at(10);
    assert_eq!(health.summary, HealthSummary::Waiting);
    assert!(!health.stalled);
    assert_eq!(health.overdue_jobs, 0);
    assert_eq!(
        error_of(&health, RecoverableErrorKind::WaitingForNetwork).recovery,
        RecoveryPath::Automatic
    );
    assert_eq!(
        error_of(&health, RecoverableErrorKind::CapabilityUnavailable).job_count,
        1
    );
}

#[test]
fn a_lapsed_lease_is_an_error_first_and_a_stall_after_the_grace() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");
    claim_job_with_lease(&mut fixture.db, Duration::minutes(5), start())
        .unwrap()
        .unwrap();

    let running = fixture.health_at(3);
    assert_eq!(running.summary, HealthSummary::Working);
    assert_eq!(running.running_jobs, 1);
    assert!(running.errors.is_empty());

    let lapsed = fixture.health_at(6);
    assert!(
        !lapsed.stalled,
        "the next drain recovers a freshly lapsed lease"
    );
    assert_eq!(
        error_of(&lapsed, RecoverableErrorKind::LeaseExpired).recovery,
        RecoveryPath::Automatic
    );

    let stuck = fixture.health_at(8);
    assert_eq!(stuck.stall_reasons, vec![StallReason::LeaseNotRecovered]);
    assert_eq!(stuck.summary, HealthSummary::Stalled);
}

#[test]
fn unavailable_destinations_need_the_user_and_are_not_retry_waits() {
    let mut fixture = Fixture::new();
    fixture.profile("profile-v1");
    fixture.item("item-revoked");
    fixture.enqueue("job-revoked", "item-revoked", Some("profile-v1"));
    revoke_profile(&mut fixture.db, "profile-v1", start()).unwrap();
    fixture.item("item-missing");
    fixture.enqueue("job-missing", "item-missing", Some("profile-gone"));

    let health = fixture.health_at(1);
    assert_eq!(health.summary, HealthSummary::NeedsAttention);
    let destination = error_of(&health, RecoverableErrorKind::DestinationUnavailable);
    assert_eq!(destination.recovery, RecoveryPath::UserAction);
    assert_eq!(
        destination.job_count, 2,
        "one retired by revocation, one queued against a missing profile"
    );
    assert!(health
        .errors
        .iter()
        .all(|error| error.kind != RecoverableErrorKind::RetryScheduled));
}

#[test]
fn a_retired_destination_job_clears_once_newer_work_completes() {
    let mut fixture = Fixture::new();
    fixture.profile("profile-v1");
    fixture.item("item-1");
    fixture.enqueue("job-old", "item-1", Some("profile-v1"));
    revoke_profile(&mut fixture.db, "profile-v1", start()).unwrap();
    assert_eq!(fixture.health_at(1).summary, HealthSummary::NeedsAttention);

    fixture.clock.set(start() + Duration::minutes(2));
    enqueue_job(
        &mut fixture.db,
        "job-new".to_string(),
        "item-1".to_string(),
        JOB_TYPE.to_string(),
        1,
        None,
        None,
        1,
        fixture.clock.now(),
    )
    .unwrap();
    fixture.force_job("job-new", "completed", None);

    let health = fixture.health_at(3);
    assert_eq!(health.summary, HealthSummary::Idle);
    assert!(health.errors.is_empty());
}

#[test]
fn terminal_failures_are_reported_until_superseded_or_deleted() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-exhausted");
    fixture.queued_job("job-permanent");
    fixture.queued_job("job-deleted");
    fixture.force_job("job-exhausted", "failed", Some(RETRIES_EXHAUSTED_REASON));
    fixture.force_job("job-permanent", "failed", Some("invalid_output"));
    fixture.force_job("job-deleted", "failed", Some("invalid_output"));
    fixture
        .db
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = 'item-job-deleted'",
            [],
        )
        .unwrap();

    let health = fixture.health_at(1);
    assert_eq!(health.summary, HealthSummary::NeedsAttention);
    assert_eq!(
        error_of(&health, RecoverableErrorKind::RetriesExhausted).recovery,
        RecoveryPath::Terminal
    );
    let permanent = error_of(&health, RecoverableErrorKind::PermanentFailure);
    assert_eq!(permanent.reason, "invalid_output");
    assert_eq!(
        permanent.job_count, 1,
        "a deleted item's failure is not reported"
    );
    assert!(!health.stalled);
}

#[test]
fn last_success_is_the_newest_recorded_interpretation_result() {
    let mut fixture = Fixture::new();
    fixture.item("item-1");
    for (proposal_id, applied_state, created_at) in [
        ("p1", "applied", "2026-01-15T10:40:00Z"),
        ("p2", "abstained", "2026-01-15T10:50:00Z"),
        ("p3", "unapplied", "2026-01-15T11:30:00Z"),
    ] {
        fixture
            .db
            .conn()
            .execute(
                "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision,
                    schema_version, text_basis_kind, applied_state, abstained, created_at)
                 VALUES (?, 'item-1', 'capture-item-1', 0, 1, 'original', ?, 0, ?)",
                rusqlite::params![proposal_id, applied_state, created_at],
            )
            .unwrap();
    }
    let health = fixture.health_at(90);
    assert_eq!(
        health.last_interpretation_success_at,
        Some(instant("2026-01-15T10:50:00Z"))
    );
}

#[test]
fn health_json_carries_no_capture_content_or_identifiers() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-secret-id");
    let claimed = claim_job_with_lease(&mut fixture.db, Duration::minutes(5), start())
        .unwrap()
        .unwrap();
    fail_job_with_backoff(
        &mut fixture.db,
        "job-secret-id",
        CAPTURED_TEXT.to_string(),
        60,
        120,
        start(),
        claimed.attempt_count,
    )
    .unwrap();

    let health = fixture.health_at(1);
    let retry = error_of(&health, RecoverableErrorKind::RetryScheduled);
    assert_eq!(retry.reason, UNRECOGNIZED_REASON);
    let json = serde_json::to_string(&health).unwrap();
    for forbidden in [
        CAPTURED_TEXT,
        "roofer",
        "job-secret-id",
        "item-job-secret-id",
        "capture-",
    ] {
        assert!(
            !json.contains(forbidden),
            "health leaked {forbidden:?}: {json}"
        );
    }
}

#[test]
fn reason_labels_are_short_lowercase_machine_labels() {
    assert_eq!(
        safe_reason_label("provider_unavailable"),
        "provider_unavailable"
    );
    assert_eq!(safe_reason_label("HTTP 503"), UNRECOGNIZED_REASON);
    assert_eq!(safe_reason_label(""), UNRECOGNIZED_REASON);
    assert_eq!(safe_reason_label(&"a".repeat(65)), UNRECOGNIZED_REASON);
    assert_eq!(safe_reason_label(&"a".repeat(64)), "a".repeat(64));
}

#[test]
fn reasons_per_kind_are_bounded() {
    let mut fixture = Fixture::new();
    for index in 0..(MAX_REASONS_PER_KIND + 8) {
        let job_id = format!("job-{index}");
        fixture.queued_job(&job_id);
        fixture.force_job(&job_id, "failed", Some(&format!("reason_{index}")));
    }
    let health = fixture.health_at(1);
    assert_eq!(health.errors.len(), MAX_REASONS_PER_KIND + 1);
    let total: u32 = health.errors.iter().map(|error| error.job_count).sum();
    assert_eq!(
        total,
        (MAX_REASONS_PER_KIND + 8) as u32,
        "overflow is folded, not dropped"
    );
    assert!(health
        .errors
        .iter()
        .any(|error| error.reason == OVERFLOW_REASON));
}

#[test]
fn reading_health_changes_nothing_and_capture_never_needs_it() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");
    fixture.queued_job("job-2");
    fixture.force_job("job-2", "failed", Some("invalid_output"));
    let before = fixture.job_rows();

    let _ = fixture.health_at(60);
    let _ = fixture.health_at(61);
    assert_eq!(fixture.job_rows(), before);

    // A new capture is saved and queued with health never having been consulted.
    fixture.queued_job("job-3");
    assert_eq!(fixture.job_rows().len(), 3);
}

#[test]
fn a_real_drain_with_no_capability_reports_waiting_then_stalls_if_nothing_drains() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");
    let runner = JobRunner::new(
        JobCapabilities::new(),
        fixture.clock.as_ref(),
        RunnerConfig::default(),
    );
    let report = runner.drain(&mut fixture.db, &CancelToken::new());
    assert_eq!(report.jobs.len(), 1);

    let waiting = fixture.health_at(1);
    assert_eq!(waiting.summary, HealthSummary::Waiting);
    assert_eq!(
        error_of(&waiting, RecoverableErrorKind::CapabilityUnavailable).reason,
        CAPABILITY_UNAVAILABLE_REASON
    );

    let stalled = fixture.health_at(15 + 16);
    assert!(stalled.stalled);
}

#[test]
fn completed_jobs_do_not_appear_as_pending_or_errors() {
    let mut fixture = Fixture::new();
    fixture.queued_job("job-1");
    let claimed = claim_job_with_lease(&mut fixture.db, Duration::minutes(5), start())
        .unwrap()
        .unwrap();
    complete_job(&mut fixture.db, "job-1", claimed.attempt_count).unwrap();
    let health = fixture.health_at(120);
    assert_eq!(health.summary, HealthSummary::Idle);
    assert_eq!(health.pending_jobs, 0);
}
