//! Drives the item-correction export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::retrieval::index::{search_source_direct, sync_item_in_tx};
use crate::store::captures::{save_capture, Capture};
use crate::store::events::ItemScope;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ITEM_ID: &str = "item-1";
const CAPTURE_TEXT: &str = "synthetic capture about parsnips";
const REMINDER_ID: &str = "rem-1";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_item_correction(handle, operation_id, bytes.as_ptr(), bytes.len())
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
    /// An active item of `item_type` with a typed capture, indexed for search.
    fn new(name: &str, item_type: &str, lifecycle: &str, with_reminder: bool) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-corrections-ffi-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let mut database = open_database(&path);
        let capture = Capture::new(
            "capture-1".to_string(),
            Some(CAPTURE_TEXT.to_string()),
            None,
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
                 VALUES (?, 'capture-1', 0, ?, ?, 'saved', 'not_configured', 'pending',
                         'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                rusqlite::params![ITEM_ID, lifecycle, item_type],
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

    fn revision(&self) -> i64 {
        self.scalar("SELECT revision FROM items").parse().unwrap()
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

fn text(command_id: &str, expected_revision: i64, text: &str) -> Value {
    json!({"command": "correct_text", "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": expected_revision, "text": text})
}

fn item_type(command_id: &str, expected_revision: i64, item_type: &str) -> Value {
    json!({"command": "correct_type", "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": expected_revision, "item_type": item_type})
}

fn topic(command_id: &str, expected_revision: i64, session_topic: &str) -> Value {
    json!({"command": "correct_session_topic", "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": expected_revision, "session_topic": session_topic})
}

#[test]
fn a_text_correction_goes_through_the_event_path_and_resyncs_the_search_index() {
    let _guard = serial();
    let store = Store::new("text", "note", "active", false);
    let mut session = Session::over(&store.path);
    assert!(store.found("parsnips"));
    assert!(!store.found("rutabagas"));

    let body = session.succeed(&text("c-1", 0, "Dig up the rutabagas"));
    assert_eq!(
        body,
        json!({"operation_id": 1, "outcome": "corrected", "item_id": ITEM_ID, "revision": 1,
               "replayed": false, "reminder_cancelled": false})
    );

    assert_eq!(store.revision(), 1);
    assert_eq!(store.count("events"), 1);
    assert_eq!(
        store.scalar("SELECT event_id FROM events"),
        "user-correction:c-1"
    );
    assert_eq!(
        store.scalar("SELECT correction_old_value FROM events"),
        CAPTURE_TEXT
    );
    assert_eq!(
        store.scalar("SELECT new_value FROM corrections WHERE kind = 'text'"),
        "Dig up the rutabagas"
    );
    assert_eq!(
        store.scalar("SELECT text FROM captures"),
        CAPTURE_TEXT,
        "the captured source is never altered"
    );
    assert!(store.found("rutabagas"), "the index follows the correction");

    let second = session.succeed(&text("c-2", 1, "Dig up the turnips"));
    assert_eq!(second["revision"], 2);
    assert_eq!(
        store.scalar(
            "SELECT correction_old_value FROM events WHERE event_id = 'user-correction:c-2'"
        ),
        "Dig up the rutabagas",
        "a later correction replaces the earlier one, not the capture"
    );
}

#[test]
fn type_and_session_topic_corrections_become_the_effective_values_without_touching_the_capture() {
    let _guard = serial();
    let store = Store::new("type-topic", "note", "active", false);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&item_type("c-1", 0, "idea"));
    assert_eq!(body["outcome"], "corrected");
    assert_eq!(body["revision"], 1);
    assert_eq!(body["reminder_cancelled"], false);
    assert_eq!(store.scalar("SELECT item_type FROM items"), "idea");

    let body = session.succeed(&topic("c-2", 1, "Allotment"));
    assert_eq!(body["revision"], 2);
    assert_eq!(
        store.scalar("SELECT current_session_topic FROM items"),
        "Allotment"
    );
    assert_eq!(
        store.scalar(
            "SELECT correction_old_value FROM events WHERE event_id = 'user-correction:c-2'"
        ),
        "Captured topic",
        "the topic stated at capture is the value replaced"
    );
    assert_eq!(
        store.scalar("SELECT session_topic FROM captures"),
        "Captured topic"
    );
    assert_eq!(store.count("corrections"), 2);
}

#[test]
fn a_stale_revision_is_refused_with_the_current_revision_and_writes_nothing() {
    let _guard = serial();
    let store = Store::new("stale", "note", "active", false);
    let mut session = Session::over(&store.path);
    session.succeed(&text("c-1", 0, "first"));

    let body = session.succeed(&text("c-2", 0, "second"));
    assert_eq!(
        body,
        json!({"operation_id": 2, "outcome": "stale_revision", "current_revision": 1})
    );
    let body = session.succeed(&item_type("c-3", 5, "action"));
    assert_eq!(body["outcome"], "stale_revision");
    assert_eq!(body["current_revision"], 1);

    assert_eq!(store.revision(), 1);
    assert_eq!(store.count("events"), 1);
    assert_eq!(store.count("corrections"), 1);
    assert_eq!(store.scalar("SELECT item_type FROM items"), "note");
}

#[test]
fn a_repeated_command_id_reports_the_first_outcome_and_writes_nothing_new() {
    let _guard = serial();
    let store = Store::new("replay", "action", "active", true);
    let mut session = Session::over(&store.path);

    let first = session.succeed(&item_type("c-1", 0, "note"));
    assert_eq!(first["reminder_cancelled"], true);
    assert_eq!(first["replayed"], false);
    session.succeed(&text("c-2", 1, "moved on"));
    let events = store.count("events");
    let commands = store.count("reminder_commands");
    let state_version = store.scalar("SELECT state_version FROM reminders");

    let replay = session.succeed(&item_type("c-1", 0, "note"));
    assert_eq!(replay["outcome"], "corrected");
    assert_eq!(replay["revision"], first["revision"]);
    assert_eq!(replay["reminder_cancelled"], true);
    assert_eq!(replay["replayed"], true);

    let replay_with_a_later_revision = session.succeed(&item_type("c-1", 2, "note"));
    assert_eq!(replay_with_a_later_revision["revision"], first["revision"]);
    assert_eq!(replay_with_a_later_revision["replayed"], true);

    assert_eq!(store.count("events"), events);
    assert_eq!(store.count("reminder_commands"), commands);
    assert_eq!(store.revision(), 2);
    assert_eq!(
        store.scalar("SELECT state_version FROM reminders"),
        state_version
    );
}

#[test]
fn reusing_a_command_id_for_a_different_correction_is_a_conflict() {
    let _guard = serial();
    let store = Store::new("conflict", "note", "active", false);
    let mut session = Session::over(&store.path);
    session.succeed(&text("c-1", 0, "first"));

    for request in [
        text("c-1", 1, "different"),
        item_type("c-1", 1, "idea"),
        topic("c-1", 1, "first"),
    ] {
        let body = session.succeed(&request);
        assert_eq!(body["outcome"], "command_conflict", "{request}");
    }
    assert_eq!(store.revision(), 1);
    assert_eq!(store.count("events"), 1);
}

#[test]
fn leaving_the_action_type_cancels_the_live_reminder_through_the_reminder_state_path() {
    let _guard = serial();
    let store = Store::new("cancel", "action", "active", true);
    let mut session = Session::over(&store.path);

    let kept = session.succeed(&item_type("c-0", 0, "action"));
    assert_eq!(kept["reminder_cancelled"], false);
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "resolved",
        "staying an action keeps the reminder"
    );

    let body = session.succeed(&item_type("c-1", 1, "idea"));
    assert_eq!(body["outcome"], "corrected");
    assert_eq!(body["reminder_cancelled"], true);
    assert_eq!(body["revision"], 2);
    assert_eq!(store.scalar("SELECT item_type FROM items"), "idea");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "cancelled"
    );
    assert_eq!(
        store.scalar("SELECT command_id FROM reminder_commands WHERE item_id = 'item-1'"),
        "user-correction:c-1:cancel-reminder"
    );
    assert_eq!(
        store.revision(),
        2,
        "the cancellation adds no second revision"
    );

    let again = session.succeed(&item_type("c-2", 2, "note"));
    assert_eq!(
        again["reminder_cancelled"], false,
        "an already cancelled reminder is not cancelled twice"
    );
}

