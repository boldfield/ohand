//! Drives the reset export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::store::captures::{save_capture, Capture};
use crate::store::schema::SystemClock;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe { ohand_core_start_reset(handle, operation_id, bytes.as_ptr(), bytes.len()) })
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

fn add_item(database: &mut Database, capture_id: &str, item_id: &str) {
    let capture = Capture::new(
        capture_id.to_string(),
        Some("synthetic reset text".to_string()),
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
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                                sync_state, processing_state, transcription_state, created_at,
                                updated_at)
             VALUES (?, ?, 0, 'active', 'saved', 'not_configured', 'pending', 'pending',
                     '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
            [item_id, capture_id],
        )
        .unwrap();
}

fn add_profile(database: &Database, version: &str, credential_ref: Option<&str>) {
    database
        .conn()
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint,
                    model, credential_ref, timeout_seconds, retry_policy, authorized_destinations,
                    capabilities, created_at)
             VALUES (?, 'profile-1', 'openai', NULL, 'model-a', ?, 30, '{}', '[]', '[]',
                     '2026-10-10T09:00:00Z')",
            rusqlite::params![version, credential_ref],
        )
        .unwrap();
}

fn store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-reset-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = open_database(&path);
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-10T09:00:00Z')",
            [],
        )
        .unwrap();
    add_item(&mut database, "capture-1", "item-1");
    add_item(&mut database, "capture-2", "item-2");
    add_profile(&database, "profile-1@1", Some("credential-a"));
    add_profile(&database, "profile-1@2", Some("credential-a"));
    add_profile(&database, "profile-1@3", None);
    (directory, path)
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<Event>,
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

    fn call(&mut self, request: &Value) -> Event {
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

    fn fail(&mut self, request: &Value) -> (u32, String) {
        let event = self.call(request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        let value: Value = serde_json::from_slice(&event.payload).unwrap();
        (event.status, value["code"].as_str().unwrap().to_string())
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

#[test]
fn status_is_empty_before_any_reset_and_advance_has_nothing_to_resume() {
    let _guard = serial();
    let (directory, path) = store("empty");
    let mut session = Session::over(open_file(&path));

    assert_eq!(
        session.succeed(&json!({"command": "status"}))["status"],
        Value::Null
    );
    assert_eq!(
        session.succeed(&json!({"command": "advance"}))["status"],
        Value::Null
    );
    assert_eq!(
        session.succeed(&json!({"command": "current_generation"}))["generation"],
        0
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn start_stays_pending_until_cleanup_is_acknowledged_then_new_captures_get_a_fresh_generation() {
    let _guard = serial();
    let (directory, path) = store("start");
    let mut session = Session::over(open_file(&path));

    let started = session.succeed(&json!({"command": "start"}));
    assert_eq!(started["status"]["generation"], 1);
    assert_eq!(started["status"]["phase"], "pending");
    assert_eq!(started["status"]["items_awaiting_tombstone"], 0);
    assert!(started["status"]["cleanup_pending"].as_u64().unwrap() > 0);
    assert_eq!(started["status"]["completed_at"], Value::Null);

    let again = session.succeed(&json!({"command": "start"}));
    assert_eq!(
        again["status"]["generation"], 1,
        "a second start resumes the pending reset"
    );
    assert_eq!(
        session.succeed(&json!({"command": "item_generation", "item_id": "item-1"}))["generation"],
        0
    );
    session.close();

    let mut database = open_database(&path);
    let work_ids: Vec<String> = {
        let mut statement = database
            .conn()
            .prepare("SELECT deletion_work_id FROM deletion_work")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    for work_id in work_ids {
        crate::lifecycle::delete_intent::mark_deletion_work_completed(
            &mut database,
            &work_id,
            Utc::now(),
        )
        .unwrap();
    }
    add_item(&mut database, "capture-3", "item-3");
    drop(database);

    let mut session = Session::over(open_file(&path));
    let advanced = session.succeed(&json!({"command": "advance"}));
    assert_eq!(advanced["status"]["phase"], "completed");
    assert_eq!(advanced["status"]["cleanup_pending"], 0);
    assert_ne!(advanced["status"]["completed_at"], Value::Null);
    assert_eq!(
        session.succeed(&json!({"command": "status"}))["status"]["phase"],
        "completed"
    );
    assert_eq!(
        session.succeed(&json!({"command": "item_generation", "item_id": "item-3"}))["generation"],
        1
    );
    assert_eq!(
        session.succeed(&json!({"command": "item_generation", "item_id": "item-1"}))["generation"],
        0
    );
    assert_eq!(
        session.succeed(&json!({"command": "item_generation", "item_id": "unknown"}))["generation"],
        Value::Null
    );
    assert_eq!(
        session.succeed(&json!({"command": "current_generation"}))["generation"],
        1
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn profiles_list_their_credential_references_and_revocation_is_idempotent() {
    let _guard = serial();
    let (directory, path) = store("profiles");
    let mut session = Session::over(open_file(&path));

    let listed = session.succeed(&json!({"command": "profile_credentials"}));
    assert_eq!(
        listed["profiles"],
        json!([
            {"profile_version": "profile-1@1", "credential_ref": "credential-a", "revoked": false},
            {"profile_version": "profile-1@2", "credential_ref": "credential-a", "revoked": false},
            {"profile_version": "profile-1@3", "credential_ref": null, "revoked": false},
        ])
    );
    session.succeed(&json!({"command": "revoke_profile", "profile_version": "profile-1@1"}));
    session.succeed(&json!({"command": "revoke_profile", "profile_version": "profile-1@1"}));
    let relisted = session.succeed(&json!({"command": "profile_credentials"}));
    assert_eq!(relisted["profiles"][0]["revoked"], true);
    assert_eq!(relisted["profiles"][1]["revoked"], false);

    let (status, code) =
        session.fail(&json!({"command": "revoke_profile", "profile_version": "no-such"}));
    assert_eq!((status, code.as_str()), (2, "reset_profile_refused"));
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let (directory, path) = store("invalid");
    let handle = open_file(&path);

    for request in [
        json!({"command": "unknown_command"}),
        json!({"command": "item_generation"}),
        json!({"command": "item_generation", "item_id": ""}),
        json!({"command": "start", "extra": 1}),
        json!({"command": "revoke_profile", "profile_version": "x".repeat(MAX_IDENTIFIER_BYTES + 1)}),
        json!([]),
    ] {
        let outcome = start(handle, 1, &request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }
    let empty = consume(unsafe { ohand_core_start_reset(handle, 2, [].as_ptr(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe { ohand_core_start_reset(handle, 3, std::ptr::null(), 4) });
    assert_eq!(null.code(), "null_argument");

    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let _ = std::fs::remove_dir_all(directory);
}
