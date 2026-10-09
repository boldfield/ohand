//! Drives the import export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, open_memory, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

const ROUTE_ID: &str = "route-personal";

fn record(capture_id: &str, text: &str) -> Value {
    json!({
        "capture_id": capture_id,
        "text": text,
        "capture_instant": "2026-10-08T09:30:00Z",
        "timezone_id": "UTC",
        "utc_offset_minutes": 0,
        "locale": "en_US",
        "calendar": "gregorian",
        "item_scope": "personal",
        "route_id": ROUTE_ID,
        "entry_locked": false,
        "created_at": "2026-10-08T09:30:01Z",
    })
}

fn start_import(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_import_foreground_ingress(
            handle,
            operation_id,
            bytes.as_ptr(),
            bytes.len(),
        )
    })
}

fn open_file(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
}

/// A store file with the personal route configured, as the settings flow will create it.
fn temporary_store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-ingress-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    let tx = database.transaction().unwrap();
    tx.execute(
        "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
         VALUES (?, ?, 'personal', '[]', '2026-10-08T09:00:00Z')",
        [ROUTE_ID, ROUTE_ID],
    )
    .unwrap();
    tx.commit().unwrap();
    (directory, path)
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<Event>,
    _recorder: Box<Recorder>,
}

impl Session {
    fn over(handle: OhandCoreHandle) -> Session {
        let (recorder, events) = recorder();
        register(handle, &recorder);
        Session {
            handle,
            events,
            _recorder: recorder,
        }
    }

