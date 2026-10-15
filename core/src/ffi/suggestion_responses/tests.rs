//! Drives the suggestion export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::store::captures::{save_capture, Capture};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const PROMPT_ID: &str = "prompt:2026-10-08";
const ITEM_A: &str = "item-a";
const ITEM_B: &str = "item-b";
const TEXT_A: &str = "synthetic task about parsnips";
const TEXT_B: &str = "synthetic task about leeks";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_suggestion_response(handle, operation_id, bytes.as_ptr(), bytes.len())
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
    /// Active items of the given `(item_id, item_type, scope, text)`.
    fn new(name: &str, items: &[(&str, &str, &str, &str)]) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-suggestions-ffi-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let mut database = open_database(&path);
        for (item_id, item_type, scope, text) in items {
            let capture_id = format!("capture-{item_id}");
            let capture = Capture::new(
                capture_id.clone(),
                Some((*text).to_string()),
                None,
                "2026-10-10T09:30:00+00:00".to_string(),
                "UTC".to_string(),
                0,
                "en_US".to_string(),
                "gregorian".to_string(),
                (*scope).to_string(),
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
                     VALUES (?, ?, 0, 'active', ?, 'saved', 'not_configured', 'pending',
                             'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                    rusqlite::params![item_id, capture_id, item_type],
                )
                .unwrap();
        }
        drop(database);
        Store { directory, path }
    }

    fn add_live_reminder(&self, item_id: &str) {
        open_database(&self.path)
            .conn()
            .execute(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state,
                                        delivery_state, acknowledgment_state,
                                        resolved_instant, timezone_id, schedule_generation,
                                        created_at, updated_at)
                 VALUES ('rem-1', ?, 'resolved', 'scheduled', 'unknown', 'not_acknowledged',
                         '2090-01-01T09:00:00Z', 'UTC', 1, '2026-10-10T09:31:00Z',
                         '2026-10-10T09:32:00Z')",
                [item_id],
            )
            .unwrap();
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
        self.scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .parse()
            .unwrap()
    }

    fn revision(&self, item_id: &str) -> i64 {
        self.scalar(&format!(
            "SELECT revision FROM items WHERE item_id = '{item_id}'"
        ))
        .parse()
        .unwrap()
    }

    fn lifecycle(&self, item_id: &str) -> String {
        self.scalar(&format!(
            "SELECT lifecycle_state FROM items WHERE item_id = '{item_id}'"
        ))
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

    /// The failure event for `request`: its status, and the raw payload.
    fn fail(&mut self, request: &Value) -> (u32, String) {
        let event = self.call(request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        (
            event.status,
            String::from_utf8_lossy(&event.payload).into_owned(),
        )
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

fn activate() -> Value {
    json!({"command": "activate_prompt", "read_authentication": "authenticated",
           "notification_id": PROMPT_ID})
}

fn respond(command_id: &str, item_id: &str, expected_revision: i64, response: &str) -> Value {
    json!({"command": "respond", "read_authentication": "authenticated",
           "command_id": command_id, "item_id": item_id,
           "expected_revision": expected_revision, "response": response})
}

fn two_actions(name: &str) -> Store {
    Store::new(
        name,
        &[
            (ITEM_A, "action", "personal", TEXT_A),
            (ITEM_B, "action", "work", TEXT_B),
        ],
    )
}

#[test]
fn activation_offers_one_eligible_item_with_its_displayable_text() {
    let _guard = serial();
    let store = two_actions("offer");
    let mut session = Session::over(&store.path);

    let body = session.succeed(&activate());
    assert_eq!(body["outcome"], "offer");
    assert_eq!(body["item_id"], ITEM_A);
    assert_eq!(body["revision"], 0);
    assert_eq!(body["text"], TEXT_A);
    assert_eq!(body["scope"], "personal");
    assert_eq!(
        body["responses"],
        json!(["not_now", "stop_suggesting", "done"])
    );
    assert!(body["selection_reason"].is_string());
    assert_eq!(
        store.count("suggestion_eligibility"),
        1,
        "the offer is stamped as selected so the next activation rotates"
    );

    let next = session.succeed(&activate());
    assert_eq!(next["item_id"], ITEM_B, "offering rotates");
}

#[test]
fn activation_can_be_limited_to_one_scope() {
    let _guard = serial();
    let store = two_actions("scope");
    let mut session = Session::over(&store.path);
    let mut request = activate();
    request["scope"] = json!("work");
    assert_eq!(session.succeed(&request)["item_id"], ITEM_B);
    assert_eq!(session.succeed(&request)["item_id"], ITEM_B);
}

#[test]
fn a_capture_only_session_is_refused_before_anything_is_read_or_stamped() {
    let _guard = serial();
    let store = two_actions("locked");
    let mut session = Session::over(&store.path);

    let mut locked_activation = activate();
    locked_activation["read_authentication"] = json!("capture_only");
    let (status, payload) = session.fail(&locked_activation);
    assert_eq!(status, OHAND_CORE_STATUS_UNAUTHORIZED);
    assert!(payload.contains("suggestion_not_authorized"), "{payload}");
    for private in [TEXT_A, TEXT_B, ITEM_A, ITEM_B] {
        assert!(!payload.contains(private), "failure disclosed {private}");
    }
    assert_eq!(store.count("suggestion_eligibility"), 0);

    let mut locked_response = respond("c-1", ITEM_A, 0, "done");
    locked_response["read_authentication"] = json!("capture_only");
    let (status, payload) = session.fail(&locked_response);
    assert_eq!(status, OHAND_CORE_STATUS_UNAUTHORIZED);
    assert!(payload.contains("suggestion_not_authorized"), "{payload}");
    assert_eq!(store.count("events"), 0);
    assert_eq!(store.lifecycle(ITEM_A), "active");
}

#[test]
fn no_eligible_item_is_a_calm_nothing_to_offer() {
    let _guard = serial();
    let store = Store::new("nothing", &[(ITEM_A, "note", "personal", TEXT_A)]);
    let mut session = Session::over(&store.path);
    assert_eq!(
        session.succeed(&activate()),
        json!({"operation_id": 1, "outcome": "nothing_to_offer"})
    );

    let empty = Store::new("empty", &[]);
    let mut session = Session::over(&empty.path);
    assert_eq!(session.succeed(&activate())["outcome"], "nothing_to_offer");
}

#[test]
fn a_notification_that_is_not_a_daily_prompt_fails_without_echoing_it() {
    let _guard = serial();
    let store = two_actions("not-a-prompt");
    let mut session = Session::over(&store.path);
    let mut request = activate();
    request["notification_id"] = json!("reminder:secret-item");
    let (status, payload) = session.fail(&request);
    assert_eq!(status, OHAND_CORE_STATUS_PERMANENT);
    assert!(payload.contains("invalid_request"), "{payload}");
    assert!(!payload.contains("secret-item"), "{payload}");
    assert_eq!(store.count("suggestion_eligibility"), 0);
}

#[test]
fn not_now_records_the_cooldown_and_leaves_the_item_active() {
    let _guard = serial();
    let store = two_actions("not-now");
    let mut session = Session::over(&store.path);
    assert_eq!(session.succeed(&activate())["item_id"], ITEM_A);

    let body = session.succeed(&respond("c-1", ITEM_A, 0, "not_now"));
    assert_eq!(
        body,
        json!({"operation_id": 2, "outcome": "applied", "replayed": false})
    );
    assert_eq!(
        store.scalar("SELECT event_id FROM events"),
        "prompt-response:c-1"
    );
    assert_eq!(store.lifecycle(ITEM_A), "active");
    assert_eq!(
        session.succeed(&activate())["item_id"],
        ITEM_B,
        "the snoozed item is not offered again during its cooldown"
    );
    assert_eq!(
        session.succeed(&activate())["item_id"],
        ITEM_B,
        "rotation alone would offer item-a here; the cooldown keeps it out"
    );
}

#[test]
fn stop_suggesting_keeps_the_item_but_never_offers_it_again() {
    let _guard = serial();
    let store = two_actions("stop");
    let mut session = Session::over(&store.path);
    assert_eq!(session.succeed(&activate())["item_id"], ITEM_A);

    let body = session.succeed(&respond("c-1", ITEM_A, 0, "stop_suggesting"));
    assert_eq!(body["outcome"], "applied");
    assert_eq!(store.lifecycle(ITEM_A), "active");
    for _ in 0..3 {
        assert_eq!(session.succeed(&activate())["item_id"], ITEM_B);
    }
}

#[test]
fn done_completes_the_item_and_cancels_its_own_reminder_in_the_same_transaction() {
    let _guard = serial();
    let store = Store::new("done", &[(ITEM_A, "action", "personal", TEXT_A)]);
    let mut session = Session::over(&store.path);
    assert_eq!(session.succeed(&activate())["item_id"], ITEM_A);
    store.add_live_reminder(ITEM_A);

    let body = session.succeed(&respond("c-1", ITEM_A, 0, "done"));
    assert_eq!(body["outcome"], "applied");
    assert_eq!(store.lifecycle(ITEM_A), "completed");
    assert_eq!(
        store.scalar("SELECT request_state FROM reminders WHERE item_id = 'item-a'"),
        "cancelled"
    );
    assert_eq!(
        session.succeed(&activate())["outcome"],
        "nothing_to_offer",
        "a completed item is never offered"
    );
}

#[test]
fn a_repeated_command_id_reports_applied_again_and_writes_nothing_new() {
    let _guard = serial();
    let store = two_actions("replay");
    let mut session = Session::over(&store.path);
    assert_eq!(
        session.succeed(&respond("c-1", ITEM_A, 0, "done"))["replayed"],
        false
    );
    let events = store.count("events");
    let revision = store.revision(ITEM_A);

    let again = session.succeed(&respond("c-1", ITEM_A, 0, "done"));
    assert_eq!(again["outcome"], "applied");
    assert_eq!(again["replayed"], true);
    assert_eq!(store.count("events"), events);
    assert_eq!(store.revision(ITEM_A), revision);
}

#[test]
fn reusing_a_command_id_for_a_different_response_is_a_conflict() {
    let _guard = serial();
    let store = two_actions("conflict");
    let mut session = Session::over(&store.path);
    session.succeed(&respond("c-1", ITEM_A, 0, "not_now"));
    let events = store.count("events");

    let body = session.succeed(&respond("c-1", ITEM_A, 0, "done"));
    assert_eq!(body["outcome"], "command_conflict");
    assert_eq!(store.count("events"), events);
    assert_eq!(store.lifecycle(ITEM_A), "active");
}

#[test]
fn a_stale_prompt_reports_the_current_revision_and_writes_nothing() {
    let _guard = serial();
    let store = two_actions("stale");
    let mut session = Session::over(&store.path);
    assert_eq!(session.succeed(&activate())["revision"], 0);
    session.succeed(&respond("c-1", ITEM_A, 0, "stop_suggesting"));
    assert_eq!(store.revision(ITEM_A), 1);
    let events = store.count("events");

    let body = session.succeed(&respond("c-2", ITEM_A, 0, "not_now"));
    assert_eq!(
        body,
        json!({"operation_id": 3, "outcome": "stale_revision", "current_revision": 1})
    );
    assert_eq!(store.count("events"), events);
}

#[test]
fn a_replaced_prompt_is_no_longer_current_and_its_old_item_is_not_offered() {
    let _guard = serial();
    let store = two_actions("replaced");
    let mut session = Session::over(&store.path);
    let offered = session.succeed(&activate());
    assert_eq!(offered["item_id"], ITEM_A);

    session.succeed(&respond("done-elsewhere", ITEM_A, 0, "done"));
    let events = store.count("events");

    let body = session.succeed(&respond("c-late", ITEM_A, 0, "not_now"));
    assert_eq!(body["outcome"], "no_longer_current");
    assert_eq!(store.count("events"), events);

    let replacement = session.succeed(&activate());
    assert_eq!(replacement["item_id"], ITEM_B);

    let unknown = session.succeed(&respond("c-unknown", "item-gone", 0, "done"));
    assert_eq!(unknown["outcome"], "no_longer_current");
}

#[test]
fn malformed_commands_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = two_actions("invalid");
    let mut session = Session::over(&store.path);

    session.refuse(&json!({"command": "snooze", "read_authentication": "authenticated"}));
    let mut forged = activate();
    forged["selected_item"] = json!(ITEM_A);
    session.refuse(&forged);
    let mut no_authentication = activate();
    no_authentication
        .as_object_mut()
        .unwrap()
        .remove("read_authentication");
    session.refuse(&no_authentication);
    let mut unknown_authentication = activate();
    unknown_authentication["read_authentication"] = json!("trusted");
    session.refuse(&unknown_authentication);
    let mut empty_notification = activate();
    empty_notification["notification_id"] = json!("");
    session.refuse(&empty_notification);
    let mut unknown_scope = activate();
    unknown_scope["scope"] = json!("family");
    session.refuse(&unknown_scope);

    session.refuse(&respond("", ITEM_A, 0, "done"));
    session.refuse(&respond(&"c".repeat(513), ITEM_A, 0, "done"));
    session.refuse(&respond("c", "", 0, "done"));
    session.refuse(&respond("c", ITEM_A, -1, "done"));
    session.refuse(&respond("c", ITEM_A, 4_294_967_296, "done"));
    session.refuse(&respond("c", ITEM_A, 0, "archive"));
    assert_eq!(store.count("events"), 0);
    assert_eq!(store.count("suggestion_eligibility"), 0);

    let empty = consume(unsafe {
        ohand_core_start_suggestion_response(session.handle, 99, std::ptr::null(), 0)
    });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe {
        ohand_core_start_suggestion_response(session.handle, 99, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");
    let too_large = consume(unsafe {
        ohand_core_start_suggestion_response(
            session.handle,
            99,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");
}
