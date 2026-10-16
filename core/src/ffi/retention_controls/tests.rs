//! Drives the retention-control exports the way Swift does, against the real core store.

use super::*;
use crate::ffi::audio_retention::{
    ohand_core_release_audio_removal, ohand_core_start_audio_retention,
};
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event as CoreEvent, Recorder, WAIT,
};
use crate::ingress::import::import_foreground_ingress;
use crate::store::captures::Capture;
use chrono::{DateTime, Utc};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const TRANSCRIBED_AUDIO: &str = "FinalizedAudio/transcribed.m4a";
const SOLE_SOURCE_AUDIO: &str = "FinalizedAudio/sole.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";
const SECONDS_PER_DAY: i64 = 86_400;

struct Store {
    directory: std::path::PathBuf,
    path: String,
    transcribed: String,
    sole_source: String,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn import_voice(
    database: &mut Database,
    capture_id: &str,
    audio: &str,
    scope: &str,
    captured_at: &str,
) -> String {
    let capture = Capture::new(
        capture_id.to_string(),
        None,
        Some(audio.to_string()),
        captured_at.to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        scope.to_string(),
        format!("route-{scope}"),
        false,
        "2026-10-01T08:00:01Z".to_string(),
        None,
    )
    .unwrap();
    let item_id = import_foreground_ingress(database, &capture)
        .unwrap()
        .item_id;
    database
        .conn()
        .execute("DELETE FROM jobs WHERE item_id = ?", [&item_id])
        .unwrap();
    item_id
}

fn transcribe(database: &Database, item_id: &str, audio: &str, transcribed_at: DateTime<Utc>) {
    database
        .conn()
        .execute(
            "UPDATE items SET transcription_state = 'transcribed' WHERE item_id = ?",
            [item_id],
        )
        .unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO transcript_attachments (item_id, source_job_id, transcript_record_id, text,
                    attached_revision, audio_reference, audio_sha256, recognizer, detected_language,
                    confidence, transcribed_at)
             VALUES (?1, ?2, ?3, 'water the ferns', 1, ?4, ?5, 'on-device-recognizer/1', 'en-US',
                     0.9, ?6)",
            rusqlite::params![
                item_id,
                format!("job-{item_id}"),
                format!("record-{item_id}"),
                audio,
                AUDIO_SHA256,
                transcribed_at.to_rfc3339(),
            ],
        )
        .unwrap();
}

