// Helper functions for deletion_intent tests.

use chrono::Utc;
use ohand_core::jobs::queue::{enqueue_job, JobStatus};
use ohand_core::store::captures::{save_capture, Capture};
use ohand_core::store::schema::{Database, SystemClock};
use std::fs;
use std::sync::Arc;

pub fn create_test_db() -> Database {
    // Create a temporary database file for testing
    let temp_dir = std::env::temp_dir();
    let test_db_path = temp_dir.join(format!("test_deletion_{}.db", uuid::Uuid::new_v4()));

    // Clean up any existing file
    let _ = fs::remove_file(&test_db_path);

    let db = Database::open(
        test_db_path.to_str().expect("Path should be valid"),
        Arc::new(SystemClock),
    )
    .expect("Database should open");

    db
}

pub fn create_test_capture(
    db: &mut Database,
    capture_id: &str,
    text: &str,
    audio_reference: Option<&str>,
) {
    let now = Utc::now();
    let capture = Capture::new(
        capture_id.to_string(),
        Some(text.to_string()),
        audio_reference.map(|s| s.to_string()),
        "2026-01-15T10:30:00+00:00".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-default".to_string(),
        false,
        now.to_rfc3339(),
        None,
    )
    .expect("Capture::new should not fail");
    save_capture(db, &capture).expect("Capture should save");
}

pub fn create_test_item(db: &mut Database, item_id: &str, capture_id: &str) {
    let now = Utc::now();
    let tx = db.transaction().expect("Transaction should succeed");

    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                            sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_configured', 'pending', 'pending', ?, ?)",
        rusqlite::params![item_id, capture_id, now.to_rfc3339(), now.to_rfc3339()],
    )
    .expect("Insert should succeed");

    tx.commit().expect("Commit should succeed");
}

pub fn create_test_job(
    db: &mut Database,
    item_id: &str,
    job_type: &str,
    status: JobStatus,
    created_at: chrono::DateTime<chrono::Utc>,
) -> String {
    let job_id = format!(
        "job-{}-{}-{}",
        item_id,
        job_type,
        created_at.timestamp_millis()
    );

    enqueue_job(
        db,
        job_id.clone(),
        item_id.to_string(),
        job_type.to_string(),
        0,
        None,
        None,
        1,
        created_at,
    )
    .expect("Job should enqueue");

    // Update status if needed
    if status != JobStatus::Queued {
        let tx = db.transaction().expect("Transaction should succeed");
        tx.execute(
            "UPDATE jobs SET status = ? WHERE job_id = ?",
            rusqlite::params![status.as_str(), &job_id],
        )
        .expect("Update should succeed");
        tx.commit().expect("Commit should succeed");
    }

    job_id
}

pub fn set_item_lifecycle_state(db: &mut Database, item_id: &str, lifecycle_state: &str) {
    let tx = db.transaction().expect("Transaction should succeed");
    tx.execute(
        "UPDATE items SET lifecycle_state = ? WHERE item_id = ?",
        rusqlite::params![lifecycle_state, item_id],
    )
    .expect("Update should succeed");
    tx.commit().expect("Commit should succeed");
}
