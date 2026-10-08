// Tests for job configuration pinning and profile version handling (V03)

use anyhow::Result;
use chrono::Duration;
use chrono::Utc;
use ohand_core::jobs::configuration::{
    get_item_jobs_for_profile, get_jobs_pinned_to_profile, get_requeue_record,
    is_job_profile_revoked, is_profile_available, requeue_job_to_new_profile,
    resolve_execution_target, revoke_profile, revoke_profile_and_retire_jobs_in_tx,
    ExecutionResolution,
};
use ohand_core::jobs::queue::{claim_job_with_lease, complete_job_in_tx, enqueue_job, JobStatus};
use ohand_core::privacy::routing::DenialReason;
use ohand_core::store::schema::{Clock, Database};
use std::sync::{Arc, RwLock};

struct MockClock {
    current_time: RwLock<chrono::DateTime<Utc>>,
}

impl MockClock {
    fn new(time: chrono::DateTime<Utc>) -> Self {
        MockClock {
            current_time: RwLock::new(time),
        }
    }
}

impl Clock for MockClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        *self.current_time.read().unwrap()
    }
}

fn setup_test_db(mock_clock: &Arc<MockClock>) -> Result<(Database, String)> {
    let db = Database::open(":memory:", mock_clock.clone() as Arc<dyn Clock>)?;
    let item_id = "item-config-test-1";
    let capture_id = "capture-config-test-1";

    let mut db = db;
    let tx = db.transaction()?;

    // Insert a capture with all required fields
    tx.execute(
        "INSERT INTO captures (
            capture_id, text, audio_reference, capture_instant, timezone_id,
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            capture_id,
            Some("test text"),
            None::<String>,
            "2026-01-01T00:00:00Z",
            "UTC",
            0,
            "en",
            "gregorian",
            "private",
            "route-1",
            0,
            "2026-01-01T00:00:00Z"
        ],
    )?;

    // Insert an item with all required fields
    tx.execute(
        "INSERT INTO items (
            item_id, capture_id, revision, lifecycle_state, save_state,
            sync_state, processing_state, transcription_state, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            item_id,
            capture_id,
            0,
            "active",
            "saved",
            "not_configured",
            "uninterpreted",
            "none",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z"
        ],
    )?;

    // Insert a route with all required fields
    tx.execute(
        "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) \
         VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            "route-1",
            "test-route",
            "private",
            r#"["https://api.example.com"]"#,
            "2026-01-01T00:00:00Z"
        ],
    )?;

    // Insert route authorization for text interpretation capability
    // Authorize the same destinations as the route itself (https://api.example.com)
    tx.execute(
        "INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) \
         VALUES (?, ?, ?, ?)",
        rusqlite::params![
            "route-1",
            "text_interpretation",
            r#"["https://api.example.com"]"#,
            "2026-01-01T00:00:00Z"
        ],
    )?;

    tx.commit()?;
    Ok((db, item_id.to_string()))
}

fn create_profile(db: &mut Database, profile_id: &str, profile_version: &str) -> Result<()> {
    let tx = db.immediate_transaction()?;
    // Determine provider based on profile_id, but all profiles use the test route's destination
    let provider_type = if profile_id.contains("openai") || profile_id.contains("new") {
        "open_ai"
    } else {
        "anthropic"
    };

    tx.execute(
        "INSERT INTO provider_profiles (
            profile_id, profile_version, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            profile_id,
            profile_version,
            provider_type,
            if provider_type == "open_ai" { "gpt-4" } else { "claude-3-5-sonnet" },
            30,
            r#"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#,
            r#"["https://api.example.com"]"#,
            r#"{"text_interpretation":{"capability":"text_interpretation","support":"supported","evidence":"V06","input_size_limit":8000,"structured_output":"none"}}"#,
            "2026-01-01T00:00:00Z"
        ],
    )?;
    tx.commit()?;
    Ok(())
}

