// Tests for job configuration pinning and profile version handling (V03)

use anyhow::Result;
use chrono::Utc;
use ohand_core::jobs::configuration::{
    get_item_jobs_for_profile, get_jobs_pinned_to_profile, get_profile_id, is_job_profile_revoked,
    is_profile_available,
};
use ohand_core::jobs::queue::enqueue_job;
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

    tx.commit()?;
    Ok((db, item_id.to_string()))
}

fn create_profile(db: &mut Database, profile_id: &str, profile_version: &str) -> Result<()> {
    let tx = db.immediate_transaction()?;
    tx.execute(
        "INSERT INTO provider_profiles (
            profile_id, profile_version, provider_type, model, timeout_seconds,
            retry_policy, authorized_destinations, capabilities, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            profile_id,
            profile_version,
            "anthropic",
            "claude-3-5-sonnet",
            30,
            r#"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#,
            r#"["https://api.anthropic.com"]"#,
            r#"{"text_interpretation":{"capability":"text_interpretation","support":"supported","evidence":"V06","input_size_limit":8000,"structured_output":"none"}}"#,
            "2026-01-01T00:00:00Z"
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Test 1: Verify that queued jobs pinned to a profile version are not affected
/// when the default provider is changed.
#[test]
fn test_default_provider_change_does_not_reroute_pinned_jobs() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let old_profile = "old-profile-uuid-0000000000001111";
    let new_profile = "new-profile-uuid-0000000000002222";

    create_profile(&mut db, "anthropic-default", old_profile)?;

    enqueue_job(
        &mut db,
        "job-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        1,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-2".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        2,
        Some(old_profile.to_string()),
        None,
        1,
        now,
    )?;

    create_profile(&mut db, "openai-default", new_profile)?;

    let old_profile_jobs = get_jobs_pinned_to_profile(db.conn(), old_profile)?;
    assert_eq!(
        old_profile_jobs.len(),
        2,
        "Both jobs should remain pinned to old profile"
    );

    let new_profile_jobs = get_jobs_pinned_to_profile(db.conn(), new_profile)?;
    assert_eq!(
        new_profile_jobs.len(),
        0,
        "No jobs should be auto-migrated to new profile"
    );

    assert!(
        is_profile_available(db.conn(), old_profile)?,
        "Old profile should still exist"
    );
    assert!(
        is_profile_available(db.conn(), new_profile)?,
        "New profile should exist"
    );

    Ok(())
}

/// Test 2: Verify that revocation stops future dispatch of queued jobs.
#[test]
fn test_revocation_stops_future_dispatch() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "revocable-profile-uuid-0000000000003333";

    create_profile(&mut db, "revocable-provider", profile)?;

    enqueue_job(
        &mut db,
        "job-for-revoked".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        1,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    assert!(
        is_profile_available(db.conn(), profile)?,
        "Profile should be available before revocation"
    );

    let tx = db.immediate_transaction()?;
    tx.execute(
        "DELETE FROM provider_profiles WHERE profile_version = ?",
        [profile],
    )?;
    tx.commit()?;

    assert!(
        !is_profile_available(db.conn(), profile)?,
        "Profile should be unavailable after revocation"
    );

    assert!(
        is_job_profile_revoked(db.conn(), "job-for-revoked")?,
        "Job's profile should be marked as revoked"
    );

    Ok(())
}

/// Test 3: Verify that credentials are not persisted in the job payload.
#[test]
fn test_credentials_not_persisted_in_job() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile = "profile-with-credential-uuid-0000004444";

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

    // Check job record: it should only contain the profile_version reference
    let job_fields: (String, Option<String>, Option<String>) = db.conn().query_row(
        "SELECT job_id, profile_version, request_version FROM jobs WHERE job_id = ?",
        ["job-needs-cred"],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let (job_id, stored_profile, stored_request) = job_fields;
    assert_eq!(job_id, "job-needs-cred");
    assert_eq!(stored_profile, Some(profile.to_string()));
    assert_eq!(stored_request, None);

    Ok(())
}

/// Test 4: Multiple profiles coexist; jobs pinned to different versions remain independent.
#[test]
fn test_multiple_profiles_independent() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile_a = "profile-a-uuid-0000000000005555";
    let profile_b = "profile-b-uuid-0000000000006666";
    let profile_c = "profile-c-uuid-0000000000007777";

    create_profile(&mut db, "provider-a", profile_a)?;
    create_profile(&mut db, "provider-b", profile_b)?;
    create_profile(&mut db, "provider-c", profile_c)?;

    enqueue_job(
        &mut db,
        "job-a-1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        1,
        Some(profile_a.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-a-2".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        2,
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
        3,
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
        4,
        Some(profile_c.to_string()),
        None,
        1,
        now,
    )?;

    let tx = db.immediate_transaction()?;
    tx.execute(
        "DELETE FROM provider_profiles WHERE profile_version = ?",
        [profile_b],
    )?;
    tx.commit()?;

    assert!(
        !is_job_profile_revoked(db.conn(), "job-a-1")?,
        "Job A1 should not be revoked"
    );
    assert!(
        !is_job_profile_revoked(db.conn(), "job-a-2")?,
        "Job A2 should not be revoked"
    );
    assert!(
        is_job_profile_revoked(db.conn(), "job-b-1")?,
        "Job B1 should be revoked (profile B deleted)"
    );
    assert!(
        !is_job_profile_revoked(db.conn(), "job-c-1")?,
        "Job C1 should not be revoked"
    );

    let jobs_a = get_jobs_pinned_to_profile(db.conn(), profile_a)?;
    let jobs_b = get_jobs_pinned_to_profile(db.conn(), profile_b)?;
    let jobs_c = get_jobs_pinned_to_profile(db.conn(), profile_c)?;

    assert_eq!(jobs_a.len(), 2);
    assert_eq!(jobs_b.len(), 1);
    assert_eq!(jobs_c.len(), 1);

    Ok(())
}

/// Test 5: Local-only (on-device) jobs are never revoked.
#[test]
fn test_local_only_jobs_not_revoked() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    enqueue_job(
        &mut db,
        "local-job-1".to_string(),
        item_id.clone(),
        "transcription_attachment".to_string(),
        1,
        None,
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "local-job-2".to_string(),
        item_id.clone(),
        "transcription_attachment".to_string(),
        2,
        None,
        None,
        1,
        now,
    )?;

    assert!(!is_job_profile_revoked(db.conn(), "local-job-1")?);
    assert!(!is_job_profile_revoked(db.conn(), "local-job-2")?);

    Ok(())
}

