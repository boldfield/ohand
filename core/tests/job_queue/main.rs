// Tests for durable job queue with lease management (J01)

use anyhow::Result;
use chrono::{Duration, Utc};
use ohand_core::jobs::queue::{
    cancel_job, claim_job_with_lease, complete_job, enqueue_job, fail_job_with_backoff,
    get_expired_lease_jobs, get_job, get_jobs_by_status, get_jobs_for_item, JobStatus,
};
use ohand_core::store::schema::{Clock, Database};
use std::sync::{Arc, RwLock};

/// Mock clock for testing time-dependent behavior.
struct MockClock {
    current_time: RwLock<chrono::DateTime<Utc>>,
}

impl MockClock {
    fn new(time: chrono::DateTime<Utc>) -> Self {
        MockClock {
            current_time: RwLock::new(time),
        }
    }

    fn advance(&self, duration: Duration) {
        *self.current_time.write().unwrap() += duration;
    }

    fn set_to(&self, time: chrono::DateTime<Utc>) {
        *self.current_time.write().unwrap() = time;
    }
}

impl Clock for MockClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        *self.current_time.read().unwrap()
    }
}

fn setup_db_with_item(mock_clock: &Arc<MockClock>) -> Result<(Database, String)> {
    let db = Database::open(":memory:", mock_clock.clone() as Arc<dyn Clock>)?;

    // Create an item in the database
    let item_id = "item-test-1";
    let capture_id = "capture-test-1";

    let mut db = db;
    let tx = db.transaction()?;

    // Insert a capture
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

    // Insert an item
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

    tx.commit()?;

    Ok((db, item_id.to_string()))
}

#[test]
fn test_enqueue_and_retrieve_job() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let job_id = "job-1";
    let job = enqueue_job(
        &mut db,
        job_id.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        Some("profile-v1".to_string()),
        None,
        1,
        mock_clock.now(),
    )?;

    assert_eq!(job.job_id, job_id);
    assert_eq!(job.item_id, item_id);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.attempt_count, 0);
    assert!(job.next_attempt_at.is_none());
    assert!(job.lease_expires_at.is_none());
    assert!(job.lease_id.is_none());

    let retrieved = get_job(&db, job_id)?.expect("Job not found");
    assert_eq!(retrieved.job_id, job_id);
    assert_eq!(retrieved.status, JobStatus::Queued);

    Ok(())
}

#[test]
fn test_duplicate_job_enqueue_fails() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let job_id = "job-1";
    enqueue_job(
        &mut db,
        job_id.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Second enqueue with same job_id should fail
    let result = enqueue_job(
        &mut db,
        job_id.to_string(),
        item_id.clone(),
        "transcription".to_string(),
        1,
        None,
        None,
        1,
        mock_clock.now(),
    );

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));

    Ok(())
}

#[test]
fn test_claim_job_with_lease() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let now = mock_clock.now();
    let lease_duration = Duration::seconds(30);
    let claimed = claim_job_with_lease(&mut db, lease_duration, now)?.expect("No job to claim");

    assert_eq!(claimed.job_id, "job-1");
    assert_eq!(claimed.status, JobStatus::Running);
    assert_eq!(claimed.attempt_count, 1);
    assert!(claimed.lease_expires_at.is_some());
    assert!(claimed.lease_id.is_some());

    let lease_at = claimed.lease_expires_at.unwrap();
    assert!(lease_at > now);
    assert!(lease_at <= now + lease_duration + Duration::seconds(1));

    // Verify it's stored in database
    let stored = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(stored.status, JobStatus::Running);
    assert_eq!(stored.attempt_count, 1);
    assert!(stored.lease_id.is_some());

    Ok(())
}

#[test]
fn test_early_retry_denial() -> Result<()> {
    // AC1: backoff is bounded - claim should not return job before next_attempt_at
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // First claim and fail with backoff
    let claimed1 =
        claim_job_with_lease(&mut db, Duration::seconds(1), now)?.expect("First claim failed");
    let lease_id = claimed1.lease_id.as_ref().unwrap().clone();
    fail_job_with_backoff(
        &mut db,
        "job-1",
        "error".to_string(),
        10,
        60,
        now,
        &lease_id,
    )?;

    // Job should have next_attempt_at set to 20 seconds from now (10 * 2^1)
    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert!(job.next_attempt_at.is_some());
    let next_at = job.next_attempt_at.unwrap();
    assert_eq!((next_at - now).num_seconds(), 20);

    // Try to claim at same time - should not return job
    let claimed2 = claim_job_with_lease(&mut db, Duration::seconds(1), now)?;
    assert!(
        claimed2.is_none(),
        "Job should not be claimable before next_attempt_at"
    );

    // Advance to just before next_attempt_at (19 seconds total) - still shouldn't claim
    mock_clock.advance(Duration::seconds(19));
    let claimed3 = claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?;
    assert!(
        claimed3.is_none(),
        "Job should not be claimable before next_attempt_at"
    );

    // Advance to exactly next_attempt_at (20 seconds total) - should claim now
    mock_clock.advance(Duration::seconds(1));
    let claimed4 = claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?
        .expect("Job should be claimable at next_attempt_at");
    assert_eq!(claimed4.attempt_count, 2);

    Ok(())
}