fn create_profile_with_credential(
    db: &mut Database,
    profile_id: &str,
    profile_version: &str,
    credential_ref: &str,
) -> Result<()> {
    create_profile(db, profile_id, profile_version)?;
    db.conn().execute(
        "UPDATE provider_profiles SET credential_ref = ? WHERE profile_version = ?",
        [credential_ref, profile_version],
    )?;
    Ok(())
}

fn enqueue_interpret(
    db: &mut Database,
    job_id: &str,
    item_id: &str,
    profile_version: &str,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    enqueue_job(
        db,
        job_id.to_string(),
        item_id.to_string(),
        "interpret".to_string(),
        0,
        Some(profile_version.to_string()),
        None,
        1,
        now,
    )?;
    Ok(())
}

fn job_state(db: &Database, job_id: &str) -> Result<(String, Option<String>)> {
    Ok(db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        [job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

fn expect_remote(
    db: &Database,
    job_id: &str,
) -> Result<ohand_core::jobs::configuration::ExecutionTarget> {
    match resolve_execution_target(db.conn(), job_id)? {
        ExecutionResolution::Remote(target) => Ok(target),
        other => panic!("expected remote resolution for {job_id}, got {other:?}"),
    }
}

/// Behavioral Test 1: Default provider change never silently reroutes pinned jobs.
/// After the "default" moves to a new profile, queued work claimed later still resolves to its
/// original pinned destination, and once that profile is revoked it is held back, never sent to
/// the new profile.
#[test]
fn test_default_provider_change_does_not_reroute_pinned_jobs() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-uuid-anthropic-v1-0000001111";
    let new_profile = "profile-uuid-openai-v1-0000002222";

    create_profile(&mut db, "anthropic-default", old_profile)?;
    enqueue_interpret(&mut db, "job-old-default-1", &item_id, old_profile, now)?;

    // Switch the default: new work would use new_profile, existing work is untouched.
    create_profile(&mut db, "openai-default", new_profile)?;

    assert_eq!(
        get_jobs_pinned_to_profile(db.conn(), new_profile)?,
        Vec::<String>::new(),
        "No jobs should auto-migrate to the new default"
    );

    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("old job is still claimable");
    assert_eq!(claimed.profile_version, Some(old_profile.to_string()));
    let target = expect_remote(&db, "job-old-default-1")?;
    assert_eq!(target.profile_version, old_profile);
    assert_eq!(target.provider_type, "anthropic");
    assert_eq!(target.model, "claude-3-5-sonnet");

    // Revoking the old profile holds the work: it is retired, not redirected to the new profile.
    revoke_profile(&mut db, old_profile, now)?;
    assert_eq!(
        resolve_execution_target(db.conn(), "job-old-default-1")?,
        ExecutionResolution::Denied(DenialReason::ProfileRevoked)
    );
    assert!(claim_job_with_lease(&mut db, Duration::minutes(5), now)?.is_none());
    assert_eq!(
        job_state(&db, "job-old-default-1")?,
        ("cancelled".to_string(), Some("profile_revoked".to_string()))
    );
    assert!(get_jobs_pinned_to_profile(db.conn(), new_profile)?.is_empty());

    Ok(())
}

/// Behavioral Test 2: Revocation stops future dispatch and retires queued/running jobs.
/// After a profile is revoked, jobs pinned to it cannot be claimed and are marked cancelled.
#[test]
fn test_revocation_stops_future_dispatch() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "revocable-profile-uuid-0000003333";

    create_profile(&mut db, "revocable-provider", profile)?;

    // Enqueue jobs pinned to this profile
    enqueue_job(
        &mut db,
        "job-for-revoked-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-for-revoked-2".to_string(),
        item_id.clone(),
        "transcription".to_string(),
        0,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    assert!(is_profile_available(db.conn(), profile)?);

    // Revoke the profile (retires queued/running jobs atomically)
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile, now)?;
        tx.commit()?;
    }

    // Verify: profile is now revoked
    assert!(!is_profile_available(db.conn(), profile)?);

    // Verify: jobs are marked as cancelled with reason 'profile_revoked'
    let job1: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-for-revoked-1"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(job1.0, "cancelled");
    assert_eq!(job1.1, Some("profile_revoked".to_string()));

    let job2: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-for-revoked-2"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(job2.0, "cancelled");
    assert_eq!(job2.1, Some("profile_revoked".to_string()));

    // Verify: claim_job_with_lease returns nothing (no claimable jobs)
    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), now)?;
    assert!(
        claimed.is_none(),
        "No claimable jobs should remain after revocation"
    );

    Ok(())
}

