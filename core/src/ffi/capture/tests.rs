//! Drives the capture exports the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    assert_rejected, consume, open_memory, recorder, register, serial, Event, Outcome, Recorder,
    WAIT,
};
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn capture_request(capture_id: &str, text: &str) -> Value {
    json!({
        "capture_id": capture_id,
        "text": text,
        "capture_instant": "2026-10-08T09:30:00Z",
        "timezone_id": "UTC",
        "utc_offset_minutes": 0,
        "locale": "en_US",
        "calendar": "gregorian",
        "item_scope": "personal",
        "route_id": "route-default",
        "entry_locked": false,
        "created_at": "2026-10-08T09:30:01Z",
    })
}

fn start_save(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_save_capture(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn start_get(handle: OhandCoreHandle, operation_id: u64, capture_id: &str) -> Outcome {
    consume(unsafe {
        ohand_core_start_get_capture(handle, operation_id, capture_id.as_ptr(), capture_id.len())
    })
}

fn start_status(handle: OhandCoreHandle, operation_id: u64, item_id: &str) -> Outcome {
    consume(unsafe {
        ohand_core_start_item_status(handle, operation_id, item_id.as_ptr(), item_id.len())
    })
}

fn next_event(events: &Receiver<Event>, operation_id: u64) -> Event {
    let event = events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.operation_id, operation_id);
    assert_ne!(event.thread, std::thread::current().id());
    event
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

fn failure_code(event: &Event) -> (u32, String) {
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let value: Value = serde_json::from_slice(&event.payload).unwrap();
    (event.status, value["code"].as_str().unwrap().to_string())
}

fn open_file(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
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

    fn save(&self, operation_id: u64, request: &Value) -> Event {
        assert_eq!(start_save(self.handle, operation_id, request).status, 0);
        next_event(&self.events, operation_id)
    }

    fn get(&self, operation_id: u64, capture_id: &str) -> Event {
        assert_eq!(start_get(self.handle, operation_id, capture_id).status, 0);
        next_event(&self.events, operation_id)
    }

    fn status(&self, operation_id: u64, item_id: &str) -> Event {
        assert_eq!(start_status(self.handle, operation_id, item_id).status, 0);
        next_event(&self.events, operation_id)
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn temporary_store(name: &str) -> (std::path::PathBuf, String) {
    let directory = std::env::temp_dir().join(format!("ohand-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    (directory, path)
}

#[test]
fn a_saved_capture_is_read_back_with_independent_initial_statuses() {
    let _guard = serial();
    let session = Session::over(open_memory());

    let ack = success_json(&session.save(1, &capture_request("capture-1", "buy oat milk")));
    assert_eq!(ack["operation_id"], 1);
    assert_eq!(ack["already_saved"], false);
    assert_eq!(ack["item_id"], "capture-1");
    assert_eq!(ack["capture"]["text"], "buy oat milk");

    let read = success_json(&session.get(2, "capture-1"));
    assert_eq!(read["capture"], ack["capture"]);

    let status = success_json(&session.status(3, "capture-1"));
    assert_eq!(status["save_state"], "saved_local");
    assert_eq!(status["sync_state"], "not_configured");
    assert_eq!(status["processing_state"], "unprocessed");
    assert_eq!(status["transcription_state"], "not_applicable");
    assert_eq!(status["processing_job_status"], Value::Null);
    assert_eq!(status["reminder_request_state"], Value::Null);
    assert_eq!(status["reminder_schedule_state"], Value::Null);
    session.close();
}

#[test]
fn an_audio_only_capture_starts_with_audio_pending_transcription() {
    let _guard = serial();
    let session = Session::over(open_memory());
    let mut request = capture_request("capture-audio", "");
    request["text"] = Value::Null;
    request["audio_reference"] = json!("staging/capture-audio.m4a");
    let ack = success_json(&session.save(1, &request));
    assert_eq!(
        ack["capture"]["audio_reference"],
        "staging/capture-audio.m4a"
    );
    let status = success_json(&session.status(2, "capture-audio"));
    assert_eq!(status["transcription_state"], "audio_pending");
    assert_eq!(status["save_state"], "saved_local");
    session.close();
}

#[test]
fn identical_resaves_converge_and_conflicting_reuse_changes_nothing() {
    let _guard = serial();
    let session = Session::over(open_memory());
    let request = capture_request("capture-dup", "original words");
    assert_eq!(
        success_json(&session.save(1, &request))["already_saved"],
        false
    );
    assert_eq!(
        success_json(&session.save(2, &request))["already_saved"],
        true
    );

    let changed_text = capture_request("capture-dup", "different words");
    assert_eq!(
        failure_code(&session.save(3, &changed_text)),
        (OHAND_CORE_STATUS_PERMANENT, "capture_conflict".to_string())
    );
    let mut changed_timestamp = request.clone();
    changed_timestamp["created_at"] = json!("2026-10-08T09:31:00Z");
    assert_eq!(
        failure_code(&session.save(4, &changed_timestamp)).1,
        "capture_conflict"
    );

    let read = success_json(&session.get(5, "capture-dup"));
    assert_eq!(read["capture"]["text"], "original words");
    assert_eq!(read["capture"]["created_at"], "2026-10-08T09:30:01Z");
    session.close();
}

#[test]
fn captures_and_statuses_survive_closing_and_reopening_the_store() {
    let _guard = serial();
    let (directory, path) = temporary_store("restart");

    let first = Session::over(open_file(&path));
    let request = capture_request("capture-restart", "still here after restart");
    let ack = success_json(&first.save(1, &request));
    first.close();

    let second = Session::over(open_file(&path));
    assert_eq!(
        success_json(&second.get(2, "capture-restart"))["capture"],
        ack["capture"]
    );
    assert_eq!(
        success_json(&second.status(3, "capture-restart"))["save_state"],
        "saved_local"
    );
    let resave = success_json(&second.save(4, &request));
    assert_eq!(resave["already_saved"], true);
    assert_eq!(resave["item_id"], "capture-restart");
    second.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn processing_and_reminder_scheduling_statuses_are_read_independently() {
    let _guard = serial();
    let (directory, path) = temporary_store("statuses");
    let first = Session::over(open_file(&path));
    success_json(&first.save(
        1,
        &capture_request("capture-status", "call the dentist tomorrow"),
    ));
    first.close();

    {
        let mut database = Database::open(
            &path,
            std::sync::Arc::new(crate::store::schema::SystemClock),
        )
        .unwrap();
        let tx = database.transaction().unwrap();
        tx.execute(
            "UPDATE items SET processing_state = 'uninterpreted' WHERE item_id = 'capture-status'",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, \
             delivery_state, acknowledgment_state, unschedulable_reason, created_at, updated_at) \
             VALUES ('reminder-1', 'capture-status', 'unschedulable', 'not_scheduled', 'unknown', \
             'not_acknowledged', 'permission_denied', '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let second = Session::over(open_file(&path));
    let status = success_json(&second.status(2, "capture-status"));
    assert_eq!(status["save_state"], "saved_local");
    assert_eq!(status["processing_state"], "uninterpreted");
    assert_eq!(status["reminder_request_state"], "unschedulable");
    assert_eq!(status["reminder_schedule_state"], "not_scheduled");
    assert_eq!(status["reminder_delivery_state"], "unknown");
    assert_eq!(status["reminder_acknowledgment_state"], "not_acknowledged");
    assert_eq!(status["unschedulable_reason"], "permission_denied");
    second.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_save_that_fails_after_the_capture_insert_acknowledges_nothing_and_leaves_nothing() {
    let _guard = serial();
    let (directory, path) = temporary_store("rollback");
    let first = Session::over(open_file(&path));
    success_json(&first.save(1, &capture_request("capture-other", "an unrelated capture")));
    first.close();

    {
        let mut database = Database::open(
            &path,
            std::sync::Arc::new(crate::store::schema::SystemClock),
        )
        .unwrap();
        let tx = database.transaction().unwrap();
        // An item that already owns the ID the new capture's item would take.
        tx.execute(
            "UPDATE items SET item_id = 'capture-blocked' WHERE item_id = 'capture-other'",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let second = Session::over(open_file(&path));
    let failed = second.save(
        2,
        &capture_request("capture-blocked", "must not be acknowledged"),
    );
    assert_eq!(
        failure_code(&failed),
        (OHAND_CORE_STATUS_PERMANENT, "storage_error".to_string())
    );
    assert_eq!(
        failure_code(&second.get(3, "capture-blocked")).1,
        "not_found"
    );
    assert_eq!(
        success_json(&second.get(4, "capture-other"))["capture"]["text"],
        "an unrelated capture"
    );
    second.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn unknown_records_are_not_found_and_failures_never_echo_the_request() {
    let _guard = serial();
    let session = Session::over(open_memory());
    let capture = session.get(1, "synthetic-marker-capture-9");
    assert_eq!(
        failure_code(&capture),
        (OHAND_CORE_STATUS_PERMANENT, "not_found".to_string())
    );
    assert!(!String::from_utf8_lossy(&capture.payload).contains("synthetic-marker"));
    assert_eq!(
        failure_code(&session.status(2, "synthetic-marker-item-9")).1,
        "not_found"
    );
    session.close();
}

#[test]
fn malformed_requests_are_refused_synchronously_and_queue_nothing() {
    let _guard = serial();
    let session = Session::over(open_memory());
    let handle = session.handle;

    let mut without_content = capture_request("capture-x", "");
    without_content["text"] = Value::Null;
    let mut empty_text = capture_request("capture-x", "");
    empty_text["text"] = json!("");
    let mut unknown_scope = capture_request("capture-x", "words");
    unknown_scope["item_scope"] = json!("shared");
    let mut empty_route = capture_request("capture-x", "words");
    empty_route["route_id"] = json!("");
    let mut extra_field = capture_request("capture-x", "words");
    extra_field["confidence"] = json!(0.5);
    let mut missing_field = capture_request("capture-x", "words");
    missing_field.as_object_mut().unwrap().remove("timezone_id");

    for (request, code) in [
        (&without_content, "invalid_capture"),
        (&empty_text, "invalid_capture"),
        (&unknown_scope, "invalid_capture"),
        (&empty_route, "invalid_capture"),
        (&extra_field, "invalid_request"),
        (&missing_field, "invalid_request"),
    ] {
        assert_rejected(
            &start_save(handle, 1, request),
            OHAND_CORE_STATUS_PERMANENT,
            code,
        );
    }

    for bytes in [&b"not json"[..], b"[]", b"{", &[0xff, 0xfe][..]] {
        let outcome = consume(unsafe {
            ohand_core_start_save_capture(handle, 2, bytes.as_ptr(), bytes.len())
        });
        assert_rejected(&outcome, OHAND_CORE_STATUS_PERMANENT, "invalid_request");
    }
    assert_rejected(
        &consume(unsafe { ohand_core_start_save_capture(handle, 3, std::ptr::null(), 0) }),
        OHAND_CORE_STATUS_PERMANENT,
        "invalid_request",
    );
    assert_rejected(
        &consume(unsafe { ohand_core_start_save_capture(handle, 4, std::ptr::null(), 9) }),
        OHAND_CORE_STATUS_PERMANENT,
        "null_argument",
    );
    // Refused from the length alone; this pointer is never read.
    let bogus = std::ptr::NonNull::<u8>::dangling().as_ptr();
    assert_rejected(
        &consume(unsafe {
            ohand_core_start_save_capture(
                handle,
                5,
                bogus,
                OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES + 1,
            )
        }),
        OHAND_CORE_STATUS_PERMANENT,
        "request_too_large",
    );
    assert_rejected(
        &consume(unsafe { ohand_core_start_get_capture(handle, 6, [0xff].as_ptr(), 1) }),
        OHAND_CORE_STATUS_PERMANENT,
        "invalid_utf8",
    );
    assert_rejected(
        &consume(unsafe { ohand_core_start_item_status(handle, 7, std::ptr::null(), 0) }),
        OHAND_CORE_STATUS_PERMANENT,
        "invalid_request",
    );

    assert!(session
        .events
        .recv_timeout(Duration::from_millis(200))
        .is_err());
    assert_eq!(failure_code(&session.get(8, "capture-x")).1, "not_found");
    session.close();
}

#[test]
fn invalid_cancelled_and_closed_handles_are_refused_for_every_capture_export() {
    let _guard = serial();
    let closed = open_memory();
    consume(ohand_core_close(closed));
    let request = serde_json::to_vec(&capture_request("capture-h", "words")).unwrap();
    for handle in [closed, 0, u64::MAX] {
        for outcome in [
            consume(unsafe {
                ohand_core_start_save_capture(handle, 1, request.as_ptr(), request.len())
            }),
            start_get(handle, 2, "capture-h"),
            start_status(handle, 3, "capture-h"),
        ] {
            assert_rejected(&outcome, OHAND_CORE_STATUS_PERMANENT, "invalid_handle");
        }
    }

    let cancelled = open_memory();
    consume(ohand_core_cancel(cancelled));
    assert_rejected(
        &consume(unsafe {
            ohand_core_start_save_capture(cancelled, 4, request.as_ptr(), request.len())
        }),
        OHAND_CORE_STATUS_CANCELLED,
        "cancelled",
    );
    assert_rejected(
        &start_get(cancelled, 5, "capture-h"),
        OHAND_CORE_STATUS_CANCELLED,
        "cancelled",
    );
    consume(ohand_core_close(cancelled));
}

#[test]
fn text_with_interior_nul_and_unicode_round_trips_unchanged() {
    let _guard = serial();
    let session = Session::over(open_memory());
    let words = "caf\u{e9} \u{1f469}\u{200d}\u{1f4bb} \u{5e9}\u{5dc}\u{5d5}\u{5dd}\0tail";
    success_json(&session.save(1, &capture_request("capture-unicode", words)));
    assert_eq!(
        success_json(&session.get(2, "capture-unicode"))["capture"]["text"],
        words
    );
    session.close();
}

#[test]
fn capture_events_do_not_leak_buffers_or_handles() {
    let _guard = serial();
    let handles_before = ohand_core_live_handles();
    let buffers_before = ohand_core_live_result_buffers();
    let session = Session::over(open_memory());
    success_json(&session.save(1, &capture_request("capture-leak", "words")));
    let _ = session.get(2, "missing");
    let _ = start_status(0, 3, "capture-leak");
    session.close();
    assert_eq!(ohand_core_live_handles(), handles_before);
    assert_eq!(ohand_core_live_result_buffers(), buffers_before);
}
