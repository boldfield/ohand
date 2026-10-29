//! Drives the item lifecycle control export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::store::captures::{save_capture, Capture};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ITEM_ID: &str = "item-1";
const REMINDER_ID: &str = "rem-1";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_item_lifecycle_control(handle, operation_id, bytes.as_ptr(), bytes.len())
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
    /// An item in `lifecycle`, with a live reminder when `with_reminder`.
    fn new(name: &str, lifecycle: &str, with_reminder: bool) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-lifecycle-controls-ffi-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let mut database = open_database(&path);
        let capture = Capture::new(
            "capture-1".to_string(),
            Some("synthetic capture about parsnips".to_string()),
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
            None,
        )
        .unwrap();
        save_capture(&mut database, &capture).unwrap();
        database
            .conn()
            .execute(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type,
                                    save_state, sync_state, processing_state,
                                    transcription_state, created_at, updated_at)
                 VALUES (?, 'capture-1', 0, ?, 'action', 'saved', 'not_configured', 'pending',
                         'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                rusqlite::params![ITEM_ID, lifecycle],
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

fn command(name: &str, command_id: &str, expected_revision: i64) -> Value {
    json!({"command": name, "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": expected_revision})
}

const ALL_COMMANDS: [&str; 4] = ["complete_item", "cancel_item", "not_now", "stop_suggesting"];

#[test]
fn completing_over_the_boundary_ends_the_item_and_cancels_its_reminder() {
    let _guard = serial();
    let store = Store::new("complete", "active", true);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&command("complete_item", "c-1", 0));

    assert_eq!(
        body,
        json!({"operation_id": 1, "outcome": "applied", "item_id": ITEM_ID, "revision": 1,
               "replayed": false})
    );
    assert_eq!(
        store.scalar("SELECT lifecycle_state FROM items"),
        "completed"
    );
    assert_eq!(store.scalar("SELECT revision FROM items"), "1");
    assert_eq!(
        store.scalar("SELECT event_id FROM events"),
        "item-control:c-1"
    );
    assert_eq!(store.scalar("SELECT event_type FROM events"), "completion");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "cancelled"
    );
}

#[test]
fn cancelling_over_the_boundary_ends_the_item_and_cancels_its_reminder() {
    let _guard = serial();
    let store = Store::new("cancel", "active", true);
    let mut session = Session::over(&store.path);

    let body = session.succeed(&command("cancel_item", "c-1", 0));

    assert_eq!(body["outcome"], "applied");
    assert_eq!(body["revision"], 1);
    assert_eq!(
        store.scalar("SELECT lifecycle_state FROM items"),
        "cancelled"
    );
    assert_eq!(
        store.scalar("SELECT event_type FROM events"),
        "cancellation"
    );
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "cancelled"
    );
}

#[test]
fn not_now_and_stop_suggesting_write_the_control_event_and_the_eligibility_row_only() {
    let _guard = serial();
    let snoozed = Store::new("not-now", "active", true);
    let mut session = Session::over(&snoozed.path);
    let body = session.succeed(&command("not_now", "c-1", 0));
    assert_eq!(body["outcome"], "applied");
    assert_eq!(body["revision"], 1);
    assert_eq!(
        snoozed.scalar("SELECT snoozed FROM suggestion_eligibility"),
        "1"
    );
    assert_eq!(
        snoozed.scalar("SELECT pull_only FROM suggestion_eligibility"),
        "0"
    );
    assert_eq!(
        snoozed.scalar("SELECT suggestion_control_kind FROM events"),
        "not_now"
    );
    assert_eq!(
        snoozed.scalar("SELECT lifecycle_state FROM items"),
        "active"
    );
    assert_eq!(
        snoozed.scalar("SELECT request_state FROM reminders"),
        "resolved"
    );
    drop(session);

    let muted = Store::new("stop", "active", true);
    let mut session = Session::over(&muted.path);
    let body = session.succeed(&command("stop_suggesting", "c-1", 0));
    assert_eq!(body["outcome"], "applied");
    assert_eq!(body["revision"], 1);
    assert_eq!(
        muted.scalar("SELECT pull_only FROM suggestion_eligibility"),
        "1"
    );
    assert_eq!(
        muted.scalar("SELECT suggestion_control_kind FROM events"),
        "stop_suggesting"
    );
    assert_eq!(muted.scalar("SELECT lifecycle_state FROM items"), "active");
    assert_eq!(
        muted.scalar("SELECT request_state FROM reminders"),
        "resolved"
    );
}

#[test]
fn a_stale_revision_is_an_answer_with_the_current_revision_and_writes_nothing() {
    let _guard = serial();
    let store = Store::new("stale", "active", true);
    let mut session = Session::over(&store.path);
    session.succeed(&command("not_now", "c-0", 0));

    for name in ALL_COMMANDS {
        let body = session.succeed(&command(name, &format!("c-{name}"), 0));
        assert_eq!(body["outcome"], "stale_revision", "{name}: {body}");
        assert_eq!(body["current_revision"], 1);
    }
    assert_eq!(store.count("events"), 1);
    assert_eq!(store.scalar("SELECT lifecycle_state FROM items"), "active");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "resolved"
    );
}