/// Behavioral Test 3: Late results are rejected after profile revocation.
/// A job is leased, its profile is revoked (retiring the running job), and the late result is
/// rejected while the job stays cancelled.
#[test]
fn test_late_results_rejected_after_revocation() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-revocable-result-test-4444";
    create_profile(&mut db, "revocable-provider", profile)?;
    enqueue_interpret(&mut db, "job-late-result", &item_id, profile, now)?;

    let claimed_job =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");
    assert_eq!(claimed_job.status, JobStatus::Running);
    let lease_attempt = claimed_job.attempt_count;

    revoke_profile(&mut db, profile, now)?;

    // The running job is retired immediately, with no need to wait for lease expiry.
    assert_eq!(
        job_state(&db, "job-late-result")?,
        ("cancelled".to_string(), Some("profile_revoked".to_string()))
    );

    let late_result = {
        let tx = db.immediate_transaction()?;
        complete_job_in_tx(&tx, "job-late-result", lease_attempt)
    };
    let error = late_result
        .expect_err("late result must be rejected")
        .to_string();
    assert!(error.contains("revoked"), "unexpected error: {error}");

    assert_eq!(
        job_state(&db, "job-late-result")?,
        ("cancelled".to_string(), Some("profile_revoked".to_string())),
        "rejected result must not change the retired job"
    );
    let later = now + Duration::minutes(10);
    assert!(claim_job_with_lease(&mut db, Duration::minutes(5), later)?.is_none());

    Ok(())
}

/// Behavioral Test 4: Revocation affects only jobs pinned to that profile.
/// Multiple profiles coexist; revoking one doesn't affect jobs pinned to others.
#[test]
fn test_revocation_affects_only_target_profile() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile_a = "profile-a-uuid-0000005555";
    let profile_b = "profile-b-uuid-0000006666";
    let profile_c = "profile-c-uuid-0000007777";

    create_profile(&mut db, "provider-a", profile_a)?;
    create_profile(&mut db, "provider-b", profile_b)?;
    create_profile(&mut db, "provider-c", profile_c)?;

    enqueue_job(
        &mut db,
        "job-a-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile_a.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-b-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile_b.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-c-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile_c.to_string()),
        None,
        1,
        now,
    )?;

    // Revoke only profile B
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile_b, now)?;
        tx.commit()?;
    }

    // Jobs in profiles A and C are still claimable; B's are not
    assert!(!is_job_profile_revoked(db.conn(), "job-a-1")?);
    assert!(is_job_profile_revoked(db.conn(), "job-b-1")?);
    assert!(!is_job_profile_revoked(db.conn(), "job-c-1")?);

    // Claim jobs: A and C are claimable, B is not
    let mut claimed_jobs = vec![];
    for _ in 0..2 {
        if let Some(job) = claim_job_with_lease(&mut db, Duration::minutes(5), now)? {
            claimed_jobs.push(job.job_id);
        }
    }

    assert!(claimed_jobs.contains(&"job-a-1".to_string()));
    assert!(claimed_jobs.contains(&"job-c-1".to_string()));
    assert!(!claimed_jobs.contains(&"job-b-1".to_string()));

    Ok(())
}

