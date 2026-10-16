//! Drives the item-detail export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Outcome, Recorder, WAIT,
};
use crate::ingress::import::import_foreground_ingress;
use crate::ingress::transcription::{
    apply_transcription_outcome, RecognizedTranscript, RecognizerResult, TranscriptionOutcome,
};
use crate::jobs::queue::{claim_job_with_lease, enqueue_job};
use crate::lifecycle::delete_intent::mark_deletion_intent;
use crate::privacy::routing::JOB_TYPE_TRANSCRIPTION_ATTACHMENT;
use crate::store::captures::Capture;
use crate::store::events::{
    save_event, Correction, CorrectionKind, Event, EventPayload, EventType,
};
use crate::store::schema::SystemClock;
use chrono::{DateTime, Duration, Utc};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use tempfile::TempDir;

const AUDIO_REFERENCE: &str = "staging/2026-10-08/clip-voice.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const CAPTURED_AT: &str = "2026-10-08T09:30:00Z";

struct Fixture {
    _directory: TempDir,
    path: String,
    typed_item: String,
    corrected_item: String,
    voice_item: String,
    work_item: String,
    deleted_item: String,
}

fn capture(
    capture_id: &str,
    text: Option<&str>,
    audio_reference: Option<&str>,
    scope: &str,
    session_topic: Option<&str>,
) -> Capture {
    Capture::new(
        capture_id.to_string(),
        text.map(str::to_string),
        audio_reference.map(str::to_string),
        CAPTURED_AT.to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        format!("route-{scope}"),
        false,
        "2026-10-08T09:30:01Z".to_string(),
        session_topic.map(str::to_string),
    )
    .unwrap()
}

impl Fixture {
    fn open(path: &str) -> Database {
        Database::open(path, Arc::new(SystemClock)).unwrap()
    }