    fn import(&self, operation_id: u64, request: &Value) -> Event {
        let outcome = start_import(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn success_json(event: &Event) -> Value {
    assert_eq!(
        event.status,
        OHAND_CORE_STATUS_OK,
        "{:?}",
        String::from_utf8_lossy(&event.payload)
    );
    serde_json::from_slice(&event.payload).unwrap()
}

fn failure_of(event: &Event) -> (u32, String, String) {
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let value: Value = serde_json::from_slice(&event.payload).unwrap();
    (
        event.status,
        value["code"].as_str().unwrap().to_string(),
        value["message"].as_str().unwrap().to_string(),
    )
}

#[test]
fn an_import_acknowledges_with_the_item_only_after_it_committed() {
    let _guard = serial();
    let (directory, path) = temporary_store("ack");
    let session = Session::over(open_file(&path));

    let ack = success_json(&session.import(1, &record("capture-1", "buy oat milk")));
    assert_eq!(ack["operation_id"], 1);
    assert_eq!(ack["capture_id"], "capture-1");
    assert_eq!(ack["disposition"], "imported");
    assert_eq!(ack["saved_at"], "2026-10-08T09:30:01Z");
    assert!(!ack["item_id"].as_str().unwrap().is_empty());
    session.close();

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    let (items, save_state): (i64, String) = database
        .conn()
        .query_row("SELECT COUNT(*), MAX(save_state) FROM items", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((items, save_state.as_str()), (1, "saved_local"));
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn repeated_delivery_converges_across_a_restart() {
    let _guard = serial();
    let (directory, path) = temporary_store("repeat");
    let request = record("capture-repeat", "call the roofer");

    let first = Session::over(open_file(&path));
    let original = success_json(&first.import(1, &request));
    let again = success_json(&first.import(2, &request));
    assert_eq!(again["disposition"], "already_imported");
    assert_eq!(again["item_id"], original["item_id"]);
    first.close();

    let second = Session::over(open_file(&path));
    let after_restart = success_json(&second.import(3, &request));
    assert_eq!(after_restart["disposition"], "already_imported");
    assert_eq!(after_restart["item_id"], original["item_id"]);
    assert_eq!(after_restart["saved_at"], original["saved_at"]);
    second.close();

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    let items: i64 = database
        .conn()
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(items, 1);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn audio_only_records_import_with_their_reference() {
    let _guard = serial();
    let (directory, path) = temporary_store("audio");
    let session = Session::over(open_file(&path));
    let mut request = record("capture-audio", "");
    request["text"] = Value::Null;
    request["audio_reference"] = json!("FinalizedAudio/capture-audio.m4a");
    let ack = success_json(&session.import(1, &request));
    assert_eq!(ack["disposition"], "imported");
    session.close();

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    let (reference, transcription): (String, String) = database
        .conn()
        .query_row(
            "SELECT c.audio_reference, i.transcription_state FROM captures c
             JOIN items i ON i.capture_id = c.capture_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(reference, "FinalizedAudio/capture-audio.m4a");
    assert_eq!(transcription, "audio_pending");
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn refusals_name_their_cause_and_write_nothing() {
    let _guard = serial();
    let (directory, path) = temporary_store("refusals");
    let session = Session::over(open_file(&path));
    let original = success_json(&session.import(1, &record("capture-kept", "original words")));

    let mut unknown_route = record("capture-route", "text");
    unknown_route["route_id"] = json!("route-missing");
    let mut work_scope = record("capture-scope", "text");
    work_scope["item_scope"] = json!("work");
    let mut bad_scope = record("capture-bad-scope", "text");
    bad_scope["item_scope"] = json!("shared");
    let mut no_content = record("capture-empty", "");
    no_content["text"] = Value::Null;
    let mut bad_time = record("capture-time", "text");
    bad_time["timezone_id"] = json!("Mars/Olympus");
    let mut bad_audio = record("capture-audio-escape", "");
    bad_audio["text"] = Value::Null;
    bad_audio["audio_reference"] = json!("../outside.m4a");
    let mut missing_field = record("capture-missing", "text");
    missing_field["locale"] = json!("");

    let expectations = [
        (unknown_route, "ingress_unknown_route"),
        (work_scope, "ingress_route_scope_mismatch"),
        (bad_scope, "ingress_unsupported_scope"),
        (no_content, "ingress_missing_content"),
        (bad_time, "ingress_malformed_time_context"),
        (bad_audio, "ingress_malformed_audio_reference"),
        (missing_field, "ingress_missing_field"),
        (
            record("capture-kept", "different words"),
            "ingress_conflicting_reuse",
        ),
    ];
    for (offset, (request, expected_code)) in expectations.iter().enumerate() {
        let (status, code, message) = failure_of(&session.import(10 + offset as u64, request));
        assert_eq!(status, OHAND_CORE_STATUS_PERMANENT, "{expected_code}");
        assert_eq!(code, *expected_code);
        assert!(!message.contains("route-missing") && !message.contains("different words"));
    }

    let kept = success_json(&session.import(30, &record("capture-kept", "original words")));
    assert_eq!(kept["item_id"], original["item_id"]);
    session.close();

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    let (captures, items): (i64, i64) = database
        .conn()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM captures), (SELECT COUNT(*) FROM items)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((captures, items), (1, 1), "refusals left no partial write");
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_deleted_item_is_reported_terminal_and_never_recreated() {
    let _guard = serial();
    let (directory, path) = temporary_store("deleted");
    let session = Session::over(open_file(&path));
    let request = record("capture-deleted", "gone");
    let ack = success_json(&session.import(1, &request));
    session.close();

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    database
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
            [ack["item_id"].as_str().unwrap()],
        )
        .unwrap();
    drop(database);

    let reopened = Session::over(open_file(&path));
    let (status, code, _) = failure_of(&reopened.import(2, &request));
    assert_eq!(
        (status, code.as_str()),
        (OHAND_CORE_STATUS_PERMANENT, "ingress_item_deleted")
    );
    reopened.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_busy_store_commits_nothing_and_a_retry_succeeds() {
    let _guard = serial();
    let (directory, path) = temporary_store("busy");
    let session = Session::over(open_file(&path));
    let request = record("capture-busy", "retry me");

    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (status, code, _) = failure_of(&session.import(1, &request));
    assert_eq!(
        (status, code.as_str()),
        (OHAND_CORE_STATUS_TRANSIENT, "ingress_not_committed")
    );
    blocker.execute_batch("ROLLBACK").unwrap();

    let retried = success_json(&session.import(2, &request));
    assert_eq!(retried["disposition"], "imported");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let handle = open_memory();

    let empty = consume(unsafe {
        ohand_core_start_import_foreground_ingress(handle, 1, std::ptr::null(), 0)
    });
    assert_eq!(
        (empty.status, empty.code().as_str()),
        (OHAND_CORE_STATUS_PERMANENT, "invalid_request")
    );

    let null = consume(unsafe {
        ohand_core_start_import_foreground_ingress(handle, 2, std::ptr::null(), 4)
    });
    assert_eq!(null.code(), "null_argument");

    let too_large = consume(unsafe {
        ohand_core_start_import_foreground_ingress(
            handle,
            3,
            std::ptr::NonNull::dangling().as_ptr(),
            OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.code(), "request_too_large");

    let mut with_unknown_field = record("capture-x", "text");
    with_unknown_field["extra"] = json!(true);
    let unknown = start_import(handle, 4, &with_unknown_field);
    assert_eq!(unknown.code(), "invalid_request");

    let not_json =
        consume(unsafe { ohand_core_start_import_foreground_ingress(handle, 5, b"{".as_ptr(), 1) });
    assert_eq!(not_json.code(), "invalid_request");

    let stale =
        consume(unsafe { ohand_core_start_import_foreground_ingress(0, 6, b"{}".as_ptr(), 2) });
    assert_eq!(stale.code(), "invalid_handle");
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
}