/// Behavioral Test 5: Local-only jobs are never affected by profile revocation.
/// Jobs with no profile_version (local-only work) remain claimable after any revocation.
#[test]
fn test_local_only_jobs_never_revoked() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-for-remote-uuid-5555";
    create_profile(&mut db, "remote-provider", profile)?;

    // Enqueue local-only jobs (no profile_version)
    enqueue_job(
        &mut db,
        "local-job-1".to_string(),
        item_id.clone(),
        "transcription_attachment".to_string(),
        0,
        None,
        None,
        1,
        now,
    )?;

    // Revoke all profiles (or at least one)
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile, now)?;
        tx.commit()?;
    }

    // Local job is still not revoked
    assert!(!is_job_profile_revoked(db.conn(), "local-job-1")?);

    // Local job is still claimable
    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("local job should be claimable");
    assert_eq!(claimed.job_id, "local-job-1");
    assert_eq!(claimed.profile_version, None);

    Ok(())
}

/// Behavioral Test 6: Credentials are resolved at execution time from the pinned profile and
/// never persisted into the job row.
#[test]
fn test_credentials_resolved_at_execution_time() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-with-real-credential-5555";
    create_profile_with_credential(&mut db, "provider-with-creds", profile, "keychain:ref-one")?;
    enqueue_interpret(&mut db, "job-with-cred-ref", &item_id, profile, now)?;
    claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("claimable");

    let target = expect_remote(&db, "job-with-cred-ref")?;
    assert_eq!(target.credential_ref.as_deref(), Some("keychain:ref-one"));
    assert_eq!(target.profile_version, profile);
    assert_eq!(
        target.destinations,
        vec!["https://api.example.com".to_string()]
    );

    // Updating the stored reference is observed at the next execution: nothing was cached.
    db.conn().execute(
        "UPDATE provider_profiles SET credential_ref = ? WHERE profile_version = ?",
        ["keychain:ref-two", profile],
    )?;
    let target = expect_remote(&db, "job-with-cred-ref")?;
    assert_eq!(target.credential_ref.as_deref(), Some("keychain:ref-two"));

    // Removing it is observed too.
    db.conn().execute(
        "UPDATE provider_profiles SET credential_ref = NULL WHERE profile_version = ?",
        [profile],
    )?;
    assert_eq!(
        expect_remote(&db, "job-with-cred-ref")?.credential_ref,
        None
    );

    // No column of the job row ever held a credential reference.
    let job_row_text: String = db.conn().query_row(
        "SELECT job_id || '|' || job_schema_version || '|' || item_id || '|' || job_type || '|' || \
         source_revision || '|' || IFNULL(profile_version, '') || '|' || IFNULL(request_version, '') \
         || '|' || status || '|' || IFNULL(failure_reason, '') || '|' || created_at \
         FROM jobs WHERE job_id = ?",
        ["job-with-cred-ref"],
        |row| row.get(0),
    )?;
    assert!(
        !job_row_text.contains("keychain:"),
        "job row: {job_row_text}"
    );

    // The resolved target does not leak the reference through Debug output.
    let rendered = format!("{:?}", expect_remote(&db, "job-with-cred-ref")?);
    assert!(!rendered.contains("keychain:"));

    Ok(())
}

/// Behavioral Test 6b: A denied job exposes no credential reference, and local work has none.
#[test]
fn test_denied_and_local_jobs_resolve_without_credentials() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-denied-credential-6666";
    create_profile_with_credential(&mut db, "provider-denied", profile, "keychain:secret-ref")?;
    enqueue_interpret(&mut db, "job-denied", &item_id, profile, now)?;
    revoke_profile(&mut db, profile, now)?;
    assert_eq!(
        resolve_execution_target(db.conn(), "job-denied")?,
        ExecutionResolution::Denied(DenialReason::ProfileRevoked)
    );

    enqueue_job(
        &mut db,
        "job-local".to_string(),
        item_id,
        "transcription_attachment".to_string(),
        0,
        None,
        None,
        1,
        now,
    )?;
    assert_eq!(
        resolve_execution_target(db.conn(), "job-local")?,
        ExecutionResolution::Local
    );
    assert_eq!(
        resolve_execution_target(db.conn(), "no-such-job")?,
        ExecutionResolution::Denied(DenialReason::JobNotFound)
    );
    Ok(())
}

