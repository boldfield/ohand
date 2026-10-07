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
    )?;

    assert_eq!(job.job_id, job_id);
    assert_eq!(job.item_id, item_id);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.attempt_count, 0);
    assert!(job.next_attempt_at.is_none());
    assert!(job.lease_expires_at.is_none());

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
    )?;

    let now = mock_clock.now();
    let lease_duration = Duration::seconds(30);
    let claimed = claim_job_with_lease(&mut db, lease_duration, now)?.expect("No job to claim");

    assert_eq!(claimed.job_id, "job-1");
    assert_eq!(claimed.status, JobStatus::Running);
    assert_eq!(claimed.attempt_count, 1);
    assert!(claimed.lease_expires_at.is_some());

    let lease_at = claimed.lease_expires_at.unwrap();
    assert!(lease_at > now);
    assert!(lease_at <= now + lease_duration + Duration::seconds(1)); // 1s tolerance for test timing

    // Verify it's stored in database
    let stored = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(stored.status, JobStatus::Running);
    assert_eq!(stored.attempt_count, 1);

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
fn test_complete_job() -> Result<()> {
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
    )?;

    complete_job(&mut db, "job-1")?;

    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Completed);
    assert!(job.lease_expires_at.is_none());

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
    )?;

    // Claim and fail multiple times
    claim_job_with_lease(&mut db, Duration::seconds(1), now)?;
    fail_job_with_backoff(&mut db, "job-1", "error 1".to_string(), 2, 60, now)?;

    let job1 = get_job(&db, "job-1")?.expect("Job not found");
    let next_at_1 = job1.next_attempt_at.unwrap();
    assert!(next_at_1 > now);
    let backoff_1 = (next_at_1 - now).num_seconds();
    assert_eq!(backoff_1, 4); // 2 * 2^1

    mock_clock.set_to(next_at_1);
    claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?;
    fail_job_with_backoff(
        &mut db,
        "job-1",
        "error 2".to_string(),
        2,
        60,
        mock_clock.now(),
    )?;

    let job2 = get_job(&db, "job-1")?.expect("Job not found");
    let next_at_2 = job2.next_attempt_at.unwrap();
    let backoff_2 = (next_at_2 - mock_clock.now()).num_seconds();
    assert_eq!(backoff_2, 8); // 2 * 2^2

    // Test max backoff
    for _ in 0..6 {
        mock_clock.set_to(get_job(&db, "job-1")?.unwrap().next_attempt_at.unwrap());
        claim_job_with_lease(&mut db, Duration::seconds(1), mock_clock.now())?;
        fail_job_with_backoff(
            &mut db,
            "job-1",
            "error".to_string(),
            2,
            60,
            mock_clock.now(),
        )?;
    }

    let final_job = get_job(&db, "job-1")?.expect("Job not found");
    let final_next_at = final_job.next_attempt_at.unwrap();
    let final_backoff = (final_next_at - mock_clock.now()).num_seconds();
    assert_eq!(final_backoff, 60); // Should be capped at max (60)

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
    )?;

    cancel_job(&mut db, "job-1")?;

    let job = get_job(&db, "job-1")?.expect("Job not found");
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.lease_expires_at.is_none());

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
    )?;

    // Mark item as deleted
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [&item_id],
    )?;
    tx.commit()?;

    // Attempt to claim should fail
    let now = mock_clock.now();
    let result = claim_job_with_lease(&mut db, Duration::seconds(30), now);

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("deleted or does not exist"));

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
    )?;

    // Claim and complete one
    let now = mock_clock.now();
    claim_job_with_lease(&mut db, Duration::seconds(30), now)?;
    complete_job(&mut db, "job-1")?;

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
    );

    assert!(duplicate.is_err());

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
