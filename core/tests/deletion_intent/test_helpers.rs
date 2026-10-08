// Helper functions for deletion_intent tests.

use chrono::{DateTime, TimeZone, Utc};
use ohand_core::jobs::queue::enqueue_job;
use ohand_core::store::captures::{save_capture, Capture};
use ohand_core::store::schema::{Database, SystemClock};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

pub fn fixed_instant(minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 15, 10, minute, 0)
        .single()
        .expect("fixed instant is valid")
}

/// A file-backed database that can be closed and reopened to model a process restart.
pub struct TestDb {
    database: Option<Database>,
    path: PathBuf,
    _directory: TempDir,
}

impl TestDb {
    pub fn reopen(&mut self) {
        self.database = None;
        self.database = Some(open_database(&self.path));
    }
}

impl Deref for TestDb {
    type Target = Database;
    fn deref(&self) -> &Database {
        self.database.as_ref().expect("database is open")
    }
}

impl DerefMut for TestDb {
    fn deref_mut(&mut self) -> &mut Database {
        self.database.as_mut().expect("database is open")
    }
}

fn open_database(path: &std::path::Path) -> Database {
    Database::open(
        path.to_str().expect("path is valid UTF-8"),
        Arc::new(SystemClock),
    )
    .expect("Database should open")
}

pub fn create_test_db() -> TestDb {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("deletion.db");
    let database = open_database(&path);
    TestDb {
        database: Some(database),
        path,
        _directory: directory,
    }
}

pub fn build_capture(capture_id: &str, text: &str, audio_reference: Option<&str>) -> Capture {
    Capture::new(
        capture_id.to_string(),
        Some(text.to_string()),
        audio_reference.map(|reference| reference.to_string()),
        "2026-01-15T10:30:00+00:00".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-default".to_string(),
        false,
        fixed_instant(0).to_rfc3339(),
        None,
    )
    .expect("Capture::new should not fail")
}

pub fn create_test_capture(
    db: &mut Database,
    capture_id: &str,
    text: &str,
    audio_reference: Option<&str>,
) {
    save_capture(db, &build_capture(capture_id, text, audio_reference))
        .expect("Capture should save");
}

pub fn create_test_item(db: &mut Database, item_id: &str, capture_id: &str) {
    let created_at = fixed_instant(0).to_rfc3339();
    let tx = db.transaction().expect("Transaction should succeed");
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                            sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, ?, 0, 'active', 'saved', 'not_configured', 'pending', 'pending', ?, ?)",
        rusqlite::params![item_id, capture_id, created_at, created_at],
    )
    .expect("Insert should succeed");
    tx.commit().expect("Commit should succeed");
}

pub fn create_queued_job(db: &mut Database, item_id: &str, job_type: &str) -> String {
    let job_id = format!("job-{}-{}", item_id, job_type);
    enqueue_job(
        db,
        job_id.clone(),
        item_id.to_string(),
        job_type.to_string(),
        0,
        None,
        None,
        1,
        fixed_instant(1),
    )
    .expect("Job should enqueue");
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

pub fn count_rows(db: &Database, sql: &str, parameter: &str) -> i64 {
    db.conn()
        .query_row(sql, [parameter], |row| row.get(0))
        .expect("count query should succeed")
}
