//! Drives the notification-action export the way Swift does, against the real core store.

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
const CURRENT: &str = "rem-1#2";
const SUPERSEDED: &str = "rem-1#1";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_notification_action(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn open_file(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
}

fn open_database(path: &str) -> Database {
    Database::open(path, Arc::new(SystemClock)).unwrap()
}

fn insert_item(database: &mut Database, item_id: &str, capture_id: &str, lifecycle: &str) {
    let capture = Capture::new(
        capture_id.to_string(),
        Some("synthetic capture".to_string()),
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
    save_capture(database, &capture).unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type, save_state,
                                sync_state, processing_state, transcription_state, created_at, updated_at)
             VALUES (?, ?, 0, ?, 'action', 'saved', 'not_configured', 'pending', 'pending',
                     '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
            rusqlite::params![item_id, capture_id, lifecycle],
        )
        .unwrap();
}

/// An active action item whose reminder is on its second schedule generation. Generation 1 was
/// handed to the OS and superseded by generation 2, which was also handed to the OS.
fn reminder_store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-actions-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = open_database(&path);
    insert_item(&mut database, ITEM_ID, "capture-1", "active");
    let conn = database.conn();
    conn.execute(
        "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state,
                                acknowledgment_state, resolved_instant, timezone_id,
                                schedule_generation, created_at, updated_at)
         VALUES (?, ?, 'resolved', 'scheduled', 'unknown', 'not_acknowledged',
                 '2090-01-01T09:00:00Z', 'UTC', 2, '2026-10-10T09:31:00Z', '2026-10-10T09:32:00Z')",
        [REMINDER_ID, ITEM_ID],
    )
    .unwrap();
    for (notification_id, state) in [(SUPERSEDED, "acknowledged"), (CURRENT, "acknowledged")] {
        conn.execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type,
                    operation_state, effect_identity, external_id, created_at)
             VALUES (?, ?, 'schedule', ?, ?, ?, '2026-10-10T09:32:00Z')",
            rusqlite::params![
                format!("schedule:{notification_id}"),
                REMINDER_ID,
                state,
                format!("schedule:{notification_id}"),
                notification_id
            ],
        )
        .unwrap();
    }
    (directory, path)
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<CoreEvent>,
    _recorder: Box<Recorder>,
    next_operation_id: u64,
}

