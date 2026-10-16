//! Drives the item-deletion export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::jobs::queue::enqueue_job;
use crate::lifecycle::delete_intent::{
    list_pending_deletion_work, mark_deletion_work_completed, mark_deletion_work_failed,
};
use crate::privacy::routing::JOB_TYPE_INTERPRET;
use crate::retrieval::index::{search_source_direct, sync_item_in_tx};
use crate::store::captures::{save_capture, Capture};
use crate::store::events::ItemScope;
use crate::store::schema::SystemClock;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ITEM_ID: &str = "item-1";
const CAPTURE_TEXT: &str = "synthetic capture about parsnips";
const REMINDER_ID: &str = "rem-1";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_item_deletion(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn open_database(path: &str) -> Database {
    Database::open(path, Arc::new(SystemClock)).unwrap()
}

struct Store {
    directory: std::path::PathBuf,
    path: String,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Store {
    /// An active item with a capture (with retained audio when `with_audio`), indexed for
    /// search, one queued job and, when `with_reminder`, a scheduled reminder.
    fn new(name: &str, with_audio: bool, with_reminder: bool) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-item-deletion-ffi-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let mut database = open_database(&path);
        let capture = Capture::new(
            "capture-1".to_string(),
            Some(CAPTURE_TEXT.to_string()),
            with_audio.then(|| "FinalizedAudio/capture-1.m4a".to_string()),
            "2026-10-10T09:30:00+00:00".to_string(),
            "UTC".to_string(),
            0,
            "en_US".to_string(),
            "gregorian".to_string(),
            "personal".to_string(),
            "route-personal".to_string(),
            false,
            "2026-10-10T09:30:01+00:00".to_string(),
            Some("Captured topic".to_string()),
        )
        .unwrap();
        save_capture(&mut database, &capture).unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type,
                                    save_state, sync_state, processing_state,
                                    transcription_state, created_at, updated_at)
                 VALUES (?, 'capture-1', 0, 'active', 'action', 'saved', 'not_configured',
                         'pending', 'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                [ITEM_ID],
            )
            .unwrap();
        if with_reminder {
            database
                .conn()
                .execute(
                    "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state,
                                            delivery_state, acknowledgment_state,
                                            resolved_instant, timezone_id, schedule_generation,
                                            created_at, updated_at)
                     VALUES (?, ?, 'resolved', 'scheduled', 'unknown', 'not_acknowledged',
                             '2090-01-01T09:00:00Z', 'UTC', 1, '2026-10-10T09:31:00Z',
                             '2026-10-10T09:32:00Z')",
                    [REMINDER_ID, ITEM_ID],
                )
                .unwrap();
        }
        enqueue_job(
            &mut database,
            "job-1".to_string(),
            ITEM_ID.to_string(),
            JOB_TYPE_INTERPRET.to_string(),
            0,
            None,
            None,
            1,
            Utc::now(),
        )
        .unwrap();
        let transaction = database.immediate_transaction().unwrap();
        sync_item_in_tx(&transaction, ITEM_ID).unwrap();
        transaction.commit().unwrap();
        drop(database);
        Store { directory, path }
    }

    fn scalar(&self, sql: &str) -> String {
        use rusqlite::types::Value as Cell;
        match open_database(&self.path)
            .conn()
            .query_row(sql, [], |row| row.get::<_, Cell>(0))
            .unwrap()
        {
            Cell::Text(text) => text,
            Cell::Integer(number) => number.to_string(),
            Cell::Null => "NULL".to_string(),
            other => panic!("unexpected cell {other:?}"),
        }
    }

    fn count(&self, table: &str) -> i64 {
        open_database(&self.path)
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn found(&self, term: &str) -> bool {
        !search_source_direct(
            open_database(&self.path).conn(),
            term,
            &[ItemScope::Personal],
        )
        .unwrap()
        .is_empty()
    }

    /// Records every pending cleanup row except the ones `skip` names as completed.
    fn complete_cleanup(&self, skip: &[DeletionWorkType]) {
        let mut database = open_database(&self.path);
        for row in list_pending_deletion_work(&database, ITEM_ID).unwrap() {
            if !skip.contains(&row.work_type) {
                mark_deletion_work_completed(&mut database, &row.deletion_work_id, Utc::now())
                    .unwrap();
            }
        }
    }

    fn fail_cleanup(&self, failing: DeletionWorkType) {
        let mut database = open_database(&self.path);
        for row in list_pending_deletion_work(&database, ITEM_ID).unwrap() {
            if row.work_type == failing {
                mark_deletion_work_failed(&mut database, &row.deletion_work_id, Utc::now())
                    .unwrap();
            }
        }
    }
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<CoreEvent>,
    _recorder: Box<Recorder>,
    next_operation_id: u64,
}

