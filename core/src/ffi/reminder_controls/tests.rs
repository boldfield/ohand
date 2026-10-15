//! Drives the reminder-control exports the way Swift does, against the real core store.

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
const FUTURE: &str = "2090-01-01T09:00:00Z";
const LATER: &str = "2090-02-03T10:30:00Z";

fn start_read(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_reminder_read(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn start_control(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_reminder_control(handle, operation_id, bytes.as_ptr(), bytes.len())
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
    fn new(name: &str, item_type: &str, lifecycle: &str, scope: &str) -> Store {
        let directory = std::env::temp_dir().join(format!(
            "ohand-reminder-controls-ffi-{name}-{}",
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
            scope.to_string(),
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
                 VALUES (?, 'capture-1', 0, ?, ?, 'saved', 'not_configured', 'pending',
                         'pending', '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
                rusqlite::params![ITEM_ID, lifecycle, item_type],
            )
            .unwrap();
        drop(database);
        Store { directory, path }
    }

    fn action(name: &str) -> Store {
        Store::new(name, "action", "active", "personal")
    }

    fn count(&self, table: &str) -> i64 {
        open_database(&self.path)
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn instant(&self) -> Option<String> {
        open_database(&self.path)
            .conn()
            .query_row("SELECT resolved_instant FROM reminders", [], |row| {
                row.get(0)
            })
            .ok()
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

    fn event(&mut self, read: bool, request: &Value) -> CoreEvent {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = if read {
            start_read(self.handle, operation_id, request)
        } else {
            start_control(self.handle, operation_id, request)
        };
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn body(event: CoreEvent) -> Value {
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

    fn read(&mut self, request: &Value) -> Value {
        Session::body(self.event(true, request))
    }

    fn write(&mut self, request: &Value) -> Value {
        Session::body(self.event(false, request))
    }

    fn refuse(&mut self, read: bool, request: &Value) {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = if read {
            start_read(self.handle, operation_id, request)
        } else {
            start_control(self.handle, operation_id, request)
        };
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

fn get(scopes: &[&str]) -> Value {
    json!({"command": "get_reminder", "item_id": ITEM_ID, "read_scopes": scopes})
}

fn history(scopes: &[&str]) -> Value {
    json!({"command": "reminder_history", "item_id": ITEM_ID, "read_scopes": scopes})
}

fn edit(command_id: &str, revision: i64, version: i64, instant: &str) -> Value {
    json!({"command": "correct_time", "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": revision, "expected_reminder_version": version,
           "instant": instant, "timezone_id": "Europe/Dublin"})
}

fn cancel(command_id: &str, revision: i64, version: i64) -> Value {
    json!({"command": "cancel", "command_id": command_id, "item_id": ITEM_ID,
           "expected_revision": revision, "expected_reminder_version": version})
}

#[test]
fn an_item_without_a_reminder_reads_as_none_and_a_time_edit_creates_one() {
    let _guard = serial();
    let store = Store::action("create");
    let mut session = Session::over(&store.path);

    let before = session.read(&get(&["personal"]));
    assert_eq!(before["item_revision"], 0);
    assert_eq!(before["item_type"], "action");
    assert_eq!(before["lifecycle_state"], "active");
    assert_eq!(before["reminder"], Value::Null);

    let created = session.write(&edit("c-1", 0, 0, FUTURE));
    assert_eq!(created["outcome"], "corrected");
    assert_eq!(created["replayed"], false);
    assert_eq!(created["reminder"]["resolved_instant"], FUTURE);
    assert_eq!(created["reminder"]["timezone_id"], "Europe/Dublin");

    let after = session.read(&get(&["personal"]));
    let reminder = &after["reminder"];
    assert_eq!(reminder["resolved_instant"], FUTURE);
    assert_eq!(reminder["request_state"], "resolved");
    assert_eq!(reminder["delivery_state"], "unknown");
    assert_eq!(reminder["acknowledgment_state"], "not_acknowledged");
    assert_eq!(reminder["unschedulable_reason"], Value::Null);
    assert!(reminder["state_version"].as_i64().unwrap() >= 1);
    assert_eq!(store.count("reminders"), 1);
}

#[test]
fn an_edit_replaces_the_instant_exactly_and_needs_the_current_versions() {
    let _guard = serial();
    let store = Store::action("edit");
    let mut session = Session::over(&store.path);
    let created = session.write(&edit("c-1", 0, 0, FUTURE));
    let version = created["reminder"]["state_version"].as_i64().unwrap();

    let stale_version = session.write(&edit("c-2", 0, version - 1, LATER));
    assert_eq!(stale_version["outcome"], "stale_reminder_version");
    assert_eq!(stale_version["current_reminder_version"], version);
    let stale_revision = session.write(&edit("c-3", 5, version, LATER));
    assert_eq!(stale_revision["outcome"], "stale_revision");
    assert_eq!(stale_revision["current_revision"], 0);
    assert_eq!(store.instant().as_deref(), Some(FUTURE));

    let edited = session.write(&edit("c-4", 0, version, LATER));
    assert_eq!(edited["outcome"], "corrected");
    assert_eq!(edited["reminder"]["resolved_instant"], LATER);
    assert!(edited["reminder"]["schedule_generation"].as_i64().unwrap() > 1);
    assert_eq!(store.instant().as_deref(), Some(LATER));
}

#[test]
fn a_time_that_is_not_in_the_future_is_refused_and_nothing_is_stored() {
    let _guard = serial();
    let store = Store::action("past");
    let mut session = Session::over(&store.path);

    let refused = session.write(&edit("c-1", 0, 0, "2020-01-01T09:00:00Z"));
    assert_eq!(
        refused,
        json!({"operation_id": 1, "outcome": "time_in_past"})
    );
    assert_eq!(store.count("reminders"), 0);
    assert_eq!(store.count("reminder_commands"), 0);

    let created = session.write(&edit("c-2", 0, 0, FUTURE));
    let version = created["reminder"]["state_version"].as_i64().unwrap();
    let refused = session.write(&edit("c-3", 0, version, "2020-01-01T09:00:00Z"));
    assert_eq!(refused["outcome"], "time_in_past");
    assert_eq!(store.instant().as_deref(), Some(FUTURE));
}

#[test]
fn a_replayed_command_answers_its_first_result_and_a_reused_id_conflicts() {
    let _guard = serial();
    let store = Store::action("replay");
    let mut session = Session::over(&store.path);
    let first = session.write(&edit("c-1", 0, 0, FUTURE));
    let version = first["reminder"]["state_version"].as_i64().unwrap();

    let replay = session.write(&edit("c-1", 0, 0, FUTURE));
    assert_eq!(replay["outcome"], "corrected");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["reminder"]["resolved_instant"], FUTURE);
    assert_eq!(
        session.read(&get(&["personal"]))["reminder"]["state_version"],
        version
    );

    let conflict = session.write(&edit("c-1", 0, 0, LATER));
    assert_eq!(conflict["outcome"], "command_conflict");
    assert_eq!(store.instant().as_deref(), Some(FUTURE));
}

#[test]
fn cancel_keeps_the_item_and_a_missing_reminder_has_nothing_to_cancel() {
    let _guard = serial();
    let store = Store::action("cancel");
    let mut session = Session::over(&store.path);

    let nothing = session.write(&cancel("c-0", 0, 0));
    assert_eq!(nothing["outcome"], "no_reminder");

    let created = session.write(&edit("c-1", 0, 0, FUTURE));
    let version = created["reminder"]["state_version"].as_i64().unwrap();
    let stale = session.write(&cancel("c-2", 0, version + 3));
    assert_eq!(stale["outcome"], "stale_reminder_version");

    let cancelled = session.write(&cancel("c-3", 0, version));
    assert_eq!(cancelled["outcome"], "cancelled");
    assert_eq!(cancelled["reminder"]["request_state"], "cancelled");
    let read = session.read(&get(&["personal"]));
    assert_eq!(read["reminder"]["request_state"], "cancelled");
    assert_eq!(read["lifecycle_state"], "active");
}

#[test]
fn items_that_cannot_carry_a_reminder_are_refused_without_writing() {
    let _guard = serial();
    let note = Store::new("note", "note", "active", "personal");
    let mut session = Session::over(&note.path);
    let refused = session.write(&edit("c-1", 0, 0, FUTURE));
    assert_eq!(refused["outcome"], "item_cannot_carry_reminder");
    assert_eq!(note.count("reminders"), 0);
    drop(session);

    let completed = Store::new("completed", "action", "completed", "personal");
    let mut session = Session::over(&completed.path);
    let refused = session.write(&edit("c-1", 0, 0, FUTURE));
    assert_eq!(refused["outcome"], "item_not_active");
    assert_eq!(refused["lifecycle_state"], "completed");
    assert_eq!(completed.count("reminders"), 0);
    drop(session);

    let deleted = Store::new("deleted", "action", "deleted", "personal");
    let mut session = Session::over(&deleted.path);
    let refused = session.write(&edit("c-1", 0, 0, FUTURE));
    assert_eq!(refused["outcome"], "item_deleted");
    assert_eq!(deleted.count("reminders"), 0);
}

#[test]
fn reads_stay_inside_the_granted_scopes_and_hide_deleted_items() {
    let _guard = serial();
    let store = Store::action("scopes");
    let mut session = Session::over(&store.path);
    session.write(&edit("c-1", 0, 0, FUTURE));

    for request in [get(&["work"]), history(&["work"])] {
        let event = session.event(true, &request);
        assert_ne!(event.status, OHAND_CORE_STATUS_OK);
        assert!(String::from_utf8_lossy(&event.payload).contains("not_found"));
    }
    assert_eq!(
        session.read(&get(&["work", "personal"]))["reminder"]["resolved_instant"],
        FUTURE
    );
    drop(session);

    let deleted = Store::new("scopes-deleted", "action", "deleted", "personal");
    let mut session = Session::over(&deleted.path);
    let event = session.event(true, &get(&["personal"]));
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    assert!(String::from_utf8_lossy(&event.payload).contains("not_found"));
}

#[test]
fn history_lists_newest_first_and_is_bounded() {
    let _guard = serial();
    let store = Store::action("history");
    let mut session = Session::over(&store.path);

    let none = session.read(&history(&["personal"]));
    assert_eq!(none["reminder_id"], Value::Null);
    assert_eq!(none["entries"], json!([]));
    assert_eq!(none["truncated"], false);

    let mut version = 0;
    for step in 0..(OHAND_CORE_MAX_REMINDER_HISTORY_ENTRIES + 10) {
        let instant = format!("2090-03-{:02}T10:{:02}:00Z", 1 + step / 40, step % 40);
        let applied = session.write(&edit(&format!("c-{step}"), 0, version, &instant));
        assert_eq!(applied["outcome"], "corrected", "step {step}");
        version = applied["reminder"]["state_version"].as_i64().unwrap();
    }

    let bounded = session.read(&history(&["personal"]));
    let entries = bounded["entries"].as_array().unwrap();
    assert_eq!(entries.len(), OHAND_CORE_MAX_REMINDER_HISTORY_ENTRIES);
    assert_eq!(bounded["truncated"], true);
    assert!(bounded["total_entries"].as_u64().unwrap() > entries.len() as u64);
    assert_eq!(bounded["summary"]["request_state"], "resolved");
    assert_eq!(bounded["summary"]["due"], "upcoming");
    let times: Vec<&str> = entries
        .iter()
        .map(|entry| entry["occurred_at"].as_str().unwrap())
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "{times:?}");
}

#[test]
fn an_unknown_item_fails_with_a_fixed_code_that_does_not_echo_the_request() {
    let _guard = serial();
    let store = Store::action("unknown");
    let mut session = Session::over(&store.path);
    let mut request = edit("secret-command", 0, 0, FUTURE);
    request["item_id"] = json!("secret-item");
    let event = session.event(false, &request);
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let payload = String::from_utf8_lossy(&event.payload).into_owned();
    assert!(payload.contains("not_found"), "{payload}");
    for secret in ["secret-command", "secret-item"] {
        assert!(!payload.contains(secret), "failure echoed {secret}");
    }
    assert_eq!(store.count("reminder_commands"), 0);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let store = Store::action("malformed");
    let mut session = Session::over(&store.path);

    session.refuse(true, &get(&[]));
    session.refuse(true, &get(&["family"]));
    session.refuse(
        true,
        &json!({"command": "get_reminder", "item_id": ITEM_ID}),
    );
    session.refuse(
        true,
        &json!({"command": "get_reminder", "item_id": "", "read_scopes": ["personal"]}),
    );
    session.refuse(
        true,
        &json!({"command": "get_reminder", "item_id": ITEM_ID, "read_scopes": ["personal"],
                "extra": 1}),
    );
    session.refuse(false, &edit("", 0, 0, FUTURE));
    session.refuse(false, &edit("c-1", -1, 0, FUTURE));
    session.refuse(false, &edit("c-1", 0, -1, FUTURE));
    session.refuse(false, &edit("c-1", 0, 0, "tomorrow"));
    session.refuse(false, &edit("c-1", 0, 0, "2090-01-01T09:00:00.500Z"));
    let mut bad_zone = edit("c-1", 0, 0, FUTURE);
    bad_zone["timezone_id"] = json!("Mars/Olympus_Mons");
    session.refuse(false, &bad_zone);
    session.refuse(false, &json!({"command": "snooze", "command_id": "c-1"}));
    let mut with_instant = cancel("c-1", 0, 0);
    with_instant["instant"] = json!(FUTURE);
    session.refuse(false, &with_instant);
    assert_eq!(store.count("reminder_commands"), 0);
}