/// Test 6: Get profile ID to identify what was revoked.
#[test]
fn test_get_profile_id_for_identification() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let mut db = Database::open(":memory:", clock.clone() as Arc<dyn Clock>)?;

    let profile_id = "my-anthropic-profile";
    let profile_version = "profile-uuid-0000000000008888";

    assert!(get_profile_id(db.conn(), profile_version)?.is_none());

    create_profile(&mut db, profile_id, profile_version)?;

    let retrieved = get_profile_id(db.conn(), profile_version)?;
    assert_eq!(retrieved, Some(profile_id.to_string()));

    Ok(())
}

/// Test 7: Get item-specific jobs pinned to a profile.
#[test]
fn test_get_item_jobs_for_profile() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id_1) = setup_test_db(&clock)?;
    let now = clock.now();

    let item_id_2 = "item-config-test-2";
    let capture_id_2 = "capture-config-test-2";

    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO captures (
            capture_id, text, audio_reference, capture_instant, timezone_id,
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            capture_id_2,
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

    let profile = "shared-profile-uuid-0000000000009999";
    create_profile(&mut db, "shared-provider", profile)?;

    enqueue_job(
        &mut db,
        "job-item1-a".to_string(),
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
        "job-item1-b".to_string(),
        item_id_1.clone(),
        "interpret".to_string(),
        2,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    enqueue_job(
        &mut db,
        "job-item2-a".to_string(),
        item_id_2.to_string(),
        "interpret".to_string(),
        1,
        Some(profile.to_string()),
        None,
        1,
        now,
    )?;

    let item1_jobs = get_item_jobs_for_profile(db.conn(), &item_id_1, profile)?;
    assert_eq!(item1_jobs.len(), 2);
    assert!(item1_jobs.contains(&"job-item1-a".to_string()));
    assert!(item1_jobs.contains(&"job-item1-b".to_string()));

    let item2_jobs = get_item_jobs_for_profile(db.conn(), item_id_2, profile)?;
    assert_eq!(item2_jobs.len(), 1);
    assert!(item2_jobs.contains(&"job-item2-a".to_string()));

    Ok(())
}

/// Test 8: Profile change is explicit via new version (not silent replacement).
#[test]
fn test_profile_change_creates_new_version() -> Result<()> {
    let clock = Arc::new(MockClock::new(Utc::now()));
    let (mut db, item_id) = setup_test_db(&clock)?;
    let now = clock.now();

    let profile_v1 = "profile-v1-uuid-0000000000000aaa";
    let profile_v2 = "profile-v2-uuid-0000000000000bbb";

    create_profile(&mut db, "same-provider", profile_v1)?;
    create_profile(&mut db, "same-provider", profile_v2)?;

    enqueue_job(
        &mut db,
        "job-on-v1".to_string(),
        item_id.clone(),
        "interpret".to_string(),
        1,
        Some(profile_v1.to_string()),
        None,
        1,
        now,
    )?;

    assert!(is_profile_available(db.conn(), profile_v1)?);
    assert!(is_profile_available(db.conn(), profile_v2)?);

    let jobs_v1 = get_jobs_pinned_to_profile(db.conn(), profile_v1)?;
    let jobs_v2 = get_jobs_pinned_to_profile(db.conn(), profile_v2)?;

    assert_eq!(jobs_v1.len(), 1);
    assert_eq!(jobs_v2.len(), 0);

    Ok(())
}