    fn revision(database: &Database, item_id: &str) -> i32 {
        database
            .conn()
            .query_row(
                "SELECT revision FROM items WHERE item_id = ?",
                [item_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn correct(
        database: &mut Database,
        item_id: &str,
        event_id: &str,
        kind: CorrectionKind,
        old_value: Option<&str>,
        new_value: &str,
        happened_at: &str,
    ) {
        let revision = Self::revision(database, item_id);
        let event = Event::new(
            event_id.to_string(),
            item_id.to_string(),
            revision,
            EventType::Correction,
            EventPayload::Correction(Correction {
                kind,
                old_value: old_value.map(str::to_string),
                new_value: new_value.to_string(),
            }),
            happened_at.to_string(),
        )
        .unwrap();
        save_event(database, &event, revision).unwrap();
    }

    fn attach_transcript(database: &mut Database, item_id: &str, text: &str) {
        let now = DateTime::parse_from_rfc3339("2026-10-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // The test queues its own transcription job in place of the one import queued.
        database
            .conn()
            .execute(
                "DELETE FROM jobs WHERE item_id = ? AND job_type = ?",
                rusqlite::params![item_id, JOB_TYPE_TRANSCRIPTION_ATTACHMENT],
            )
            .unwrap();
        enqueue_job(
            database,
            "transcription-1".to_string(),
            item_id.to_string(),
            JOB_TYPE_TRANSCRIPTION_ATTACHMENT.to_string(),
            Self::revision(database, item_id),
            None,
            None,
            1,
            now,
        )
        .unwrap();
        let job = loop {
            let claimed = claim_job_with_lease(database, Duration::seconds(300), now)
                .unwrap()
                .expect("job is claimable");
            if claimed.job_id == "transcription-1" {
                break claimed;
            }
        };
        apply_transcription_outcome(
            database,
            &TranscriptionOutcome {
                job_id: job.job_id.clone(),
                lease_attempt: job.attempt_count,
                audio_reference: AUDIO_REFERENCE.to_string(),
                result: RecognizerResult::Transcript(RecognizedTranscript {
                    text: text.to_string(),
                    confidence: Some(0.91),
                    detected_language: Some("en-US".to_string()),
                    recognizer: "on-device-recognizer/1".to_string(),
                    audio_sha256: AUDIO_SHA256.to_string(),
                }),
                interpretation: None,
            },
            now + Duration::minutes(5),
        )
        .unwrap();
    }

    fn new() -> Fixture {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("core.sqlite").display().to_string();
        let mut database = Self::open(&path);
        for scope in ["personal", "work"] {
            database
                .conn()
                .execute(
                    "INSERT INTO routes (route_id, route_name, scope, processing_destinations,
                                         created_at)
                     VALUES (?, ?, ?, '[]', '2026-10-01T00:00:00Z')",
                    rusqlite::params![format!("route-{scope}"), format!("route-{scope}"), scope],
                )
                .unwrap();
        }
        let mut import = |record: Capture| {
            import_foreground_ingress(&mut database, &record)
                .unwrap()
                .item_id
        };
        let typed_item = import(capture(
            "capture-typed",
            Some("water the ferns"),
            None,
            "personal",
            None,
        ));
        let corrected_item = import(capture(
            "capture-corrected",
            Some("buy oat milk"),
            None,
            "personal",
            Some("Groceries"),
        ));
        let voice_item = import(capture(
            "capture-voice",
            None,
            Some(AUDIO_REFERENCE),
            "personal",
            None,
        ));
        let work_item = import(capture(
            "capture-work",
            Some("quarterly roadmap draft"),
            None,
            "work",
            None,
        ));
        let deleted_item = import(capture(
            "capture-deleted",
            Some("deleted-secret-text"),
            None,
            "personal",
            None,
        ));

        Self::correct(
            &mut database,
            &corrected_item,
            "correction-text",
            CorrectionKind::Text,
            Some("buy oat milk"),
            "buy oat milk and eggs",
            "2026-10-08T11:00:00Z",
        );
        Self::correct(
            &mut database,
            &corrected_item,
            "correction-type",
            CorrectionKind::Type,
            None,
            "action",
            "2026-10-08T11:05:00Z",
        );
        Self::correct(
            &mut database,
            &corrected_item,
            "correction-topic",
            CorrectionKind::SessionTopic,
            Some("Groceries"),
            "Weekly shop",
            "2026-10-08T11:10:00Z",
        );
        Self::attach_transcript(&mut database, &voice_item, "water the ferns tomorrow");
        let deleted_revision = Self::revision(&database, &deleted_item);
        mark_deletion_intent(
            &mut database,
            &deleted_item,
            deleted_revision,
            DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap();
        drop(database);
        Fixture {
            _directory: directory,
            path,
            typed_item,
            corrected_item,
            voice_item,
            work_item,
            deleted_item,
        }
    }
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<CoreEvent>,
    _recorder: Box<Recorder>,
    next_operation_id: u64,
}

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_item_detail(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
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

    fn read(&mut self, item_id: &str, scopes: &[&str]) -> Value {
        let event = self.call(&json!({"item_id": item_id, "read_scopes": scopes}));
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

    fn read_failure(&mut self, item_id: &str, scopes: &[&str]) -> Vec<u8> {
        let event = self.call(&json!({"item_id": item_id, "read_scopes": scopes}));
        assert_eq!(event.status, OHAND_CORE_STATUS_PERMANENT);
        event.payload
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

const BOTH: [&str; 2] = ["personal", "work"];

#[test]
fn a_typed_item_reports_its_original_text_and_no_history() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);

    let body = session.read(&fixture.typed_item, &BOTH);

    assert_eq!(body["item_id"], fixture.typed_item.as_str());
    assert_eq!(body["capture_id"], "capture-typed");
    assert_eq!(body["revision"], 0);
    assert_eq!(body["item_type"], Value::Null);
    assert_eq!(body["scope"], "personal");
    assert_eq!(body["session_topic"], Value::Null);
    assert_eq!(body["lifecycle_state"], "active");
    assert_eq!(body["captured_at"], CAPTURED_AT);
    assert_eq!(body["text"], "water the ferns");
    assert_eq!(body["text_basis"], "original");
    assert_eq!(body["original_text"], "water the ferns");
    assert_eq!(body["corrected_text"], Value::Null);
    assert_eq!(body["audio_reference"], Value::Null);
    assert_eq!(body["transcript"], Value::Null);
    assert_eq!(body["history"], json!([]));
    assert_eq!(body["history_truncated"], false);
}

#[test]
fn a_corrected_item_keeps_the_original_beside_the_correction_and_lists_history_newest_first() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);

    let body = session.read(&fixture.corrected_item, &BOTH);

    assert_eq!(body["revision"], 3);
    assert_eq!(body["item_type"], "action");
    assert_eq!(body["session_topic"], "Weekly shop");
    assert_eq!(body["text"], "buy oat milk and eggs");
    assert_eq!(body["text_basis"], "corrected");
    assert_eq!(body["original_text"], "buy oat milk");
    assert_eq!(body["corrected_text"], "buy oat milk and eggs");
    assert_eq!(
        body["history"],
        json!([
            {"event_id": "correction-topic", "revision": 2, "event_type": "correction",
             "happened_at": "2026-10-08T11:10:00Z", "suggestion_control": null,
             "correction": {"kind": "session_topic", "old_value": "Groceries",
                            "new_value": "Weekly shop"}},
            {"event_id": "correction-type", "revision": 1, "event_type": "correction",
             "happened_at": "2026-10-08T11:05:00Z", "suggestion_control": null,
             "correction": {"kind": "type", "old_value": null, "new_value": "action"}},
            {"event_id": "correction-text", "revision": 0, "event_type": "correction",
             "happened_at": "2026-10-08T11:00:00Z", "suggestion_control": null,
             "correction": {"kind": "text", "old_value": "buy oat milk",
                            "new_value": "buy oat milk and eggs"}},
        ])
    );
    assert_eq!(body["history_truncated"], false);
}

#[test]
fn a_transcribed_voice_item_reports_its_audio_and_transcript_provenance() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);

    let body = session.read(&fixture.voice_item, &BOTH);

    assert_eq!(body["text"], "water the ferns tomorrow");
    assert_eq!(body["text_basis"], "transcribed");
    assert_eq!(body["original_text"], Value::Null);
    assert_eq!(body["corrected_text"], Value::Null);
    assert_eq!(body["audio_reference"], AUDIO_REFERENCE);
    let transcript = &body["transcript"];
    assert_eq!(transcript["text"], "water the ferns tomorrow");
    assert_eq!(transcript["recognizer"], "on-device-recognizer/1");
    assert_eq!(transcript["source_job_id"], "transcription-1");
    assert_eq!(transcript["audio_sha256"], AUDIO_SHA256);
    assert_eq!(transcript["detected_language"], "en-US");
    assert_eq!(transcript["confidence"], 0.91);
    assert_eq!(
        DateTime::parse_from_rfc3339(transcript["transcribed_at"].as_str().unwrap()).unwrap(),
        DateTime::parse_from_rfc3339("2026-10-08T10:05:00Z").unwrap()
    );
    assert_eq!(
        transcript["attached_revision"].as_i64().unwrap(),
        body["revision"].as_i64().unwrap()
    );
    assert_eq!(
        body["history"][0]["event_id"],
        transcript["transcript_record_id"]
    );
    assert_eq!(body["history"][0]["event_type"], "transcript");
    assert_eq!(body["history"][0]["correction"], Value::Null);
}

#[test]
fn an_item_outside_the_read_scopes_is_indistinguishable_from_an_unknown_one() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);

    assert_eq!(
        session.read(&fixture.work_item, &["work"])["text"],
        "quarterly roadmap draft"
    );
    let excluded = session.read_failure(&fixture.work_item, &["personal"]);
    let unknown = session.read_failure("no-such-item", &["personal"]);
    let failure: Value = serde_json::from_slice(&excluded).unwrap();
    assert_eq!(failure["code"], "not_found");
    assert_eq!(failure["class"], "permanent");
    assert_eq!(failure["message"], "no stored record has that identifier");
    assert_eq!(excluded, unknown, "scope exclusion answers like a miss");
    assert!(
        !String::from_utf8_lossy(&excluded).contains(&fixture.work_item),
        "a failure never echoes the identifier"
    );
}

