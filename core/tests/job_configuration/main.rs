// Tests for job configuration pinning and profile version handling (V03)

use anyhow::Result;
use chrono::Duration;
use chrono::Utc;
use ohand_core::jobs::configuration::{
    get_item_jobs_for_profile, get_jobs_pinned_to_profile, is_job_profile_revoked,
    is_profile_available, requeue_job_to_new_profile_in_tx, revoke_profile_and_retire_jobs_in_tx,
};
use ohand_core::jobs::queue::{claim_job_with_lease, complete_job_in_tx, enqueue_job, JobStatus};
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
        "openai"
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
            if provider_type == "openai" { "gpt-4" } else { "claude-3-5-sonnet" },
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

/// Behavioral Test 1: Default provider change never silently reroutes pinned jobs.
/// After switching defaults, old queued work stays pinned to its original profile,
/// never automatically migrates to the new default.
#[test]
fn test_default_provider_change_does_not_reroute_pinned_jobs() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-uuid-anthropic-v1-0000001111";
    let new_profile = "profile-uuid-openai-v1-0000002222";

    // Create old default profile and enqueue jobs pinned to it
    create_profile(&mut db, "anthropic-default", old_profile)?;

    let job1 = enqueue_job(
        &mut db,
        "job-old-default-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;

    assert_eq!(job1.status, JobStatus::Queued);
    assert_eq!(job1.profile_version, Some(old_profile.to_string()));

    // Switch default provider
    create_profile(&mut db, "openai-default", new_profile)?;

    // Verify: old job remains pinned to old profile
    let old_jobs = get_jobs_pinned_to_profile(db.conn(), old_profile)?;
    assert_eq!(old_jobs.len(), 1);
    assert!(old_jobs.contains(&"job-old-default-1".to_string()));

    // Verify: new profile has no auto-migrated jobs
    let new_jobs = get_jobs_pinned_to_profile(db.conn(), new_profile)?;
    assert_eq!(
        new_jobs.len(),
        0,
        "No jobs should auto-migrate to new default"
    );

    // Verify: both profiles are available
    assert!(is_profile_available(db.conn(), old_profile)?);
    assert!(is_profile_available(db.conn(), new_profile)?);

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
/// A job is claimed, then its profile is revoked, then a result submission fails.
#[test]
fn test_late_results_rejected_after_revocation() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-revocable-result-test-4444";
    create_profile(&mut db, "revocable-provider", profile)?;

    // Enqueue and claim a job
    enqueue_job(
        &mut db,
        "job-late-result".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    let claimed_job =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");

    assert_eq!(claimed_job.job_id, "job-late-result");
    assert_eq!(claimed_job.status, JobStatus::Running);
    let lease_attempt = claimed_job.attempt_count;

    // Profile is revoked while job is running
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile, now)?;
        tx.commit()?;
    }

    // Attempt to complete the job: should fail because profile was revoked
    // (complete_job_in_tx checks revocation status)
    let complete_result = {
        let tx = db.immediate_transaction()?;
        complete_job_in_tx(&tx, "job-late-result", lease_attempt)
            .map(|_| "success")
            .map_err(|e| e.to_string())
    };

    assert!(
        complete_result.is_err(),
        "Completing a job with revoked profile should fail"
    );
    assert!(
        complete_result.unwrap_err().contains("profile"),
        "Error should mention profile revocation"
    );

    // Advance time past lease expiry to trigger the claim loop's revocation cancellation
    let now_plus_10_min = now + Duration::minutes(10);
    let claimed_after_expiry =
        claim_job_with_lease(&mut db, Duration::minutes(5), now_plus_10_min)?;
    assert!(
        claimed_after_expiry.is_none(),
        "Claim should return none after revoked job's lease expires and is cancelled"
    );

    // Verify the job was cancelled by the claim loop
    let job_status: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-late-result"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        job_status.0, "cancelled",
        "Running job with revoked profile should be marked cancelled after lease expiry"
    );
    assert_eq!(
        job_status.1,
        Some("profile_revoked".to_string()),
        "Job should have profile_revoked failure reason"
    );

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