impl Session {
    fn over(path: &str) -> Session {
        let mut handle = 0;
        let opened = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(opened.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
            next_operation_id: 1,
        }
    }

    fn call(&mut self, request: &Value) -> CoreEvent {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn succeed(&mut self, request: &Value) -> Value {
        let event = self.call(request);
        assert_eq!(
            event.status,
            OHAND_CORE_STATUS_OK,
            "{:?}",
            String::from_utf8_lossy(&event.payload)
        );
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(body["operation_id"], event.operation_id);
        body
    }

    fn fail(&mut self, request: &Value) -> String {
        let event = self.call(request);
        assert_eq!(event.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        String::from_utf8_lossy(&event.payload).into_owned()
    }

    fn refuse(&mut self, request: &Value) {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
        assert!(
            self.events
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "a refused request must not queue work"
        );
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        consume(ohand_core_close(self.handle));
    }
}

fn mark_deleted(expected_revision: i64) -> Value {
    json!({"command": "mark_deleted", "item_id": ITEM_ID, "expected_revision": expected_revision})
}

fn progress() -> Value {
    json!({"command": "progress", "item_id": ITEM_ID})
}

#[test]
fn marking_an_item_deleted_tombstones_it_and_enqueues_the_cleanup_in_one_step() {
    let _guard = serial();
    let store = Store::new("tombstone", true, true);
    let mut session = Session::over(&store.path);
    assert!(store.found("parsnips"));

    let body = session.succeed(&mark_deleted(0));
    assert_eq!(body["outcome"], "deletion_recorded");
    assert_eq!(body["item_id"], ITEM_ID);
    assert_eq!(body["deletion_revision"], 0);
    assert_eq!(body["replayed"], false);
    assert_eq!(body["progress"]["state"], "in_progress");
    assert_eq!(
        body["progress"]["pending"],
        json!(["cancel_notifications", "clear_ingress", "remove_audio"])
    );
    assert_eq!(body["progress"]["failed"], json!([]));

    assert_eq!(store.scalar("SELECT lifecycle_state FROM items"), "deleted");
    assert_eq!(store.scalar("SELECT text FROM captures"), "");
    assert!(!store.found("parsnips"), "a deleted item leaves search");
    assert_eq!(store.scalar("SELECT status FROM jobs"), "cancelled");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "cancelled"
    );
    assert_eq!(store.count("deletion_work"), 3);
    assert_eq!(
        store.scalar("SELECT audio_reference FROM captures"),
        "FinalizedAudio/capture-1.m4a",
        "the audio reference stays until the native removal is acknowledged"
    );
}

#[test]
fn an_item_without_audio_gets_no_audio_cleanup() {
    let _guard = serial();
    let store = Store::new("no-audio", false, false);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&mark_deleted(0));
    assert_eq!(
        body["progress"]["pending"],
        json!(["cancel_notifications", "clear_ingress"])
    );
    assert_eq!(store.count("deletion_work"), 2);
}

#[test]
fn repeating_the_command_reports_the_same_intent_and_writes_nothing() {
    let _guard = serial();
    let store = Store::new("replay", true, true);
    let mut session = Session::over(&store.path);
    let first = session.succeed(&mark_deleted(0));
    let events = store.count("events");
    let work = store.count("deletion_work");
    let operations = store.count("reminder_operations");

    let again = session.succeed(&mark_deleted(0));
    assert_eq!(again["outcome"], "deletion_recorded");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["deletion_revision"], first["deletion_revision"]);
    assert_eq!(again["progress"], first["progress"]);
    assert_eq!(store.count("events"), events);
    assert_eq!(store.count("deletion_work"), work);
    assert_eq!(store.count("reminder_operations"), operations);
}