#[test]
fn a_deleted_item_is_not_found_and_leaks_no_text() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);

    let deleted = session.read_failure(&fixture.deleted_item, &BOTH);
    let unknown = session.read_failure("no-such-item", &BOTH);

    assert_eq!(deleted, unknown);
    assert_eq!(
        serde_json::from_slice::<Value>(&deleted).unwrap()["code"],
        "not_found"
    );
    assert!(!String::from_utf8_lossy(&deleted).contains("deleted-secret-text"));
}

#[test]
fn history_is_bounded_and_says_when_older_events_were_left_out() {
    let _guard = serial();
    let fixture = Fixture::new();
    {
        let mut database = Fixture::open(&fixture.path);
        let total = OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT + 2;
        for index in 0..total {
            let old_value = if index == 0 {
                None
            } else {
                Some(format!("topic-{}", index - 1))
            };
            Fixture::correct(
                &mut database,
                &fixture.typed_item,
                &format!("topic-correction-{index}"),
                CorrectionKind::SessionTopic,
                old_value.as_deref(),
                &format!("topic-{index}"),
                "2026-10-08T12:00:00Z",
            );
        }
    }
    let mut session = Session::over(&fixture.path);

    let body = session.read(&fixture.typed_item, &BOTH);

    let history = body["history"].as_array().unwrap();
    assert_eq!(history.len(), OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT);
    assert_eq!(body["history_truncated"], true);
    assert_eq!(body["revision"], 52);
    assert_eq!(history[0]["revision"], 51, "the newest event comes first");
    assert_eq!(history[0]["correction"]["new_value"], "topic-51");
    assert_eq!(
        history[OHAND_CORE_ITEM_DETAIL_HISTORY_LIMIT - 1]["revision"],
        2
    );
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let fixture = Fixture::new();
    let mut session = Session::over(&fixture.path);
    let item_id = fixture.typed_item.as_str();

    session.refuse(&json!({"item_id": item_id}));
    session.refuse(&json!({"item_id": item_id, "read_scopes": []}));
    session.refuse(&json!({"item_id": item_id, "read_scopes": ["household"]}));
    session.refuse(&json!({"item_id": "", "read_scopes": ["personal"]}));
    session.refuse(&json!({
        "item_id": "x".repeat(OHAND_CORE_MAX_ITEM_DETAIL_IDENTIFIER_BYTES + 1),
        "read_scopes": ["personal"],
    }));
    session.refuse(&json!({"read_scopes": ["personal"]}));
    session.refuse(&json!({"item_id": item_id, "read_scopes": ["personal"], "limit": 5}));

    let bytes = b"not json";
    let outcome = consume(unsafe {
        ohand_core_start_item_detail(session.handle, 99, bytes.as_ptr(), bytes.len())
    });
    assert_eq!(outcome.code(), "invalid_request");
    let empty =
        consume(unsafe { ohand_core_start_item_detail(session.handle, 99, std::ptr::null(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    let null =
        consume(unsafe { ohand_core_start_item_detail(session.handle, 99, std::ptr::null(), 4) });
    assert_eq!(null.code(), "null_argument");
}

#[test]
fn reading_an_item_changes_nothing() {
    let _guard = serial();
    let fixture = Fixture::new();
    let counts = |path: &str| -> (i64, i64) {
        let database = Fixture::open(path);
        let count = |table: &str| {
            database
                .conn()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        (count("events"), count("items"))
    };
    let before = counts(&fixture.path);
    let revision_before = Fixture::revision(&Fixture::open(&fixture.path), &fixture.corrected_item);
    let mut session = Session::over(&fixture.path);
    session.read(&fixture.corrected_item, &BOTH);
    session.read(&fixture.voice_item, &BOTH);
    assert_eq!(counts(&fixture.path), before);
    assert_eq!(
        Fixture::revision(&Fixture::open(&fixture.path), &fixture.corrected_item),
        revision_before
    );
}