/// Behavioral Test 6: Credentials are never persisted in job payload.
/// Job records contain only profile_version reference; credentials are resolved at execution.
#[test]
fn test_credentials_not_persisted_in_job() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-with-credential-uuid-4444";
    create_profile(&mut db, "provider-with-creds", profile)?;

    enqueue_job(
        &mut db,
        "job-needs-cred".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        1,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    // Verify job payload contains only profile_version, no credentials
    let job_fields: (String, Option<String>, Option<String>) = db.conn().query_row(
        "SELECT job_id, profile_version, request_version FROM jobs WHERE job_id = ?",
        ["job-needs-cred"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let (_job_id, stored_profile, stored_request) = job_fields;
    assert_eq!(stored_profile, Some(profile.to_string()));
    assert_eq!(
        stored_request, None,
        "Credentials should never be in request_version"
    );

    Ok(())
}

/// Behavioral Test 7: Explicit requeue is inspectable; profile change is not silent.
/// When a profile configuration changes, the old job is cancelled with reason 'requeued'
/// and a new job is created pinned to the new profile version.
#[test]
fn test_explicit_requeue_after_profile_change() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-old-version-uuid-7777";
    let new_profile = "profile-new-version-uuid-8888";

    create_profile(&mut db, "provider-old-config", old_profile)?;

    // Enqueue job with old profile
    let enqueued = enqueue_job(
        &mut db,
        "job-on-old-profile".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;
    assert_eq!(enqueued.status, JobStatus::Queued);

    // Claim the old job to verify it's the only claimable one before requeue
    let claimed_before = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("old job should be claimable");
    assert_eq!(claimed_before.job_id, "job-on-old-profile");
    assert_eq!(
        claimed_before.profile_version,
        Some(old_profile.to_string())
    );

    // Job is now running; we can test requeue with a running job
    assert_eq!(claimed_before.status, JobStatus::Running);

    // Profile is updated (new version created)
    create_profile(&mut db, "provider-new-config", new_profile)?;

    // Explicit requeue: atomically retire old job and create new job pinned to new profile
    {
        let tx = db.immediate_transaction()?;
        requeue_job_to_new_profile_in_tx(
            &tx,
            "job-on-old-profile",
            "job-on-new-profile".to_string(),
            item_id.clone(),
            "interpret".to_string(),
            0,
            new_profile.to_string(),
            None,
            1,
            now,
        )?;
        tx.commit()?;
    }

    // Verify: old job is now cancelled with reason 'requeued'
    let old_status: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-on-old-profile"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(old_status.0, "cancelled");
    assert_eq!(old_status.1, Some("requeued".to_string()));

    // Verify: new job is pinned to new profile and is queued
    let new_job_data: (String, Option<String>) = db.conn().query_row(
        "SELECT status, profile_version FROM jobs WHERE job_id = ?",
        ["job-on-new-profile"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(new_job_data.0, "queued");
    assert_eq!(new_job_data.1, Some(new_profile.to_string()));

    // Verify: claiming after requeue returns only the new job
    let claimed_after = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("new job should be claimable");
    assert_eq!(claimed_after.job_id, "job-on-new-profile");
    assert_eq!(claimed_after.profile_version, Some(new_profile.to_string()));

    // Verify: the old job is not claimable again
    let claimed_again = claim_job_with_lease(&mut db, Duration::minutes(5), now)?;
    assert!(claimed_again.is_none(), "No more jobs should be claimable");

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

    // Revoke the profile while the job is running
    {
        let tx = db.immediate_transaction()?;
        revoke_profile_and_retire_jobs_in_tx(&tx, profile, now)?;
        tx.commit()?;
    }

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

/// Behavioral Test 12: Requeue creates inspectable record and atomicity.
/// After requeue, old job is marked with reason 'requeued', new job is created and claimable,
/// and both jobs can be linked through their records. The requeue is atomic and complete.
#[test]
fn test_requeue_creates_inspectable_record() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "profile-requeue-old-eeee";
    let new_profile = "profile-requeue-new-ffff";

    create_profile(&mut db, "provider-old", old_profile)?;
    create_profile(&mut db, "provider-new", new_profile)?;

    // Enqueue and claim an old job
    enqueue_job(
        &mut db,
        "job-to-requeue".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        0,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;

    let _claimed =
        claim_job_with_lease(&mut db, Duration::minutes(5), now)?.expect("job should be claimable");

    // Requeue to new profile
    {
        let tx = db.immediate_transaction()?;
        requeue_job_to_new_profile_in_tx(
            &tx,
            "job-to-requeue",
            "job-requeued".to_string(),
            item_id,
            "interpret".to_string(),
            0,
            new_profile.to_string(),
            None,
            1,
            now,
        )?;
        tx.commit()?;
    }

    // Verify old job is marked as requeued
    let old_job_data: (String, Option<String>) = db.conn().query_row(
        "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
        ["job-to-requeue"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(old_job_data.0, "cancelled");
    assert_eq!(old_job_data.1, Some("requeued".to_string()));

    // Verify new job exists and is queued on new profile
    let new_job_data: (String, Option<String>, i32) = db.conn().query_row(
        "SELECT status, profile_version, attempt_count FROM jobs WHERE job_id = ?",
        ["job-requeued"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(new_job_data.0, "queued");
    assert_eq!(new_job_data.1, Some(new_profile.to_string()));
    assert_eq!(
        new_job_data.2, 0,
        "Requeued job should start with fresh attempt count"
    );

    // Verify: only new job is now claimable
    let claimed_after = claim_job_with_lease(&mut db, Duration::minutes(5), now)?
        .expect("new job should be claimable");
    assert_eq!(claimed_after.job_id, "job-requeued");
    assert_eq!(claimed_after.profile_version, Some(new_profile.to_string()));

    // Verify: no more jobs claimable
    let claimed_again = claim_job_with_lease(&mut db, Duration::minutes(5), now)?;
    assert!(claimed_again.is_none(), "No more jobs should be claimable");

    Ok(())
}