#[test]
fn a_correction_on_a_completed_item_is_accepted_and_a_deleted_item_refuses() {
    let _guard = serial();
    let completed = Store::new("completed", "action", "completed", false);
    let mut session = Session::over(&completed.path);
    let body = session.succeed(&text("c-1", 0, "still correctable"));
    assert_eq!(body["outcome"], "corrected");
    drop(session);

    let deleted = Store::new("deleted", "action", "deleted", false);
    let mut session = Session::over(&deleted.path);
    let body = session.succeed(&text("c-1", 0, "ignored"));
    assert_eq!(body["outcome"], "item_deleted");
    assert_eq!(deleted.count("events"), 0);
}

#[test]
fn an_unknown_item_fails_with_a_fixed_code_that_does_not_echo_the_request() {
    let _guard = serial();
    let store = Store::new("unknown", "note", "active", false);
    let mut session = Session::over(&store.path);
    let mut request = text("secret-command", 0, "secret text");
    request["item_id"] = json!("secret-item");
    let event = session.call(&request);
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let payload = String::from_utf8_lossy(&event.payload).into_owned();
    assert!(payload.contains("not_found"), "{payload}");
    for secret in ["secret-command", "secret text", "secret-item"] {
        assert!(!payload.contains(secret), "failure echoed {secret}");
    }
    assert_eq!(store.count("events"), 0);
}

#[test]
fn malformed_commands_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = Store::new("invalid", "note", "active", false);
    let mut session = Session::over(&store.path);

    session.refuse(
        &json!({"command": "correct_scope", "command_id": "c", "item_id": ITEM_ID,
                           "expected_revision": 0, "scope": "work"}),
    );
    session.refuse(
        &json!({"command": "correct_text", "command_id": "c", "item_id": ITEM_ID,
                           "expected_revision": 0, "text": "ok", "old_value": "forged"}),
    );
    session.refuse(&json!({"command": "correct_text", "item_id": ITEM_ID,
                           "expected_revision": 0, "text": "ok"}));
    session.refuse(&text("", 0, "ok"));
    session.refuse(&text(&"c".repeat(513), 0, "ok"));
    session.refuse(&text("c", -1, "ok"));
    session.refuse(&text("c", 4_294_967_296, "ok"));
    session.refuse(&text("c", 0, "   "));
    session.refuse(&item_type("c", 0, "scope"));
    session.refuse(&topic("c", 0, " "));
    session.refuse(&topic("c", 0, &"t".repeat(MAX_SESSION_TOPIC_BYTES + 1)));
    let mut no_item = text("c", 0, "ok");
    no_item["item_id"] = json!("");
    session.refuse(&no_item);
    assert_eq!(store.count("events"), 0);

    let empty = consume(unsafe {
        ohand_core_start_item_correction(session.handle, 99, std::ptr::null(), 0)
    });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe {
        ohand_core_start_item_correction(session.handle, 99, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let too_large = consume(unsafe {
        ohand_core_start_item_correction(
            session.handle,
            99,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");
}