/// One voice item transcribed ten days ago and one that was never transcribed (its audio is the
/// only source).
fn store(name: &str, sole_source_scope: &str) -> Store {
    let directory = std::env::temp_dir().join(format!(
        "ohand-retention-controls-ffi-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = Database::open(&path, Arc::new(SystemClock)).unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-01T07:00:00Z'),
                    ('route-work', 'route-work', 'work', '[]', '2026-10-01T07:00:00Z')",
            [],
        )
        .unwrap();
    let transcribed = import_voice(
        &mut database,
        "capture-transcribed",
        TRANSCRIBED_AUDIO,
        "personal",
        "2026-10-01T08:00:00Z",
    );
    let sole_source = import_voice(
        &mut database,
        "capture-sole",
        SOLE_SOURCE_AUDIO,
        sole_source_scope,
        "2026-10-02T09:15:00Z",
    );
    transcribe(
        &database,
        &transcribed,
        TRANSCRIBED_AUDIO,
        Utc::now() - chrono::Duration::days(10),
    );
    Store {
        directory,
        path,
        transcribed,
        sole_source,
    }
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<CoreEvent>,
    _recorder: Box<Recorder>,
    next_operation_id: u64,
}

#[derive(Clone, Copy)]
enum Export {
    Read,
    Control,
    AudioRetention,
}

fn start(export: Export, handle: OhandCoreHandle, operation_id: u64, request: &Value) -> u32 {
    let bytes = serde_json::to_vec(request).unwrap();
    let outcome = consume(unsafe {
        match export {
            Export::Read => {
                ohand_core_start_retention_read(handle, operation_id, bytes.as_ptr(), bytes.len())
            }
            Export::Control => ohand_core_start_retention_control(
                handle,
                operation_id,
                bytes.as_ptr(),
                bytes.len(),
            ),
            Export::AudioRetention => {
                ohand_core_start_audio_retention(handle, operation_id, bytes.as_ptr(), bytes.len())
            }
        }
    });
    if outcome.status != OHAND_CORE_STATUS_OK {
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }
    outcome.status
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

    fn event(&mut self, export: Export, request: &Value) -> CoreEvent {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        assert_eq!(
            start(export, self.handle, operation_id, request),
            OHAND_CORE_STATUS_OK
        );
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_ne!(event.thread, std::thread::current().id());
        event
    }

    fn body(&mut self, export: Export, request: &Value) -> Value {
        let event = self.event(export, request);
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

    fn status(&mut self, scopes: &[&str]) -> Value {
        self.body(
            Export::Read,
            &json!({"command": "retention_status", "read_scopes": scopes}),
        )
    }

    fn set_period(&mut self, seconds: i64) -> Value {
        self.body(
            Export::Control,
            &json!({"command": "set_period", "period_seconds": seconds}),
        )
    }

    fn retry(&mut self, item_id: &str) -> Value {
        self.body(
            Export::Control,
            &json!({"command": "retry_transcription", "item_id": item_id}),
        )
    }

    fn sweep(&mut self) -> Value {
        self.body(
            Export::AudioRetention,
            &json!({"command": "sweep", "pressure": "normal"}),
        )
    }

    fn refuse(&mut self, export: Export, request: &Value) {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let bytes = serde_json::to_vec(request).unwrap();
        let outcome = consume(unsafe {
            match export {
                Export::Read => ohand_core_start_retention_read(
                    self.handle,
                    operation_id,
                    bytes.as_ptr(),
                    bytes.len(),
                ),
                _ => ohand_core_start_retention_control(
                    self.handle,
                    operation_id,
                    bytes.as_ptr(),
                    bytes.len(),
                ),
            }
        });
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

fn cleanup_kind<'a>(sweep: &'a Value, item_id: &str) -> &'a str {
    sweep["assessments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["item_id"] == item_id)
        .expect("the item is assessed")["cleanup"]["kind"]
        .as_str()
        .unwrap()
}

#[test]
fn status_reports_the_default_policy_and_the_unresolved_sole_audio() {
    let _guard = serial();
    let store = store("status", "personal");
    let mut session = Session::over(&store.path);

    let status = session.status(&["personal"]);
    assert_eq!(status["policy"]["period_seconds"], 7 * SECONDS_PER_DAY);
    assert_eq!(status["policy"]["is_default"], true);
    let unresolved = status["unresolved"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1, "a transcribed item is not unresolved");
    assert_eq!(unresolved[0]["item_id"], store.sole_source);
    assert_eq!(unresolved[0]["captured_at"], "2026-10-02T09:15:00Z");
    assert_eq!(unresolved[0]["reason"], "transcription_pending");
    assert_eq!(
        unresolved[0]["options"],
        json!(["retry_transcription", "delete_audio"])
    );
}

#[test]
fn the_period_is_stored_durably_and_the_sweep_reads_it() {
    let _guard = serial();
    let store = store("period", "personal");
    {
        let mut session = Session::over(&store.path);
        // Ten days after transcription the default period of seven days has expired the audio.
        assert_eq!(
            cleanup_kind(&session.sweep(), &store.transcribed),
            "remove_audio"
        );

        let stored = session.set_period(30 * SECONDS_PER_DAY);
        assert_eq!(stored["outcome"], "stored");
        assert_eq!(stored["period_seconds"], 30 * SECONDS_PER_DAY);
        assert_eq!(
            cleanup_kind(&session.sweep(), &store.transcribed),
            "none",
            "a sweep without a period uses the stored setting"
        );
    }

    // A new process finds the setting.
    let mut session = Session::over(&store.path);
    let status = session.status(&["personal"]);
    assert_eq!(status["policy"]["period_seconds"], 30 * SECONDS_PER_DAY);
    assert_eq!(status["policy"]["is_default"], false);
    assert_eq!(
        cleanup_kind(&session.sweep(), &store.transcribed),
        "none",
        "the stored period survives a restart"
    );

    session.set_period(SECONDS_PER_DAY);
    assert_eq!(
        cleanup_kind(&session.sweep(), &store.transcribed),
        "remove_audio"
    );
}

#[test]
fn out_of_bounds_periods_are_refused_and_nothing_is_stored() {
    let _guard = serial();
    let store = store("bounds", "personal");
    let mut session = Session::over(&store.path);
    for seconds in [0, -1, 3650 * SECONDS_PER_DAY + 1, i64::MAX] {
        session.refuse(
            Export::Control,
            &json!({"command": "set_period", "period_seconds": seconds}),
        );
    }
    session.refuse(
        Export::Control,
        &json!({"command": "set_period", "period_seconds": 60, "extra": 1}),
    );
    assert_eq!(session.status(&["personal"])["policy"]["is_default"], true);

    let longest = session.set_period(3650 * SECONDS_PER_DAY);
    assert_eq!(longest["period_seconds"], 3650 * SECONDS_PER_DAY);
    let shortest = session.set_period(1);
    assert_eq!(shortest["period_seconds"], 1);
}

#[test]
fn changing_the_period_never_expires_sole_source_audio() {
    let _guard = serial();
    let store = store("sole", "personal");
    let mut session = Session::over(&store.path);

    session.set_period(1);
    let sweep = session.sweep();
    assert_eq!(cleanup_kind(&sweep, &store.sole_source), "none");
    assert_eq!(cleanup_kind(&sweep, &store.transcribed), "remove_audio");
    let status = session.status(&["personal"]);
    assert_eq!(status["unresolved"].as_array().unwrap().len(), 1);

    let denied = session.body(
        Export::AudioRetention,
        &json!({"command": "begin_removal", "item_id": store.sole_source,
                "audio_reference": SOLE_SOURCE_AUDIO}),
    );
    assert_eq!(denied["decision"], "denied");
    assert!(denied.get("lease_id").is_none());
}

#[test]
fn a_user_removal_of_sole_source_audio_is_refused_and_points_to_item_deletion() {
    let _guard = serial();
    let store = store("user-removal", "personal");
    let mut session = Session::over(&store.path);

    let denied = session.body(
        Export::AudioRetention,
        &json!({"command": "begin_user_removal", "item_id": store.sole_source,
                "audio_reference": SOLE_SOURCE_AUDIO}),
    );
    assert_eq!(denied["decision"], "denied");
    assert_eq!(denied["reason"], "sole_source");
    assert!(denied.get("lease_id").is_none());

    let approved = session.body(
        Export::AudioRetention,
        &json!({"command": "begin_user_removal", "item_id": store.transcribed,
                "audio_reference": TRANSCRIBED_AUDIO}),
    );
    assert_eq!(approved["decision"], "approved");
    let lease_id = approved["lease_id"].as_u64().unwrap();
    assert_eq!(
        consume(ohand_core_release_audio_removal(session.handle, lease_id)).status,
        OHAND_CORE_STATUS_OK
    );
}

#[test]
fn get_audio_names_the_audio_and_whether_it_is_the_only_source() {
    let _guard = serial();
    let store = store("get-audio", "personal");
    let mut session = Session::over(&store.path);

    let sole = session.body(
        Export::Read,
        &json!({"command": "get_audio", "item_id": store.sole_source, "read_scopes": ["personal"]}),
    );
    assert_eq!(sole["audio_reference"], SOLE_SOURCE_AUDIO);
    assert_eq!(sole["sole_source"], true);

    let transcribed = session.body(
        Export::Read,
        &json!({"command": "get_audio", "item_id": store.transcribed, "read_scopes": ["personal"]}),
    );
    assert_eq!(transcribed["audio_reference"], TRANSCRIBED_AUDIO);
    assert_eq!(transcribed["sole_source"], false);
}

#[test]
fn reads_are_limited_to_the_callers_scopes() {
    let _guard = serial();
    let store = store("scopes", "work");
    let mut session = Session::over(&store.path);

    assert!(session.status(&["personal"])["unresolved"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        session.status(&["work", "personal"])["unresolved"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let event = session.event(
        Export::Read,
        &json!({"command": "get_audio", "item_id": store.sole_source, "read_scopes": ["personal"]}),
    );
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    assert!(String::from_utf8_lossy(&event.payload).contains("not_found"));

    for request in [
        json!({"command": "retention_status", "read_scopes": []}),
        json!({"command": "retention_status"}),
        json!({"command": "retention_status", "read_scopes": ["secret"]}),
        json!({"command": "get_audio", "item_id": "", "read_scopes": ["personal"]}),
        json!({"command": "nonsense", "read_scopes": ["personal"]}),
    ] {
        session.refuse(Export::Read, &request);
    }
}

#[test]
fn retry_transcription_requeues_through_the_core_and_reports_refusals() {
    let _guard = serial();
    let store = store("retry", "personal");
    let mut session = Session::over(&store.path);

    let requeued = session.retry(&store.sole_source);
    assert_eq!(requeued["outcome"], "requeued");
    assert!(!requeued["job_id"].as_str().unwrap().is_empty());
    let queued: i64 = Database::open(&store.path, Arc::new(SystemClock))
        .unwrap()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE item_id = ? AND status = 'queued'",
            [&store.sole_source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1);

    assert_eq!(
        session.retry(&store.sole_source)["outcome"],
        "job_in_flight"
    );
    let transcribed = session.retry(&store.transcribed);
    assert_eq!(transcribed["outcome"], "not_retryable");
    assert_eq!(transcribed["transcription_state"], "transcribed");

    let event = session.event(
        Export::Control,
        &json!({"command": "retry_transcription", "item_id": "no-such-item"}),
    );
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let payload = String::from_utf8_lossy(&event.payload).into_owned();
    assert!(payload.contains("not_found"), "{payload}");
    assert!(!payload.contains("no-such-item"));

    session.refuse(
        Export::Control,
        &json!({"command": "retry_transcription", "item_id": ""}),
    );
}