/// Behavioral Test 7: Explicit requeue retires the old job, creates a pinned replacement and
/// records the link; before the requeue nothing is sent to the new profile, after it only the
/// replacement is claimable.
#[test]
fn test_explicit_requeue_after_profile_change() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-old-version-uuid-7777";
    let new_profile = "profile-new-version-uuid-8888";

    create_profile(&mut db, "provider-old-config", old_profile)?;
    enqueue_interpret(&mut db, "job-on-old-profile", &item_id, old_profile, now)?;

    // The configuration changes; the queued job still targets the old pin.
    create_profile(&mut db, "provider-new-config", new_profile)?;
    assert_eq!(
        expect_remote(&db, "job-on-old-profile")?.profile_version,
        old_profile
    );

    let replacement = requeue_job_to_new_profile(
        &mut db,
        "job-on-old-profile",
        "job-on-new-profile".to_string(),
        new_profile.to_string(),
        now,
    )?;
    assert_eq!(replacement.job_id, "job-on-new-profile");
    assert_eq!(replacement.profile_version, Some(new_profile.to_string()));
    assert_eq!(replacement.status, JobStatus::Queued);
    assert_eq!(replacement.attempt_count, 0);
    assert_eq!(replacement.item_id, item_id);
    assert_eq!(replacement.job_type, "interpret");

    assert_eq!(
        job_state(&db, "job-on-old-profile")?,
        ("cancelled".to_string(), Some("requeued".to_string()))
    );

    let claimed = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("replacement should be claimable");
    assert_eq!(claimed.job_id, "job-on-new-profile");
    assert_eq!(
        expect_remote(&db, "job-on-new-profile")?.profile_version,
        new_profile
    );
    assert!(claim_job_with_lease(&mut db, Duration::minutes(5), now)?.is_none());

    Ok(())
}

/// Requeueing a running job fences its lease: the in-flight result is rejected and only the
/// replacement can complete.
#[test]
fn test_requeue_of_running_job_rejects_old_lease_result() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-running-requeue-old-1111";
    let new_profile = "profile-running-requeue-new-2222";
    create_profile(&mut db, "provider-old", old_profile)?;
    create_profile(&mut db, "provider-new", new_profile)?;
    enqueue_interpret(&mut db, "job-running-old", &item_id, old_profile, now)?;
    let leased = claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("claimable");

    requeue_job_to_new_profile(
        &mut db,
        "job-running-old",
        "job-running-new".to_string(),
        new_profile.to_string(),
        now,
    )?;

    let tx = db.immediate_transaction()?;
    assert!(complete_job_in_tx(&tx, "job-running-old", leased.attempt_count).is_err());
    drop(tx);
    assert_eq!(
        job_state(&db, "job-running-old")?,
        ("cancelled".to_string(), Some("requeued".to_string()))
    );
    Ok(())
}

/// Behavioral Test 8: get_item_jobs_for_profile supports selective requeue.
/// When deciding which jobs to requeue after profile change, we can query per-item.
#[test]
fn test_get_item_jobs_for_profile_supports_selective_requeue() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id_1) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-for-requeue-9999";
    create_profile(&mut db, "provider-requeue", profile)?;

    enqueue_job(
        &mut db,
        "job-item1-interpret".to_string(),
        item_id_1.clone(),
        "interpret".to_string(),
        1,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-item1-transcribe".to_string(),
        item_id_1.clone(),
        "transcription".to_string(),
        1,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    // Query item's jobs for a specific profile version
    let jobs = get_item_jobs_for_profile(db.conn(), &item_id_1, profile)?;
    assert_eq!(jobs.len(), 2);
    assert!(jobs.contains(&"job-item1-interpret".to_string()));
    assert!(jobs.contains(&"job-item1-transcribe".to_string()));

    Ok(())
}

