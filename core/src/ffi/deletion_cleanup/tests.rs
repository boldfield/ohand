//! Drives the deletion-cleanup export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::lifecycle::delete_intent::{deletion_progress, mark_deletion_intent, DeletionProgress};
use crate::store::captures::{save_capture, Capture};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ITEM_ID: &str = "item-1";
const AUDIO_REFERENCE: &str = "FinalizedAudio/capture-1.m4a";

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_deletion_cleanup(handle, operation_id, bytes.as_ptr(), bytes.len())
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

/// A store holding one deleted item with audio and a reminder that went through two generations:
/// the first generation's cancel was already acknowledged, the second generation's is pending.
fn deleted_item_store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-deletion-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = open_database(&path);
    let capture = Capture::new(
        "capture-1".to_string(),
        None,
        Some(AUDIO_REFERENCE.to_string()),
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
    let tx = database.transaction().unwrap();
    tx.execute(
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state,
                            sync_state, processing_state, transcription_state, created_at, updated_at)
         VALUES (?, 'capture-1', 0, 'active', 'saved', 'not_configured', 'pending', 'pending',
                 '2026-10-10T09:30:01Z', '2026-10-10T09:30:01Z')",
        [ITEM_ID],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state,
                                acknowledgment_state, resolved_instant, timezone_id,
                                schedule_generation, created_at, updated_at)
         VALUES ('rem-1', ?, 'resolved', 'scheduled', 'unknown', 'not_acknowledged',
                 '2026-10-12T09:00:00Z', 'UTC', 2, '2026-10-10T09:31:00Z', '2026-10-10T09:32:00Z')",
        [ITEM_ID],
    )
    .unwrap();
    for (operation_id, operation_type, notification_id, created_at) in [
        ("op-1", "schedule", "rem-1#1", "2026-10-10T09:31:00Z"),
        ("op-2", "cancel", "rem-1#1", "2026-10-10T09:32:00Z"),
        ("op-3", "schedule", "rem-1#2", "2026-10-10T09:32:00Z"),
    ] {
        tx.execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type,
                    operation_state, effect_identity, external_id, created_at)
             VALUES (?, 'rem-1', ?, 'acknowledged', ?, ?, ?)",
            rusqlite::params![
                operation_id,
                operation_type,
                format!("{operation_type}:{notification_id}"),
                notification_id,
                created_at
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    mark_deletion_intent(&mut database, ITEM_ID, 0, Utc::now()).unwrap();
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

fn work_ids(body: &Value) -> std::collections::BTreeMap<String, String> {
    body["work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["work_type"].as_str().unwrap().to_string(),
                row["work_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn the_whole_queue_is_readable_and_completable_through_the_export() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("cycle");
    let mut session = Session::over(open_file(&path));

    let items = session.succeed(&json!({"command": "pending_items"}));
    assert_eq!(items["item_ids"], json!([ITEM_ID]));

    let pending = session.succeed(&json!({"command": "pending_work", "item_id": ITEM_ID}));
    let work = work_ids(&pending);
    assert_eq!(
        work.keys().cloned().collect::<Vec<_>>(),
        ["cancel_notifications", "clear_ingress", "remove_audio"]
    );
    assert!(pending["work"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["item_id"] == ITEM_ID));

    let target = session.succeed(&json!({"command": "cleanup_target", "item_id": ITEM_ID}));
    assert_eq!(target["target"]["capture_id"], "capture-1");
    assert_eq!(target["target"]["audio_reference"], AUDIO_REFERENCE);

    let reminder =
        session.succeed(&json!({"command": "reminder_notifications", "item_id": ITEM_ID}));
    let mut identifiers: Vec<String> = reminder["notification_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect();
    identifiers.sort();
    assert_eq!(identifiers, ["rem-1#1", "rem-1#2"]);

    let (status, code) =
        session.fail(&json!({"command": "complete_work", "work_id": work["cancel_notifications"]}));
    assert_eq!((status, code.as_str()), (2, "deletion_work_refused"));

    let unrecorded = session.succeed(
        &json!({"command": "record_notification_cancelled", "notification_id": "rem-1#9"}),
    );
    assert_eq!(unrecorded["recorded"], false);
    let unknown = session.succeed(
        &json!({"command": "record_notification_cancelled", "notification_id": "other#1"}),
    );
    assert_eq!(unknown["recorded"], false);
    let earlier_generation = session.succeed(
        &json!({"command": "record_notification_cancelled", "notification_id": "rem-1#1"}),
    );
    assert_eq!(earlier_generation["recorded"], true);
    let recorded = session.succeed(
        &json!({"command": "record_notification_cancelled", "notification_id": "rem-1#2"}),
    );
    assert_eq!(recorded["recorded"], true);
    let repeated = session.succeed(
        &json!({"command": "record_notification_cancelled", "notification_id": "rem-1#2"}),
    );
    assert_eq!(repeated["recorded"], true);

    for work_type in ["cancel_notifications", "clear_ingress", "remove_audio"] {
        session.succeed(&json!({"command": "complete_work", "work_id": work[work_type]}));
    }
    session.succeed(&json!({"command": "complete_work", "work_id": work["remove_audio"]}));

    let released = session.succeed(&json!({"command": "cleanup_target", "item_id": ITEM_ID}));
    assert_eq!(released["target"]["audio_reference"], Value::Null);
    assert_eq!(
        session.succeed(&json!({"command": "pending_items"}))["item_ids"],
        json!([])
    );
    let (status, code) =
        session.fail(&json!({"command": "fail_work", "work_id": work["remove_audio"]}));
    assert_eq!((status, code.as_str()), (2, "deletion_work_refused"));
    session.close();

    let mut database = open_database(&path);
    assert_eq!(
        deletion_progress(&mut database, ITEM_ID).unwrap(),
        DeletionProgress::Complete
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_terminal_failure_is_recorded_leaves_the_queue_and_stays_listed() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("failed");
    let mut session = Session::over(open_file(&path));
    let work = work_ids(&session.succeed(&json!({"command": "pending_work", "item_id": ITEM_ID})));

    assert_eq!(
        session.succeed(&json!({"command": "failed_work"}))["work"],
        json!([])
    );
    session.succeed(&json!({"command": "fail_work", "work_id": work["remove_audio"]}));
    session.succeed(&json!({"command": "fail_work", "work_id": work["remove_audio"]}));
    let failed = session.succeed(&json!({"command": "failed_work"}));
    assert_eq!(
        failed["work"],
        json!([{
            "work_id": work["remove_audio"],
            "item_id": ITEM_ID,
            "work_type": "remove_audio",
        }]),
        "a terminal failure stays readable after it leaves the pending queue"
    );
    let remaining =
        work_ids(&session.succeed(&json!({"command": "pending_work", "item_id": ITEM_ID})));
    assert_eq!(
        remaining.keys().cloned().collect::<Vec<_>>(),
        ["cancel_notifications", "clear_ingress"]
    );
    let (status, code) =
        session.fail(&json!({"command": "complete_work", "work_id": work["remove_audio"]}));
    assert_eq!((status, code.as_str()), (2, "deletion_work_refused"));
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn an_unknown_work_row_is_refused_without_changing_anything() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("unknown");
    let mut session = Session::over(open_file(&path));
    let (status, code) =
        session.fail(&json!({"command": "complete_work", "work_id": "no-such-work"}));
    assert_eq!((status, code.as_str()), (2, "deletion_work_refused"));
    assert_eq!(
        session.succeed(&json!({"command": "pending_items"}))["item_ids"],
        json!([ITEM_ID])
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn an_item_without_deletion_work_or_reminder_reads_as_empty() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("empty");
    let mut session = Session::over(open_file(&path));
    let work = session.succeed(&json!({"command": "pending_work", "item_id": "other-item"}));
    assert_eq!(work["work"], json!([]));
    let target = session.succeed(&json!({"command": "cleanup_target", "item_id": "other-item"}));
    assert_eq!(target["target"], Value::Null);
    let reminder =
        session.succeed(&json!({"command": "reminder_notifications", "item_id": "other-item"}));
    assert_eq!(reminder["notification_ids"], json!([]));
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("invalid");
    let handle = open_file(&path);

    for request in [
        json!({"command": "unknown_command"}),
        json!({"command": "pending_work"}),
        json!({"command": "pending_work", "item_id": ""}),
        json!({"command": "pending_items", "extra": 1}),
        json!({"command": "failed_work", "item_id": "item-1"}),
        json!({"command": "record_notification_cancelled", "notification_id": "x".repeat(MAX_IDENTIFIER_BYTES + 1)}),
        json!([]),
    ] {
        let outcome = start(handle, 1, &request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }
    let empty = consume(unsafe { ohand_core_start_deletion_cleanup(handle, 2, [].as_ptr(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    let null =
        consume(unsafe { ohand_core_start_deletion_cleanup(handle, 3, std::ptr::null(), 4) });
    assert_eq!(null.code(), "null_argument");

    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_malformed_notification_identifier_is_invalid_not_recorded() {
    let _guard = serial();
    let (directory, path) = deleted_item_store("identifier");
    let mut session = Session::over(open_file(&path));
    for notification_id in ["no-separator", "#3"] {
        let (status, code) = session.fail(
            &json!({"command": "record_notification_cancelled", "notification_id": notification_id}),
        );
        assert_eq!((status, code.as_str()), (2, "invalid_request"));
    }
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}