#[test]
fn test_expired_lease_recovery() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let now = mock_clock.now();
    let lease_duration = Duration::seconds(10);

    // Claim the job
    let claimed1 = claim_job_with_lease(&mut db, lease_duration, now)?.expect("First claim failed");
    assert_eq!(claimed1.status, JobStatus::Running);

    // No job to claim yet (lease is still valid)
    mock_clock.advance(Duration::seconds(5));
    let claimed2 = claim_job_with_lease(&mut db, lease_duration, mock_clock.now())?;
    assert!(claimed2.is_none());

    // Advance past lease expiry
    mock_clock.advance(Duration::seconds(10)); // Now at 15 seconds past original now
    let claimed3 = claim_job_with_lease(&mut db, lease_duration, mock_clock.now())?
        .expect("Should reclaim expired lease");

    assert_eq!(claimed3.job_id, "job-1");
    assert_eq!(claimed3.status, JobStatus::Running);
    assert_eq!(claimed3.attempt_count, 2); // Second claim

    Ok(())
}

#[test]
fn test_stale_lease_completion_rejected() -> Result<()> {
    // AC1: duplicate delivery cannot apply twice - stale lease holder rejected
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let lease_duration = Duration::seconds(10);

    // Worker 1: claim job
    let claimed1 = claim_job_with_lease(&mut db, lease_duration, now)?.expect("First claim failed");
    let lease_id_1 = claimed1.lease_id.as_ref().unwrap().clone();

    // Advance past worker 1's lease expiry
    mock_clock.advance(Duration::seconds(15));

    // Worker 2: claim and complete the same job
    let claimed2 = claim_job_with_lease(&mut db, lease_duration, mock_clock.now())?
        .expect("Second claim failed");
    let lease_id_2 = claimed2.lease_id.as_ref().unwrap().clone();

    // Worker 2 completes with their lease
    complete_job(&mut db, "job-1", &lease_id_2)?;
    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Completed);

    // Worker 1 tries to complete with stale lease - should fail
    let result = complete_job(&mut db, "job-1", &lease_id_1);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .to_lowercase()
        .contains("already completed"));

    Ok(())
}

#[test]
fn test_complete_job() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let claimed = claim_job_with_lease(&mut db, Duration::seconds(30), now)?.expect("Claim failed");
    let lease_id = claimed.lease_id.unwrap();

    complete_job(&mut db, "job-1", &lease_id)?;

    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Completed);
    assert!(job.lease_expires_at.is_none());
    assert!(job.lease_id.is_none());

    Ok(())
}

#[test]
fn test_bounded_exponential_backoff() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Claim and fail multiple times
    let claimed1 =
        claim_job_with_lease(&mut db, Duration::seconds(1), now)?.expect("First claim failed");
    fail_job_with_backoff(
        &mut db,
        "job-1",
        "error 1".to_string(),
        2,
        60,
        now,
        &claimed1.lease_id.unwrap(),
    )?;

    let job1 = get_job(&db, "job-1")?.expect("Job not found");
    let next_at_1 = job1.next_attempt_at.unwrap();
    assert!(next_at_1 > now);
    let backoff_1 = (next_at_1 - now).num_seconds();
    assert_eq!(backoff_1, 4); // 2 * 2^1

    mock_clock.set_to(next_at_1);
    let claimed2 = claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?
        .expect("Second claim failed");
    fail_job_with_backoff(
        &mut db,
        "job-1",
        "error 2".to_string(),
        2,
        60,
        mock_clock.now(),
        &claimed2.lease_id.unwrap(),
    )?;

    let job2 = get_job(&db, "job-1")?.expect("Job not found");
    let next_at_2 = job2.next_attempt_at.unwrap();
    let backoff_2 = (next_at_2 - mock_clock.now()).num_seconds();
    assert_eq!(backoff_2, 8); // 2 * 2^2

    // Test max backoff
    for _ in 0..6 {
        let job = get_job(&db, "job-1")?.expect("Job not found");
        mock_clock.set_to(job.next_attempt_at.unwrap());
        let claimed = claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?
            .expect("Claim failed");
        fail_job_with_backoff(
            &mut db,
            "job-1",
            "error".to_string(),
            2,
            60,
            mock_clock.now(),
            &claimed.lease_id.unwrap(),
        )?;
    }

    let final_job = get_job(&db, "job-1")?.expect("Job not found");
    let final_next_at = final_job.next_attempt_at.unwrap();
    let final_backoff = (final_next_at - mock_clock.now()).num_seconds();
    assert_eq!(final_backoff, 60); // Should be capped at max (60)

    Ok(())
}