#[test]
fn a_repeated_command_id_replays_and_a_reused_one_conflicts() {
    let _guard = serial();
    let store = Store::new("replay", "active", true);
    let mut session = Session::over(&store.path);

    let first = session.succeed(&command("complete_item", "c-1", 0));
    let replay = session.succeed(&command("complete_item", "c-1", 0));
    assert_eq!(replay["outcome"], "applied");
    assert_eq!(replay["revision"], first["revision"]);
    assert_eq!(replay["replayed"], true);
    let replay_later = session.succeed(&command("complete_item", "c-1", 7));
    assert_eq!(replay_later["replayed"], true);

    let conflict = session.succeed(&command("cancel_item", "c-1", 1));
    assert_eq!(conflict["outcome"], "command_conflict");
    assert_eq!(store.count("events"), 1);
    assert_eq!(
        store.scalar("SELECT lifecycle_state FROM items"),
        "completed"
    );
}

#[test]
fn reopening_over_the_boundary_returns_the_item_and_the_stored_reminder_instant() {
    let _guard = serial();
    let store = Store::new("reopen", "active", true);
    let mut session = Session::over(&store.path);
    let before_instant = store.scalar("SELECT resolved_instant FROM reminders");

    let already_open = session.succeed(&command("reopen", "c-0", 0));
    assert_eq!(already_open["outcome"], "already_open");
    assert_eq!(store.count("events"), 0);

    session.succeed(&command("complete_item", "c-1", 0));
    let stale = session.succeed(&command("reopen", "c-2", 0));
    assert_eq!(
        stale,
        json!({"operation_id": 3, "outcome": "stale_revision", "current_revision": 1})
    );

    let body = session.succeed(&command("reopen", "c-2", 1));
    assert_eq!(body["outcome"], "reopened");
    assert_eq!(body["revision"], 2);
    assert_eq!(body["replayed"], false);
    assert_eq!(body["former_reminder_instant"], before_instant);
    assert_eq!(store.scalar("SELECT lifecycle_state FROM items"), "active");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders"),
        "cancelled"
    );

    let replay = session.succeed(&command("reopen", "c-2", 1));
    assert_eq!(replay["outcome"], "reopened");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["former_reminder_instant"], before_instant);
    assert_eq!(store.count("events"), 2);
}

#[test]
fn deleted_and_ended_items_are_answers_and_unknown_ones_are_not_found() {
    let _guard = serial();
    let deleted = Store::new("deleted", "deleted", true);
    let mut session = Session::over(&deleted.path);
    for name in ALL_COMMANDS {
        let body = session.succeed(&command(name, &format!("c-{name}"), 0));
        assert_eq!(body["outcome"], "item_deleted", "{name}");
    }
    assert_eq!(deleted.count("events"), 0);
    assert_eq!(deleted.count("suggestion_eligibility"), 0);
    drop(session);

    let ended = Store::new("ended", "completed", false);
    let mut session = Session::over(&ended.path);
    for name in ALL_COMMANDS {
        let body = session.succeed(&command(name, &format!("c-{name}"), 0));
        assert_eq!(body["outcome"], "item_ended", "{name}");
        assert_eq!(body["lifecycle"], "completed");
    }
    assert_eq!(ended.count("events"), 0);
    drop(session);

    let store = Store::new("unknown", "active", false);
    let mut session = Session::over(&store.path);
    let mut request = command("complete_item", "secret-command", 0);
    request["item_id"] = json!("secret-item");
    let event = session.call(&request);
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let payload = String::from_utf8_lossy(&event.payload).into_owned();
    assert!(payload.contains("not_found"), "{payload}");
    for secret in ["secret-command", "secret-item"] {
        assert!(!payload.contains(secret), "failure echoed {secret}");
    }
}

#[test]
fn malformed_commands_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = Store::new("invalid", "active", false);
    let mut session = Session::over(&store.path);

    session.refuse(&command("snooze_item", "c", 0));
    let mut forged = command("complete_item", "c", 0);
    forged["lifecycle"] = json!("cancelled");
    session.refuse(&forged);
    session.refuse(&json!({"command": "not_now", "item_id": ITEM_ID, "expected_revision": 0}));
    session.refuse(&json!({"command": "not_now", "command_id": "c", "item_id": ITEM_ID}));
    session.refuse(&command("complete_item", "", 0));
    session.refuse(&command("complete_item", &"c".repeat(513), 0));
    session.refuse(&command("complete_item", "c", -1));
    session.refuse(&command("complete_item", "c", 4_294_967_296));
    let mut no_item = command("stop_suggesting", "c", 0);
    no_item["item_id"] = json!("");
    session.refuse(&no_item);
    assert_eq!(store.count("events"), 0);

    let empty = consume(unsafe {
        ohand_core_start_item_lifecycle_control(session.handle, 99, std::ptr::null(), 0)
    });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe {
        ohand_core_start_item_lifecycle_control(session.handle, 99, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let too_large = consume(unsafe {
        ohand_core_start_item_lifecycle_control(
            session.handle,
            99,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");
}