/// Behavioral Test 9: Revocation after lease expiry does not create infinite loop.
/// When a running job's lease expires and the profile is revoked before the next claim,
/// the claim loop must retire the job and not spin forever.
#[test]
fn test_revocation_after_lease_expiry_does_not_hang() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-lease-expiry-test-aaaa";
    create_profile(&mut db, "provider-lease-expiry", profile)?;

    // Enqueue a job
    enqueue_job(
        &mut db,
        "job-running-revoked".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    // Claim the job with a 5-minute lease
    let claimed =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");
    assert_eq!(claimed.status, JobStatus::Running);

    // Mark the profile revoked without going through revoke_profile (for example a direct
    // store edit), leaving the running job untouched so the claim loop must retire it.
    db.conn().execute(
        "UPDATE provider_profiles SET revoked_at = ? WHERE profile_version = ?",
        [now.to_rfc3339().as_str(), profile],
    )?;

    // Advance time past the lease expiry
    let now_plus_10_min = now + Duration::minutes(10);
    *clock.current_time.write().unwrap() = now_plus_10_min;

    // Attempt to claim the next job - this should NOT hang
    // The expired running job should be selected by the query, found to have a revoked profile,
    // cancelled immediately, and skipped. Then claim should return None.
    let next_claimed = claim_job_with_lease(&mut db, Duration::minutes(5), now_plus_10_min)?;
    assert!(
        next_claimed.is_none(),
        "No jobs should be claimable after revocation"
    );

    // Verify the running job is now cancelled
    let job_status: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-running-revoked"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(job_status.0, "cancelled");
    assert_eq!(job_status.1, Some("profile_revoked".to_string()));

    Ok(())
}

/// Behavioral Test 10: authorize_job rejects jobs with revoked profiles.
/// This end-to-end test verifies that jobs with revoked profiles cannot be authorized for dispatch.
/// It proves the revocation gate works in the authorization path with no provider effect.
#[test]
fn test_authorize_job_rejects_revoked_profile() -> Result<()> {
    use ohand_core::privacy::routing::{authorize_job, AuthorizationDecision, DenialReason};

    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-for-auth-test-bbbb";
    create_profile(&mut db, "provider-auth-test", profile)?;

    // Enqueue a job pinned to this profile
    enqueue_job(
        &mut db,
        "job-auth-revoke-test".to_string(),
        item_id,
        "interpret".to_string(),
        0,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    // Claim the job to move it to running state
    let claimed =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");
    assert_eq!(claimed.status, JobStatus::Running);

    // Before revocation, authorization should succeed
    let auth_before = authorize_job(db.conn(), "job-auth-revoke-test")?;
    match &auth_before {
        AuthorizationDecision::Authorized(_) => {}
        AuthorizationDecision::Denied(reason) => {
            panic!(
                "Expected authorization to succeed, got denial: {:?}",
                reason
            );
        }
    }

    // Revoke the profile
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile, now)?;
        tx.commit()?;
    }

    // After revocation, authorization should fail with ProfileRevoked
    let auth_after = authorize_job(db.conn(), "job-auth-revoke-test")?;
    assert!(
        matches!(
            auth_after,
            AuthorizationDecision::Denied(DenialReason::ProfileRevoked)
        ),
        "After revocation, job should be denied with ProfileRevoked reason"
    );

    Ok(())
}