#[test]
fn a_stale_revision_is_refused_and_writes_nothing() {
    let _guard = serial();
    let store = Store::new("stale", true, true);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&mark_deleted(3));
    assert_eq!(
        body,
        json!({"operation_id": 1, "outcome": "stale_revision", "current_revision": 0})
    );
    assert_eq!(store.scalar("SELECT lifecycle_state FROM items"), "active");
    assert_eq!(store.count("deletion_work"), 0);
    assert_eq!(store.scalar("SELECT status FROM jobs"), "queued");
    assert!(store.found("parsnips"));

    session.succeed(&mark_deleted(0));
    let late = session.succeed(&mark_deleted(2));
    assert_eq!(late["outcome"], "stale_revision");
    assert_eq!(
        late["current_revision"],
        store
            .scalar("SELECT revision FROM items")
            .parse::<i64>()
            .unwrap()
    );
    assert_eq!(store.count("deletion_work"), 3);
}

#[test]
fn progress_reads_the_durable_cleanup_rows_through_to_completion() {
    let _guard = serial();
    let store = Store::new("progress", true, false);
    let mut session = Session::over(&store.path);

    let before = session.succeed(&progress());
    assert_eq!(before["outcome"], "progress");
    assert_eq!(
        before["progress"],
        json!({"state": "not_requested", "pending": [], "failed": []})
    );

    session.succeed(&mark_deleted(0));
    store.complete_cleanup(&[DeletionWorkType::CancelNotifications]);
    let partway = session.succeed(&progress());
    assert_eq!(
        partway["progress"],
        json!({"state": "in_progress", "pending": ["cancel_notifications"], "failed": []})
    );

    store.complete_cleanup(&[]);
    let done = session.succeed(&progress());
    assert_eq!(
        done["progress"],
        json!({"state": "complete", "pending": [], "failed": []})
    );
}

#[test]
fn a_terminally_failed_effect_is_reported_and_never_reads_as_complete() {
    let _guard = serial();
    let store = Store::new("failed", true, false);
    let mut session = Session::over(&store.path);
    session.succeed(&mark_deleted(0));

    store.fail_cleanup(DeletionWorkType::RemoveAudio);
    let partway = session.succeed(&progress());
    assert_eq!(partway["progress"]["state"], "in_progress");
    assert_eq!(partway["progress"]["failed"], json!(["remove_audio"]));

    store.complete_cleanup(&[]);
    let finished = session.succeed(&progress());
    assert_eq!(
        finished["progress"],
        json!({"state": "failed", "pending": [], "failed": ["remove_audio"]})
    );
}

#[test]
fn unknown_items_are_not_found() {
    let _guard = serial();
    let store = Store::new("unknown", true, false);
    let mut session = Session::over(&store.path);

    let marked = session.fail(
        &json!({"command": "mark_deleted", "item_id": "secret-item", "expected_revision": 0}),
    );
    let read = session.fail(&json!({"command": "progress", "item_id": "secret-item"}));
    for payload in [marked, read] {
        assert!(payload.contains("not_found"), "{payload}");
        assert!(!payload.contains("secret-item"), "failure echoed the item");
    }
    assert_eq!(store.count("deletion_work"), 0);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = Store::new("malformed", true, false);
    let mut session = Session::over(&store.path);

    session.refuse(&json!({"command": "mark_deleted", "item_id": "", "expected_revision": 0}));
    session.refuse(&mark_deleted(-1));
    session.refuse(&mark_deleted(i64::from(i32::MAX) + 1));
    session.refuse(&json!({"command": "mark_deleted", "item_id": ITEM_ID}));
    session.refuse(&json!({"command": "progress", "item_id": ""}));
    session.refuse(&json!({"command": "progress", "item_id": ITEM_ID, "extra": 1}));
    session.refuse(&json!({"command": "purge", "item_id": ITEM_ID}));
    assert_eq!(store.scalar("SELECT lifecycle_state FROM items"), "active");

    let handle = session.handle;
    assert_eq!(
        consume(unsafe { ohand_core_start_item_deletion(handle, 99, std::ptr::null(), 0) }).code(),
        "invalid_request"
    );
    assert_eq!(
        consume(unsafe { ohand_core_start_item_deletion(handle, 99, std::ptr::null(), 4) }).code(),
        "null_argument"
    );
}