#[test]
fn test_cancellation_is_terminal() -> Result<()> {
    // AC2: cancellation is durable - cannot be overwritten by fail or complete
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Claim and cancel
    let claimed = claim_job_with_lease(&mut db, Duration::seconds(30), now)?.expect("Claim failed");
    let lease_id = claimed.lease_id.unwrap();
    cancel_job(&mut db, "job-1")?;

    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Cancelled);

    // Try to fail the cancelled job - should fail
    let result =
        fail_job_with_backoff(&mut db, "job-1", "error".to_string(), 2, 60, now, &lease_id);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .to_lowercase()
        .contains("cancelled"));

    // Try to complete the cancelled job - should fail
    let result2 = complete_job(&mut db, "job-1", &lease_id);
    assert!(result2.is_err());
    assert!(result2
        .unwrap_err()
        .to_string()
        .to_lowercase()
        .contains("cancelled"));

    // Job should still be cancelled
    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Cancelled);

    Ok(())
}

#[test]
fn test_cancel_job() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    cancel_job(&mut db, "job-1")?;

    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.lease_expires_at.is_none());

    Ok(())
}

#[test]
fn test_deleted_item_job_skipped() -> Result<()> {
    // AC2: deleted/stale targets cannot be claimed for mutation - skip them
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Create a second item and job
    let item_id_2 = "item-test-2";
    let capture_id_2 = "capture-test-2";
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO captures (
            capture_id, text, audio_reference, capture_instant, timezone_id,
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            capture_id_2,
            Some("test text 2"),
            None::<String>,
            "2026-01-01T00:00:00Z",
            "UTC",
            0,
            "en",
            "gregorian",
            "private",
            "route-2",
            0,
            "2026-01-01T00:00:00Z"
        ],
    )?;
    tx.execute(
        "INSERT INTO items (
            item_id, capture_id, revision, lifecycle_state, save_state,
            sync_state, processing_state, transcription_state, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            item_id_2,
            capture_id_2,
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
    tx.commit()?;

    let mut db = db;
    enqueue_job(
        &mut db,
        "job-2".to_string(),
        item_id_2.to_string(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Delete the first item
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [&item_id],
    )?;
    tx.commit()?;

    let now = mock_clock.now();
    // Claim should skip job-1 (deleted item) and return job-2
    let claimed =
        claim_job_with_lease(&mut db, Duration::seconds(30), now)?.expect("Should claim job-2");

    assert_eq!(claimed.job_id, "job-2");

    // job-1 should be cancelled now
    let job1 = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job1.status, JobStatus::Cancelled);

    Ok(())
}

#[test]
fn test_stale_revision_not_claimed() -> Result<()> {
    // AC2: stale targets cannot be claimed - revision mismatch
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    // Create job with item at revision 0
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Update item to revision 1
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE items SET revision = 1 WHERE item_id = ?",
        [&item_id],
    )?;
    tx.commit()?;

    let mut db = db;
    // Create another job with new revision
    enqueue_job(
        &mut db,
        "job-2".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        1,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Claim should skip job-1 (stale revision) and return job-2
    let claimed =
        claim_job_with_lease(&mut db, Duration::seconds(30), now)?.expect("Should claim job-2");

    assert_eq!(claimed.job_id, "job-2");

    // Verify job-1 was cancelled due to stale revision
    let job1 = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job1.status, JobStatus::Cancelled);

    Ok(())
}

#[test]
fn test_cannot_claim_deleted_item() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Mark item as deleted
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [&item_id],
    )?;
    tx.commit()?;

    let mut db = db;
    // Attempt to claim should skip and cancel the job
    let now = mock_clock.now();
    let result = claim_job_with_lease(&mut db, Duration::seconds(30), now)?;

    assert!(result.is_none());

    // Job should be cancelled
    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Cancelled);

    Ok(())
}