/// Behavioral Test 11: Default provider switch shows pinned destination in authorization.
/// After changing the default provider, claiming and authorizing an old job should
/// still resolve to its original pinned profile, not the new default.
#[test]
fn test_pinned_destination_honored_in_authorization() -> Result<()> {
    use ohand_core::privacy::routing::authorize_job;

    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-old-auth-cccc";
    let new_profile = "profile-new-auth-dddd";

    create_profile(&mut db, "provider-old-config", old_profile)?;

    // Enqueue job pinned to old profile
    enqueue_job(
        &mut db,
        "job-pinned-old".to_string(),
        item_id,
        "interpret".to_string(),
        0,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;

    // Claim and authorize before default switch
    let _claimed_before =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");
    assert_eq!(
        _claimed_before.profile_version,
        Some(old_profile.to_string())
    );

    let auth_before = authorize_job(db.conn(), "job-pinned-old")?;
    let profile_before = match auth_before {
        ohand_core::privacy::routing::AuthorizationDecision::Authorized(auth) => {
            auth.profile_version().map(|s| s.to_string())
        }
        ohand_core::privacy::routing::AuthorizationDecision::Denied(reason) => {
            panic!(
                "Expected authorization to succeed before switch, got denial: {:?}",
                reason
            );
        }
    };
    assert_eq!(
        profile_before,
        Some(old_profile.to_string()),
        "Before default switch, pinned profile should be old one"
    );

    // Switch default provider
    create_profile(&mut db, "provider-new-config", new_profile)?;

    // Authorization should still use the pinned old profile, not the new default
    let auth_after = authorize_job(db.conn(), "job-pinned-old")?;
    let profile_after = match auth_after {
        ohand_core::privacy::routing::AuthorizationDecision::Authorized(auth) => {
            auth.profile_version().map(|s| s.to_string())
        }
        _ => panic!("Authorization should still succeed after switch"),
    };
    assert_eq!(
        profile_after, profile_before,
        "After default switch, pinned profile should still be old one, not new default"
    );

    Ok(())
}

/// Behavioral Test 12: Requeue leaves a durable, queryable link in both directions.
#[test]
fn test_requeue_creates_inspectable_record() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-requeue-old-eeee";
    let new_profile = "profile-requeue-new-ffff";
    create_profile(&mut db, "provider-old", old_profile)?;
    create_profile(&mut db, "provider-new", new_profile)?;
    enqueue_interpret(&mut db, "job-to-requeue", &item_id, old_profile, now)?;

    assert_eq!(get_requeue_record(db.conn(), "job-to-requeue")?, None);

    requeue_job_to_new_profile(
        &mut db,
        "job-to-requeue",
        "job-requeued".to_string(),
        new_profile.to_string(),
        now,
    )?;

    let from_old = get_requeue_record(db.conn(), "job-to-requeue")?.expect("record by old id");
    let from_new = get_requeue_record(db.conn(), "job-requeued")?.expect("record by new id");
    assert_eq!(from_old, from_new);
    assert_eq!(from_old.old_job_id, "job-to-requeue");
    assert_eq!(from_old.new_job_id, "job-requeued");
    assert_eq!(from_old.from_profile_version, Some(old_profile.to_string()));
    assert_eq!(from_old.to_profile_version, new_profile);
    assert_eq!(from_old.requeued_at.timestamp(), now.timestamp());
    Ok(())
}

/// Requeue is idempotent for the same target and refuses a second, different target.
#[test]
fn test_requeue_retry_is_idempotent() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-idem-old-aaaa";
    let new_profile = "profile-idem-new-bbbb";
    let other_profile = "profile-idem-other-cccc";
    create_profile(&mut db, "p-old", old_profile)?;
    create_profile(&mut db, "p-new", new_profile)?;
    create_profile(&mut db, "p-other", other_profile)?;
    enqueue_interpret(&mut db, "job-idem-old", &item_id, old_profile, now)?;

    let first = requeue_job_to_new_profile(
        &mut db,
        "job-idem-old",
        "job-idem-new".to_string(),
        new_profile.to_string(),
        now,
    )?;
    let retry = requeue_job_to_new_profile(
        &mut db,
        "job-idem-old",
        "job-idem-retry-id".to_string(),
        new_profile.to_string(),
        now,
    )?;
    assert_eq!(
        retry.job_id, first.job_id,
        "retry returns the existing replacement"
    );

    let job_count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))?;
    assert_eq!(job_count, 2);

    let other_target = requeue_job_to_new_profile(
        &mut db,
        "job-idem-old",
        "job-idem-other".to_string(),
        other_profile.to_string(),
        now,
    );
    assert!(other_target.is_err(), "an already requeued job cannot fork");
    // The replacement itself can still be requeued onward.
    requeue_job_to_new_profile(
        &mut db,
        "job-idem-new",
        "job-idem-onward".to_string(),
        other_profile.to_string(),
        now,
    )?;
    Ok(())
}

