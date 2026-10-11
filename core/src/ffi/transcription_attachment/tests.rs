//! Drives the transcription exports the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::jobs::queue::{claim_job_with_lease, enqueue_job};
use chrono::Duration;
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const ROUTE_ID: &str = "route-personal";
const CAPTURE_ID: &str = "3f9c1e52-0000-4000-8000-0000000000c6";
const AUDIO_REFERENCE: &str = "staging/2026-10-08/clip-6.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const JOB_ID: &str = "3f9c1e52-0000-4000-8000-0000000000d6";
const TRANSCRIPT: &str = "remind me to water the ferns tomorrow";

fn open_file(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
}

fn database_at(path: &str) -> Database {
    Database::open(path, Arc::new(crate::store::schema::SystemClock)).unwrap()
}

/// A store with one imported voice capture and one claimed transcription job.
fn claimed_store(name: &str) -> (std::path::PathBuf, String, i32) {
    let directory = std::env::temp_dir().join(format!(
        "ohand-transcription-ffi-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = database_at(&path);
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES (?, ?, 'personal', '[]', '2026-10-08T09:00:00Z')",
            [ROUTE_ID, ROUTE_ID],
        )
        .unwrap();
    let capture = crate::store::captures::Capture::new(
        CAPTURE_ID.to_string(),
        None,
        Some(AUDIO_REFERENCE.to_string()),
        "2026-10-08T09:30:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        ROUTE_ID.to_string(),
        false,
        "2026-10-08T09:30:01Z".to_string(),
        None,
    )
    .unwrap();
    let item_id = crate::ingress::import_foreground_ingress(&mut database, &capture)
        .unwrap()
        .item_id;
    let revision: i32 = database
        .conn()
        .query_row(
            "SELECT revision FROM items WHERE item_id = ?",
            [&item_id],
            |row| row.get(0),
        )
        .unwrap();
    let now = Utc::now();
    enqueue_job(
        &mut database,
        JOB_ID.to_string(),
        item_id,
        JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
        revision,
        None,
        None,
        1,
        now,
    )
    .unwrap();
    let claimed = claim_job_with_lease(&mut database, Duration::seconds(300), now)
        .unwrap()
        .expect("the job is eligible");
    (directory, path, claimed.attempt_count)
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

    fn run(
        &self,
        operation_id: u64,
        request: &[u8],
        start: unsafe extern "C" fn(OhandCoreHandle, u64, *const u8, usize) -> OhandCoreResult,
    ) -> Event {
        let outcome: Outcome =
            consume(unsafe { start(self.handle, operation_id, request.as_ptr(), request.len()) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn read(&self, operation_id: u64, request: &Value) -> Event {
        self.run(
            operation_id,
            &serde_json::to_vec(request).unwrap(),
            ohand_core_start_read_transcription_job,
        )
    }

    fn apply(&self, operation_id: u64, request: &Value) -> Event {
        self.run(
            operation_id,
            &serde_json::to_vec(request).unwrap(),
            ohand_core_start_apply_transcription_outcome,
        )
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

fn failure_code(event: &Event) -> String {
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let value: Value = serde_json::from_slice(&event.payload).unwrap();
    value["code"].as_str().unwrap().to_string()
}

fn transcript_request(attempt: i32, text: &str) -> Value {
    json!({
        "job_id": JOB_ID,
        "lease_attempt": attempt,
        "audio_reference": AUDIO_REFERENCE,
        "result": "transcript",
        "transcript": {
            "text": text,
            "confidence": 0.9,
            "detected_language": "en-US",
            "recognizer": "sfspeech-ondevice-1",
            "audio_sha256": AUDIO_SHA256,
        },
    })
}

fn outcome_request(attempt: i32, result: &str) -> Value {
    json!({
        "job_id": JOB_ID,
        "lease_attempt": attempt,
        "audio_reference": AUDIO_REFERENCE,
        "result": result,
    })
}

fn item_state(path: &str) -> (String, Option<String>) {
    database_at(path)
        .conn()
        .query_row(
            "SELECT i.transcription_state, c.audio_reference
               FROM items i JOIN captures c ON c.capture_id = i.capture_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn job_row(path: &str) -> (String, Option<String>) {
    database_at(path)
        .conn()
        .query_row(
            "SELECT status, failure_reason FROM jobs WHERE job_id = ?",
            [JOB_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn reading_a_running_job_returns_the_audio_reference_and_locale_without_writing() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("read");
    let session = Session::over(open_file(&path));

    let source =
        success_json(&session.read(1, &json!({"job_id": JOB_ID, "lease_attempt": attempt})));
    assert_eq!(source["operation_id"], 1);
    assert_eq!(source["job_id"], JOB_ID);
    assert_eq!(source["capture_id"], CAPTURE_ID);
    assert_eq!(source["audio_reference"], AUDIO_REFERENCE);
    assert_eq!(source["locale"], "en_US");
    assert_eq!(source["lifecycle_state"], "active");
    assert_eq!(source["transcription_state"], "audio_pending");
    assert_eq!(job_row(&path).0, "running");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn reading_refuses_unknown_stale_and_foreign_jobs() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("read-refused");
    let session = Session::over(open_file(&path));

    let stale = session.read(1, &json!({"job_id": JOB_ID, "lease_attempt": attempt + 1}));
    assert_eq!(failure_code(&stale), "transcription_lease_not_held");
    let unknown = session.read(2, &json!({"job_id": "missing", "lease_attempt": 1}));
    assert_eq!(failure_code(&unknown), "transcription_lease_not_held");

    database_at(&path)
        .conn()
        .execute(
            "UPDATE jobs SET job_type = 'interpretation' WHERE job_id = ?",
            [JOB_ID],
        )
        .unwrap();
    let foreign = session.read(3, &json!({"job_id": JOB_ID, "lease_attempt": attempt}));
    assert_eq!(failure_code(&foreign), "transcription_wrong_job_type");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_transcript_is_attached_once_and_a_redelivery_converges() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("attach");
    let session = Session::over(open_file(&path));

    let request = transcript_request(attempt, TRANSCRIPT);
    let first = success_json(&session.apply(1, &request));
    assert_eq!(first["operation_id"], 1);
    assert_eq!(first["disposition"], "attached");
    assert_eq!(first["interpretation"]["status"], "not_requested");
    let revision = first["revision"].clone();
    assert_eq!(item_state(&path).0, "transcribed");
    assert_eq!(job_row(&path).0, "completed");

    let second = success_json(&session.apply(2, &request));
    assert_eq!(second["disposition"], "already_attached");
    assert_eq!(second["revision"], revision);

    let different = session.apply(3, &transcript_request(attempt, "something else entirely"));
    assert_eq!(failure_code(&different), "transcription_conflicting_result");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn unsupported_and_failed_outcomes_keep_the_audio_pending() {
    let _guard = serial();
    for (name, result, state, reason) in [
        (
            "unsupported",
            "unsupported",
            "transcription_unsupported",
            "transcription_unsupported",
        ),
        (
            "failed",
            "failed",
            "transcription_failed",
            "transcription_failed",
        ),
    ] {
        let (directory, path, attempt) = claimed_store(name);
        let session = Session::over(open_file(&path));
        let answer = success_json(&session.apply(1, &outcome_request(attempt, result)));
        assert_eq!(answer["disposition"], "pending");
        assert_eq!(answer["transcription_state"], state);
        let (item, audio) = item_state(&path);
        assert_eq!(item, state);
        assert_eq!(audio.as_deref(), Some(AUDIO_REFERENCE));
        let (status, failure) = job_row(&path);
        assert_eq!(status, "failed");
        assert_eq!(failure.as_deref(), Some(reason));
        session.close();
        let _ = std::fs::remove_dir_all(directory);
    }
}

#[test]
fn a_stale_lease_writes_nothing() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("stale");
    let session = Session::over(open_file(&path));

    let stale = session.apply(1, &transcript_request(attempt + 1, TRANSCRIPT));
    assert_eq!(failure_code(&stale), "transcription_lease_not_held");
    assert_eq!(item_state(&path).0, "audio_pending");
    assert_eq!(job_row(&path).0, "running");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn audio_of_another_identity_is_discarded_and_the_item_is_unchanged() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("identity");
    let session = Session::over(open_file(&path));

    let mut request = transcript_request(attempt, TRANSCRIPT);
    request["audio_reference"] = json!("staging/2026-10-08/other-clip.m4a");
    let answer = success_json(&session.apply(1, &request));
    assert_eq!(answer["disposition"], "discarded");
    assert_eq!(answer["discard_reason"], "audio_identity_mismatch");
    assert_eq!(item_state(&path).0, "audio_pending");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn an_interpretation_request_is_queued_only_when_the_core_authorizes_it() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("interpretation");
    let session = Session::over(open_file(&path));

    let mut request = transcript_request(attempt, TRANSCRIPT);
    request["interpretation"] = json!({
        "job_id": "3f9c1e52-0000-4000-8000-0000000000d7",
        "request_version": "interpretation-v1",
    });
    let answer = success_json(&session.apply(1, &request));
    assert_eq!(answer["disposition"], "attached");
    assert_eq!(answer["interpretation"]["status"], "queued");
    assert_eq!(
        answer["interpretation"]["job_id"],
        "3f9c1e52-0000-4000-8000-0000000000d7"
    );
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_without_a_write() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("malformed");
    let session = Session::over(open_file(&path));

    let mut bad_digest = transcript_request(attempt, TRANSCRIPT);
    bad_digest["transcript"]["audio_sha256"] = json!("not-a-digest");
    assert_eq!(
        failure_code(&session.apply(2, &bad_digest)),
        "transcription_rejected"
    );

    let mut oversized = transcript_request(attempt, TRANSCRIPT);
    oversized["transcript"]["text"] = json!("a".repeat(MAX_TRANSCRIPT_BYTES + 1));
    let refused = consume(unsafe {
        let bytes = serde_json::to_vec(&oversized).unwrap();
        ohand_core_start_apply_transcription_outcome(session.handle, 3, bytes.as_ptr(), bytes.len())
    });
    assert_eq!(refused.status, OHAND_CORE_STATUS_PERMANENT);

    let mut with_text = outcome_request(attempt, "failed");
    with_text["transcript"] = transcript_request(attempt, TRANSCRIPT)["transcript"].clone();
    let refused = consume(unsafe {
        let bytes = serde_json::to_vec(&with_text).unwrap();
        ohand_core_start_apply_transcription_outcome(session.handle, 4, bytes.as_ptr(), bytes.len())
    });
    assert_eq!(refused.status, OHAND_CORE_STATUS_PERMANENT);

    let mut unknown_field = transcript_request(attempt, TRANSCRIPT);
    unknown_field["extra"] = json!(true);
    let refused = consume(unsafe {
        let bytes = serde_json::to_vec(&unknown_field).unwrap();
        ohand_core_start_apply_transcription_outcome(session.handle, 5, bytes.as_ptr(), bytes.len())
    });
    assert_eq!(refused.status, OHAND_CORE_STATUS_PERMANENT);

    assert_eq!(item_state(&path).0, "audio_pending");
    assert_eq!(job_row(&path).0, "running");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn request_bounds_are_checked_before_the_pointer_is_read() {
    let _guard = serial();
    let (directory, path, _) = claimed_store("bounds");
    let handle = open_file(&path);

    let too_large = consume(unsafe {
        ohand_core_start_apply_transcription_outcome(
            handle,
            1,
            std::ptr::null(),
            OHAND_CORE_MAX_TRANSCRIPTION_REQUEST_BYTES + 1,
        )
    });
    assert_eq!(too_large.status, OHAND_CORE_STATUS_PERMANENT);
    let empty =
        consume(unsafe { ohand_core_start_read_transcription_job(handle, 2, std::ptr::null(), 0) });
    assert_eq!(empty.status, OHAND_CORE_STATUS_PERMANENT);
    let null =
        consume(unsafe { ohand_core_start_read_transcription_job(handle, 3, std::ptr::null(), 4) });
    assert_eq!(null.status, OHAND_CORE_STATUS_PERMANENT);
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn failure_messages_never_echo_the_transcript() {
    let _guard = serial();
    let (directory, path, attempt) = claimed_store("echo");
    let session = Session::over(open_file(&path));
    let secret = "the secret dictated sentence";

    let event = session.apply(1, &transcript_request(attempt + 1, secret));
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    assert!(!String::from_utf8_lossy(&event.payload).contains(secret));
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}