impl Session {
    fn over(handle: OhandCoreHandle) -> Session {
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

    fn outcome(&mut self, request: &Value) -> String {
        self.succeed(request)["outcome"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn scalar(path: &str, sql: &str) -> String {
    open_database(path)
        .conn()
        .query_row(sql, [], |row| row.get::<_, String>(0))
        .unwrap()
}

fn lifecycle(path: &str) -> String {
    scalar(
        path,
        &format!("SELECT lifecycle_state FROM items WHERE item_id = '{ITEM_ID}'"),
    )
}

fn revision(path: &str) -> i64 {
    open_database(path)
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [ITEM_ID],
            |row| row.get(0),
        )
        .unwrap()
}

fn acknowledgment(path: &str) -> String {
    scalar(
        path,
        &format!("SELECT acknowledgment_state FROM reminders WHERE reminder_id = '{REMINDER_ID}'"),
    )
}

fn complete(notification_id: &str) -> Value {
    json!({"command": "complete", "notification_id": notification_id})
}

fn acknowledge(notification_id: &str) -> Value {
    json!({"command": "acknowledge", "notification_id": notification_id})
}

#[test]
fn acknowledge_and_complete_are_distinct_and_repeats_change_nothing() {
    let _guard = serial();
    let (directory, path) = reminder_store("distinct");
    let mut session = Session::over(open_file(&path));

    assert_eq!(session.outcome(&acknowledge(CURRENT)), "recorded");
    assert_eq!(acknowledgment(&path), "acknowledged");
    assert_eq!(lifecycle(&path), "active", "acknowledging never completes");
    assert_eq!(revision(&path), 0);
    assert_eq!(session.outcome(&acknowledge(CURRENT)), "already_recorded");

    assert_eq!(session.outcome(&complete(CURRENT)), "completed");
    assert_eq!(lifecycle(&path), "completed");
    assert_eq!(revision(&path), 1, "one revisioned completion event");
    assert_eq!(
        scalar(
            &path,
            &format!("SELECT request_state FROM reminders WHERE reminder_id = '{REMINDER_ID}'")
        ),
        "cancelled",
        "completing cancels the reminder in the same transaction"
    );

    assert_eq!(session.outcome(&complete(CURRENT)), "already_ended");
    assert_eq!(revision(&path), 1, "a duplicate callback does not mutate");
    assert_eq!(
        scalar(
            &path,
            "SELECT CAST(COUNT(*) AS TEXT) FROM events WHERE event_type = 'completion'"
        ),
        "1"
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn completing_never_acknowledges() {
    let _guard = serial();
    let (directory, path) = reminder_store("complete-only");
    let mut session = Session::over(open_file(&path));
    assert_eq!(session.outcome(&complete(CURRENT)), "completed");
    assert_eq!(acknowledgment(&path), "not_acknowledged");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_superseded_generation_cannot_complete_the_item() {
    let _guard = serial();
    let (directory, path) = reminder_store("stale");
    let mut session = Session::over(open_file(&path));
    assert_eq!(session.outcome(&complete(SUPERSEDED)), "stale_notification");
    assert_eq!(lifecycle(&path), "active");
    assert_eq!(revision(&path), 0);

    assert_eq!(
        session.outcome(&acknowledge(SUPERSEDED)),
        "recorded_superseded"
    );
    assert_eq!(
        acknowledgment(&path),
        "not_acknowledged",
        "an older generation's acknowledgment never marks the current one"
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn unknown_malformed_and_mismatched_identifiers_change_nothing() {
    let _guard = serial();
    let (directory, path) = reminder_store("unknown");
    let mut session = Session::over(open_file(&path));
    for notification_id in ["other#1", "rem-1#9x", "rem-1", "rem-1#02", "rem-1#0", "#2"] {
        assert_eq!(
            session.outcome(&complete(notification_id)),
            "unknown_notification",
            "{notification_id}"
        );
        assert_eq!(
            session.outcome(&acknowledge(notification_id)),
            "unknown_notification",
            "{notification_id}"
        );
    }
    assert_eq!(
        session.outcome(&complete("rem-1#3")),
        "unknown_notification",
        "a generation core never scheduled"
    );
    assert_eq!(
        session.outcome(
            &json!({"command": "complete", "notification_id": CURRENT, "item_id": "someone-else"})
        ),
        "item_mismatch"
    );
    assert_eq!(
        session.outcome(
            &json!({"command": "acknowledge", "notification_id": CURRENT, "item_id": "someone-else"})
        ),
        "item_mismatch"
    );
    assert_eq!(lifecycle(&path), "active");
    assert_eq!(revision(&path), 0);
    assert_eq!(acknowledgment(&path), "not_acknowledged");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_request_never_handed_to_the_os_cannot_complete_the_item() {
    let _guard = serial();
    let (directory, path) = reminder_store("not-installed");
    open_database(&path)
        .conn()
        .execute(
            "UPDATE reminder_operations SET operation_state = 'pending' WHERE external_id = ?",
            [CURRENT],
        )
        .unwrap();
    let mut session = Session::over(open_file(&path));
    assert_eq!(session.outcome(&complete(CURRENT)), "not_installed");
    assert_eq!(session.outcome(&acknowledge(CURRENT)), "not_installed");
    assert_eq!(lifecycle(&path), "active");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_deleted_or_cancelled_item_is_not_resurrected_or_mutated() {
    let _guard = serial();
    let (directory, path) = reminder_store("ended");
    let mut session = Session::over(open_file(&path));

    open_database(&path)
        .conn()
        .execute("UPDATE items SET lifecycle_state = 'cancelled'", [])
        .unwrap();
    assert_eq!(session.outcome(&complete(CURRENT)), "already_ended");
    assert_eq!(
        lifecycle(&path),
        "cancelled",
        "completion never overwrites a cancellation"
    );

    open_database(&path)
        .conn()
        .execute("UPDATE items SET lifecycle_state = 'deleted'", [])
        .unwrap();
    assert_eq!(session.outcome(&complete(CURRENT)), "item_deleted");
    assert_eq!(session.outcome(&acknowledge(CURRENT)), "item_deleted");
    assert_eq!(
        session.outcome(
            &json!({"command": "record_evidence", "notification_id": CURRENT,
                    "kind": "opened", "observed_at_unix_ms": now_ms()})
        ),
        "item_deleted"
    );
    assert_eq!(
        session.outcome(&json!({"command": "resolve", "notification_id": CURRENT})),
        "item_deleted"
    );
    assert_eq!(lifecycle(&path), "deleted");
    assert_eq!(revision(&path), 0);
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn delivery_and_opening_evidence_is_idempotent_and_never_acknowledges() {
    let _guard = serial();
    let (directory, path) = reminder_store("evidence");
    let mut session = Session::over(open_file(&path));
    let evidence = |kind: &str, at: i64| {
        json!({"command": "record_evidence", "notification_id": CURRENT,
               "kind": kind, "observed_at_unix_ms": at})
    };
    assert_eq!(
        session.outcome(&evidence("delivered", now_ms() - 1000)),
        "recorded"
    );
    assert_eq!(
        session.outcome(&evidence("delivered", now_ms() - 500)),
        "already_recorded"
    );
    assert_eq!(
        session.outcome(&evidence("opened", now_ms() - 100)),
        "recorded"
    );
    assert_eq!(
        session.outcome(&evidence("opened", now_ms())),
        "already_recorded"
    );
    assert_eq!(
        scalar(
            &path,
            &format!("SELECT delivery_state FROM reminders WHERE reminder_id = '{REMINDER_ID}'")
        ),
        "opened"
    );
    assert_eq!(acknowledgment(&path), "not_acknowledged");
    assert_eq!(
        session.outcome(&evidence("delivered", now_ms() + 3_600_000)),
        "future_observation"
    );
    assert_eq!(
        session.outcome(
            &json!({"command": "record_evidence", "notification_id": "ghost#1",
                    "kind": "delivered", "observed_at_unix_ms": now_ms()})
        ),
        "unknown_notification"
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn resolve_names_the_item_a_tap_opens() {
    let _guard = serial();
    let (directory, path) = reminder_store("resolve");
    let mut session = Session::over(open_file(&path));
    let current = session.succeed(&json!({"command": "resolve", "notification_id": CURRENT}));
    assert_eq!(current["outcome"], "item");
    assert_eq!(current["item_id"], ITEM_ID);
    assert_eq!(current["lifecycle"], "active");
    assert_eq!(current["current_generation"], true);
    let older = session.succeed(&json!({"command": "resolve", "notification_id": SUPERSEDED}));
    assert_eq!(older["outcome"], "item");
    assert_eq!(older["current_generation"], false);
    assert_eq!(
        session.outcome(&json!({"command": "resolve", "notification_id": "ghost#1"})),
        "unknown_notification"
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_before_anything_runs() {
    let _guard = serial();
    let (directory, path) = reminder_store("invalid");
    let handle = open_file(&path);
    let session = Session::over(handle);
    for request in [
        json!({"command": "unknown", "notification_id": CURRENT}),
        json!({"command": "complete", "notification_id": ""}),
        json!({"command": "complete", "notification_id": CURRENT, "extra": 1}),
        json!({"command": "complete", "notification_id": "x".repeat(MAX_IDENTIFIER_BYTES + 1)}),
        json!({"command": "record_evidence", "notification_id": CURRENT, "kind": "seen",
               "observed_at_unix_ms": 0}),
        json!({"command": "complete"}),
    ] {
        let outcome = start(session.handle, 99, &request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
    }
    let outcome = start(
        session.handle,
        99,
        &json!({"command": "complete", "notification_id": CURRENT}),
    );
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    let event = session.events.recv_timeout(WAIT).unwrap();
    assert_eq!(event.operation_id, 99);
    assert_eq!(lifecycle(&path), "completed");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}