/// Requeue goes through the shared duplicate guard: equivalent work already on the target
/// profile fails the requeue and leaves the original job untouched.
#[test]
fn test_requeue_rejects_duplicate_on_target_profile() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-dup-old-aaaa";
    let new_profile = "profile-dup-new-bbbb";
    create_profile(&mut db, "p-old", old_profile)?;
    create_profile(&mut db, "p-new", new_profile)?;
    enqueue_interpret(&mut db, "job-dup-a", &item_id, old_profile, now)?;
    enqueue_interpret(&mut db, "job-dup-b", &item_id, new_profile, now)?;

    let result = requeue_job_to_new_profile(
        &mut db,
        "job-dup-a",
        "job-dup-c".to_string(),
        new_profile.to_string(),
        now,
    );
    assert!(result.is_err(), "duplicate delivery must be refused");

    assert_eq!(job_state(&db, "job-dup-a")?, ("queued".to_string(), None));
    assert_eq!(get_requeue_record(db.conn(), "job-dup-a")?, None);
    let job_count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))?;
    assert_eq!(job_count, 2);
    Ok(())
}

/// Requeue refuses unsafe moves: unknown/revoked/same target, local-only and finished jobs.
#[test]
fn test_requeue_rejects_invalid_requests() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-invalid-old-aaaa";
    let revoked_profile = "profile-invalid-revoked-bbbb";
    create_profile(&mut db, "p-old", old_profile)?;
    create_profile(&mut db, "p-revoked", revoked_profile)?;
    revoke_profile(&mut db, revoked_profile, now)?;
    enqueue_interpret(&mut db, "job-invalid", &item_id, old_profile, now)?;
    enqueue_job(
        &mut db,
        "job-invalid-local".to_string(),
        item_id.clone(),
        "transcription_attachment".to_string(),
        0,
        None,
        None,
        1,
        now,
    )?;

    for (job, target) in [
        ("job-invalid", "profile-does-not-exist"),
        ("job-invalid", revoked_profile),
        ("job-invalid", old_profile),
        ("job-invalid-local", old_profile),
        ("job-missing", old_profile),
    ] {
        let result = requeue_job_to_new_profile(
            &mut db,
            job,
            "job-new".to_string(),
            target.to_string(),
            now,
        );
        assert!(result.is_err(), "requeue of {job} to {target} must fail");
    }
    assert_eq!(job_state(&db, "job-invalid")?, ("queued".to_string(), None));

    // A job that already finished cannot be requeued.
    let other_profile = "profile-invalid-other-cccc";
    create_profile(&mut db, "p-other", other_profile)?;
    let leased = claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("claimable");
    {
        let tx = db.immediate_transaction()?;
        complete_job_in_tx(&tx, &leased.job_id, leased.attempt_count)?;
        tx.commit()?;
    }
    assert!(requeue_job_to_new_profile(
        &mut db,
        &leased.job_id,
        "job-new".to_string(),
        other_profile.to_string(),
        now
    )
    .is_err());
    Ok(())
}

/// Revoking an unknown profile is an error, and a missing pin target reads as revoked.
#[test]
fn test_revoke_unknown_profile_fails_and_missing_pin_is_unavailable() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    assert!(revoke_profile(&mut db, "profile-never-existed", now).is_err());

    let profile = "profile-vanishing-dddd";
    create_profile(&mut db, "p-vanish", profile)?;
    enqueue_interpret(&mut db, "job-vanish", &item_id, profile, now)?;
    assert!(!is_job_profile_revoked(db.conn(), "job-vanish")?);
    db.conn().execute(
        "DELETE FROM provider_profiles WHERE profile_version = ?",
        [profile],
    )?;
    assert!(!is_profile_available(db.conn(), profile)?);
    assert!(is_job_profile_revoked(db.conn(), "job-vanish")?);
    Ok(())
}