#[test]
fn test_get_jobs_by_status() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    enqueue_job(
        &mut db,
        "job-2".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        1,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    // Claim and complete one
    let now = mock_clock.now();
    let claimed = claim_job_with_lease(&mut db, Duration::seconds(30), now)?.expect("Claim failed");
    let lease_id = claimed.lease_id.unwrap();
    complete_job(&mut db, "job-1", &lease_id)?;

    let queued = get_jobs_by_status(&db, JobStatus::Queued)?;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].job_id, "job-2");

    let completed = get_jobs_by_status(&db, JobStatus::Completed)?;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].job_id, "job-1");

    Ok(())
}

#[test]
fn test_get_jobs_for_item() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    enqueue_job(
        &mut db,
        "job-2".to_string(),
        item_id.clone(),
        "transcription".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let jobs = get_jobs_for_item(&db, &item_id)?;
    assert_eq!(jobs.len(), 2);

    Ok(())
}

#[test]
fn test_offline_jobs_inspectable() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    // Create several jobs
    for i in 0..3 {
        enqueue_job(
            &mut db,
            format!("job-{}", i).to_string(),
            item_id.clone(),
            "interpretation".to_string(),
            i,
            None,
            None,
            1,
            mock_clock.now(),
        )?;
    }

    let all_queued = get_jobs_by_status(&db, JobStatus::Queued)?;
    assert_eq!(all_queued.len(), 3);

    // Verify they remain queryable even without claiming
    for i in 0..3 {
        let job = get_job(&db, &format!("job-{}", i))?;
        assert!(job.is_some());
    }

    Ok(())
}

#[test]
fn test_duplicate_delivery_prevention() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    // Job keyed by capture/capability/version should be unique
    let job_id = "job-capture-1-interp-v1";
    let first = enqueue_job(
        &mut db,
        job_id.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        Some("profile-v1".to_string()),
        None,
        1,
        mock_clock.now(),
    )?;

    assert_eq!(first.job_id, job_id);

    // Duplicate enqueue with same ID fails
    let duplicate = enqueue_job(
        &mut db,
        job_id.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        Some("profile-v1".to_string()),
        None,
        1,
        mock_clock.now(),
    );

    assert!(duplicate.is_err());

    // Same logical work with different job_id also fails (logical key enforcement)
    let different_id = "job-capture-1-interp-v1-retry";
    let result = enqueue_job(
        &mut db,
        different_id.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        Some("profile-v1".to_string()),
        None,
        1,
        mock_clock.now(),
    );
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Logical job already exists"));

    // But different version/profile can be enqueued
    let job_id_v2 = "job-capture-1-interp-v2";
    let second = enqueue_job(
        &mut db,
        job_id_v2.to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        Some("profile-v2".to_string()),
        None,
        1,
        mock_clock.now(),
    )?;

    assert_eq!(second.job_id, job_id_v2);

    Ok(())
}

#[test]
fn test_get_expired_lease_jobs() -> Result<()> {
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        1,
        mock_clock.now(),
    )?;

    let now = mock_clock.now();

    // Claim with short lease
    claim_job_with_lease(&mut db, Duration::seconds(5), now)?;

    // Initially no expired leases
    let expired = get_expired_lease_jobs(&db, now)?;
    assert_eq!(expired.len(), 0);

    // Advance past expiry
    mock_clock.advance(Duration::seconds(10));
    let expired = get_expired_lease_jobs(&db, mock_clock.now())?;
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].job_id, "job-1");

    Ok(())
}

#[test]
fn test_unsupported_job_version() -> Result<()> {
    // AC2: unsupported job schema versions are rejected
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    // Enqueue with unsupported version
    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        2, // Unsupported version
        now,
    )?;

    // Claim should skip unsupported version job and mark it as failed
    let claimed = claim_job_with_lease(&mut db, Duration::seconds(30), now)?;
    assert!(
        claimed.is_none(),
        "Unsupported version job should not be claimed"
    );

    // Verify job was marked as failed with unsupported_job_version reason
    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(
        job.failure_reason,
        Some("unsupported_job_version".to_string())
    );

    Ok(())
}

#[test]
fn test_version_zero_rejected() -> Result<()> {
    // Version 0 is not supported - must be rejected
    let mock_clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_db_with_item(&mock_clock)?;

    let now = mock_clock.now();
    enqueue_job(
        &mut db,
        "job-0".to_string(),
        item_id.clone(),
        "interpretation".to_string(),
        0,
        None,
        None,
        0, // Version 0 is unsupported
        now,
    )?;

    // Claim should skip version 0 job and mark it as failed
    let claimed = claim_job_with_lease(&mut db, Duration::seconds(30), now)?;
    assert!(claimed.is_none(), "Version 0 job should not be claimed");

    let job = get_job(&db, "job-0")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(
        job.failure_reason,
        Some("unsupported_job_version".to_string())
    );

    Ok(())
}
