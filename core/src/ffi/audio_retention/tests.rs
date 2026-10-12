//! Drives the retention export the way Swift does, against the real core store.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::ingress::import::import_foreground_ingress;
use crate::store::captures::Capture;
use chrono::{DateTime, Utc};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const EXPIRED_AUDIO: &str = "FinalizedAudio/expired.m4a";
const RETAINED_AUDIO: &str = "FinalizedAudio/retained.m4a";
const SOLE_SOURCE_AUDIO: &str = "FinalizedAudio/sole.m4a";
const AUDIO_SHA256: &str = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112";

struct Items {
    expired: String,
    retained: String,
    sole_source: String,
}

fn start(handle: OhandCoreHandle, operation_id: u64, request: &Value) -> Outcome {
    let bytes = serde_json::to_vec(request).unwrap();
    consume(unsafe {
        ohand_core_start_audio_retention(handle, operation_id, bytes.as_ptr(), bytes.len())
    })
}

fn open_file(path: &str) -> OhandCoreHandle {
    let mut handle = 0;
    let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    handle
}

fn import_voice(database: &mut Database, capture_id: &str, audio: &str) -> String {
    let capture = Capture::new(
        capture_id.to_string(),
        None,
        Some(audio.to_string()),
        "2026-10-01T08:00:00Z".to_string(),
        "UTC".to_string(),
        0,
        "en_US".to_string(),
        "gregorian".to_string(),
        "personal".to_string(),
        "route-personal".to_string(),
        false,
        "2026-10-01T08:00:01Z".to_string(),
        None,
    )
    .unwrap();
    import_foreground_ingress(database, &capture)
        .unwrap()
        .item_id
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

/// A store with one voice item transcribed long enough ago to be expired, one transcribed a day
/// ago and one that was never transcribed.
fn store(name: &str) -> (std::path::PathBuf, String, Items) {
    let directory =
        std::env::temp_dir().join(format!("ohand-retention-ffi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    let mut database = Database::open(&path, Arc::new(SystemClock)).unwrap();
    database
        .conn()
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at)
             VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-01T07:00:00Z')",
            [],
        )
        .unwrap();
    let items = Items {
        expired: import_voice(&mut database, "capture-expired", EXPIRED_AUDIO),
        retained: import_voice(&mut database, "capture-retained", RETAINED_AUDIO),
        sole_source: import_voice(&mut database, "capture-sole", SOLE_SOURCE_AUDIO),
    };
    transcribe(
        &database,
        &items.expired,
        EXPIRED_AUDIO,
        Utc::now() - chrono::Duration::days(10),
    );
    transcribe(
        &database,
        &items.retained,
        RETAINED_AUDIO,
        Utc::now() - chrono::Duration::days(1),
    );
    (directory, path, items)
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

    fn succeed(&mut self, request: &Value) -> Value {
        let operation_id = self.next_operation_id;
        self.next_operation_id += 1;
        let outcome = start(self.handle, operation_id, request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.events.recv_timeout(WAIT).expect("an outcome event");
        assert_eq!(event.operation_id, operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK);
        assert_ne!(event.thread, std::thread::current().id());
        let body: Value = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(body["operation_id"], operation_id);
        body
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
    }
}

fn assessment<'a>(body: &'a Value, item_id: &str) -> &'a Value {
    body["assessments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["item_id"] == item_id)
        .expect("the item is assessed")
}

#[test]
fn a_sweep_offers_cleanup_only_for_expired_audio() {
    let _guard = serial();
    let (directory, path, items) = store("sweep");
    let mut session = Session::over(open_file(&path));

    let body = session.succeed(&json!({"command": "sweep", "pressure": "normal"}));
    assert_eq!(body["assessments"].as_array().unwrap().len(), 3);

    let expired = assessment(&body, &items.expired);
    assert_eq!(expired["state"]["kind"], "expired");
    assert_eq!(expired["cleanup"]["kind"], "remove_audio");
    assert_eq!(expired["cleanup"]["audio_reference"], EXPIRED_AUDIO);
    assert_eq!(expired["sole_source"], false);

    let retained = assessment(&body, &items.retained);
    assert_eq!(retained["state"]["kind"], "retained");
    assert_eq!(retained["cleanup"]["kind"], "none");

    let sole = assessment(&body, &items.sole_source);
    assert_eq!(sole["state"]["kind"], "unresolved");
    assert_eq!(sole["state"]["reason"], "transcription_pending");
    assert_eq!(sole["sole_source"], true);
    assert_eq!(sole["cleanup"]["kind"], "none");
    assert_eq!(
        sole["options"],
        json!(["retry_transcription", "delete_audio"])
    );

    let pressured = session.succeed(&json!({"command": "sweep", "pressure": "low"}));
    assert_eq!(
        assessment(&pressured, &items.sole_source)["storage_pressure_notice"],
        "sole_source_retained"
    );
    assert_eq!(
        assessment(&pressured, &items.retained)["storage_pressure_notice"],
        "not_yet_eligible"
    );
    assert_eq!(
        assessment(&pressured, &items.expired)["storage_pressure_notice"],
        Value::Null
    );
    assert_eq!(
        assessment(&pressured, &items.sole_source)["cleanup"]["kind"],
        "none"
    );

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn removal_is_authorized_again_for_the_exact_audio_at_removal_time() {
    let _guard = serial();
    let (directory, path, items) = store("authorize");
    let mut session = Session::over(open_file(&path));

    let approved = session.succeed(&json!({
        "command": "authorize_removal", "item_id": items.expired, "audio_reference": EXPIRED_AUDIO,
    }));
    assert_eq!(approved["decision"], "approved");

    let denied = |session: &mut Session, item_id: &str, audio: &str| {
        session.succeed(&json!({
            "command": "authorize_removal", "item_id": item_id, "audio_reference": audio,
        }))
    };
    let other_audio = denied(&mut session, &items.expired, RETAINED_AUDIO);
    assert_eq!(other_audio["reason"], "audio_reference_mismatch");
    let retained = denied(&mut session, &items.retained, RETAINED_AUDIO);
    assert_eq!(retained["decision"], "denied");
    assert_eq!(retained["reason"], "not_yet_expired");
    let sole = denied(&mut session, &items.sole_source, SOLE_SOURCE_AUDIO);
    assert_eq!(sole["reason"], "unresolved");
    assert_eq!(sole["unresolved_reason"], "transcription_pending");
    let missing = denied(&mut session, "no-such-item", EXPIRED_AUDIO);
    assert_eq!(missing["reason"], "item_not_found");

    session.close();

    let database = Database::open(&path, Arc::new(SystemClock)).unwrap();
    database
        .conn()
        .execute(
            "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
            [&items.expired],
        )
        .unwrap();
    drop(database);
    let mut session = Session::over(open_file(&path));
    let deleted = denied(&mut session, &items.expired, EXPIRED_AUDIO);
    assert_eq!(deleted["reason"], "deletion_owns_cleanup");
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_configured_period_moves_the_expiry() {
    let _guard = serial();
    let (directory, path, items) = store("period");
    let mut session = Session::over(open_file(&path));

    let thirty_days = 30 * 24 * 60 * 60;
    let lengthened = session.succeed(&json!({
        "command": "authorize_removal", "item_id": items.expired, "audio_reference": EXPIRED_AUDIO,
        "period_seconds": thirty_days,
    }));
    assert_eq!(lengthened["reason"], "not_yet_expired");

    let shortened = session.succeed(&json!({
        "command": "sweep", "pressure": "normal", "period_seconds": 3600,
    }));
    assert_eq!(
        assessment(&shortened, &items.retained)["state"]["kind"],
        "expired"
    );
    assert_eq!(
        assessment(&shortened, &items.sole_source)["cleanup"]["kind"],
        "none"
    );

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn malformed_requests_are_refused_before_anything_is_queued() {
    let _guard = serial();
    let (directory, path, _items) = store("invalid");
    let handle = open_file(&path);

    for request in [
        json!({"command": "unknown_command"}),
        json!({"command": "sweep"}),
        json!({"command": "sweep", "pressure": "severe"}),
        json!({"command": "sweep", "pressure": "normal", "extra": 1}),
        json!({"command": "sweep", "pressure": "normal", "period_seconds": u64::MAX}),
        json!({"command": "sweep", "pressure": "normal", "period_seconds": 3650 * 86_400 + 1}),
        json!({"command": "authorize_removal", "item_id": "item", "audio_reference": ""}),
        json!({"command": "authorize_removal", "item_id": "", "audio_reference": "a"}),
        json!({"command": "authorize_removal", "item_id": "x".repeat(MAX_IDENTIFIER_BYTES + 1),
               "audio_reference": "a"}),
        json!([]),
        json!({"command": "begin_removal", "item_id": "item"}),
        json!({"command": "begin_removal", "item_id": "", "audio_reference": "a"}),
        json!({"command": "begin_removal", "item_id": "item", "audio_reference": ""}),
        json!({"command": "begin_removal", "item_id": "item", "audio_reference": "a",
               "audio_path": "/tmp/a"}),
        json!({"command": "remove_audio", "item_id": "item", "audio_reference": "a"}),
    ] {
        let outcome = start(handle, 1, &request);
        assert_eq!(outcome.status, OHAND_CORE_STATUS_PERMANENT, "{request}");
        assert_eq!(outcome.code(), "invalid_request", "{request}");
    }
    let empty = consume(unsafe { ohand_core_start_audio_retention(handle, 2, [].as_ptr(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    let null = consume(unsafe { ohand_core_start_audio_retention(handle, 3, std::ptr::null(), 4) });
    assert_eq!(null.code(), "null_argument");

    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    let _ = std::fs::remove_dir_all(directory);
}

fn release(handle: OhandCoreHandle, lease_id: u64) -> Outcome {
    consume(ohand_core_release_audio_removal(handle, lease_id))
}

fn begin(session: &mut Session, item_id: &str, reference: &str) -> Value {
    session.succeed(&json!({
        "command": "begin_removal", "item_id": item_id, "audio_reference": reference,
    }))
}

fn raw_connection(path: &str, busy: std::time::Duration) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection.busy_timeout(busy).unwrap();
    connection
}

fn protect(connection: &rusqlite::Connection, item_id: &str) -> rusqlite::Result<usize> {
    connection.execute(
        "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = ?",
        [item_id],
    )
}

fn is_busy(result: &rusqlite::Result<usize>) -> bool {
    matches!(
        result,
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

#[test]
fn an_approved_removal_holds_the_store_write_lock_until_it_is_released() {
    let _guard = serial();
    let (directory, path, items) = store("lease");
    let mut session = Session::over(open_file(&path));

    let granted = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    assert_eq!(granted["decision"], "approved");
    assert_eq!(granted["lease_millis"], 3000);
    let lease_id = granted["lease_id"].as_u64().expect("a lease");

    // A write from another connection (another handle or process) cannot land in the interval
    // between the approval and the file effect.
    let other = raw_connection(&path, std::time::Duration::from_millis(100));
    assert!(is_busy(&protect(&other, &items.expired)));
    // Readers are not blocked.
    let state: String = other
        .query_row(
            "SELECT lifecycle_state FROM items WHERE item_id = ?",
            [&items.expired],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(state, "deleted");

    assert_eq!(
        release(session.handle, lease_id).status,
        OHAND_CORE_STATUS_OK
    );
    assert_eq!(protect(&other, &items.expired).unwrap(), 1);

    // Decided again after the change, the removal is refused and holds nothing.
    let denied = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    assert_eq!(denied["decision"], "denied");
    assert_eq!(denied["reason"], "deletion_owns_cleanup");
    assert_eq!(denied["lease_id"], Value::Null);
    assert_eq!(protect(&other, &items.retained).unwrap(), 1);

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_write_that_would_protect_approved_audio_waits_for_the_effect_to_finish() {
    let _guard = serial();
    let (directory, path, items) = store("race");
    let mut session = Session::over(open_file(&path));

    let granted = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    let lease_id = granted["lease_id"].as_u64().expect("a lease");

    // The change is attempted after the approval and before the native effect (the lease is still
    // held); it commits only once the effect has finished and the lease is released.
    let (finished_sender, finished) = std::sync::mpsc::channel();
    let writer_path = path.clone();
    let writer_item = items.expired.clone();
    let writer = std::thread::spawn(move || {
        let connection = raw_connection(&writer_path, std::time::Duration::from_secs(10));
        let result = protect(&connection, &writer_item);
        finished_sender.send(std::time::Instant::now()).unwrap();
        result.unwrap()
    });
    assert!(
        finished
            .recv_timeout(std::time::Duration::from_millis(400))
            .is_err(),
        "the protecting write must wait while the lease is held"
    );
    let released_at = std::time::Instant::now();
    assert_eq!(
        release(session.handle, lease_id).status,
        OHAND_CORE_STATUS_OK
    );
    let committed_at = finished.recv_timeout(WAIT).expect("the write commits");
    assert!(committed_at >= released_at);
    assert_eq!(writer.join().unwrap(), 1);

    let denied = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    assert_eq!(denied["reason"], "deletion_owns_cleanup");

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_denied_removal_holds_no_lock_and_no_lease() {
    let _guard = serial();
    let (directory, path, items) = store("denied");
    let mut session = Session::over(open_file(&path));
    let other = raw_connection(&path, std::time::Duration::from_millis(100));

    for (item_id, reference, reason) in [
        (&items.retained, RETAINED_AUDIO, "not_yet_expired"),
        (&items.sole_source, SOLE_SOURCE_AUDIO, "unresolved"),
        (&items.expired, RETAINED_AUDIO, "audio_reference_mismatch"),
    ] {
        let denied = begin(&mut session, item_id, reference);
        assert_eq!(denied["decision"], "denied");
        assert_eq!(denied["reason"], reason);
        assert_eq!(denied["lease_id"], Value::Null);
        assert_eq!(
            other
                .execute(
                    "UPDATE items SET revision = revision WHERE item_id = ?",
                    [item_id]
                )
                .unwrap(),
            1,
            "a denial must not keep the store locked"
        );
    }
    let missing = begin(&mut session, "no-such-item", EXPIRED_AUDIO);
    assert_eq!(missing["reason"], "item_not_found");
    let lengthened = session.succeed(&json!({
        "command": "begin_removal", "item_id": items.expired, "audio_reference": EXPIRED_AUDIO,
        "period_seconds": 30 * 24 * 60 * 60,
    }));
    assert_eq!(lengthened["reason"], "not_yet_expired");
    assert_eq!(lengthened["lease_id"], Value::Null);
    assert_eq!(lease::live_leases(), 0);

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_lease_that_is_not_released_expires_and_frees_the_store() {
    let _guard = serial();
    let (directory, path, items) = store("expiry");
    let database = Database::open(&path, Arc::new(SystemClock)).unwrap();
    let store_path = std::path::Path::new(&path);

    let begun = lease::begin(
        store_path,
        7,
        &SystemClock,
        &RetentionPolicy::default(),
        &items.expired,
        EXPIRED_AUDIO,
        std::time::Duration::from_millis(300),
    )
    .unwrap();
    let lease_id = begun.lease_id.expect("an approved removal is leased");
    let other = raw_connection(&path, std::time::Duration::from_millis(50));
    assert!(is_busy(&protect(&other, &items.expired)));

    // The caller never releases: the store is freed by the expiry, and a late release says so.
    let waiting = raw_connection(&path, std::time::Duration::from_secs(5));
    assert_eq!(protect(&waiting, &items.expired).unwrap(), 1);
    assert_eq!(lease::live_leases(), 0);
    let late = lease::release(7, lease_id).unwrap_err();
    assert_eq!(late.code, "lease_expired");
    assert_eq!(late.status(), OHAND_CORE_STATUS_TRANSIENT);
    drop(database);
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn only_the_handle_that_holds_a_lease_can_release_it() {
    let _guard = serial();
    let (directory, path, items) = store("owner");
    let mut session = Session::over(open_file(&path));
    let other_handle = open_file(&path);

    let granted = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    let lease_id = granted["lease_id"].as_u64().unwrap();
    let other = raw_connection(&path, std::time::Duration::from_millis(50));

    let foreign = release(other_handle, lease_id);
    assert_eq!(foreign.code(), "invalid_request");
    assert!(is_busy(&protect(&other, &items.expired)), "still held");
    assert_eq!(release(0, lease_id).code(), "invalid_handle");
    assert_eq!(
        release(session.handle, lease_id + 1000).code(),
        "lease_expired"
    );

    assert_eq!(
        release(session.handle, lease_id).status,
        OHAND_CORE_STATUS_OK
    );
    assert_eq!(release(session.handle, lease_id).code(), "lease_expired");
    assert_eq!(protect(&other, &items.expired).unwrap(), 1);

    assert_eq!(consume(ohand_core_close(other_handle)).status, 0);
    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_busy_store_defers_the_removal_instead_of_deciding_it() {
    let _guard = serial();
    let (directory, path, items) = store("busy");
    let mut session = Session::over(open_file(&path));

    let writer = raw_connection(&path, std::time::Duration::from_millis(50));
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let operation_id = session.next_operation_id;
    session.next_operation_id += 1;
    let request = json!({
        "command": "begin_removal", "item_id": items.expired, "audio_reference": EXPIRED_AUDIO,
    });
    assert_eq!(start(session.handle, operation_id, &request).status, 0);
    let event = session.events.recv_timeout(WAIT).expect("an outcome event");
    assert_eq!(event.status, OHAND_CORE_STATUS_TRANSIENT);
    let failure: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(failure["code"], "store_busy");
    writer.execute_batch("ROLLBACK").unwrap();

    let granted = begin(&mut session, &items.expired, EXPIRED_AUDIO);
    assert_eq!(granted["decision"], "approved");
    let lease_id = granted["lease_id"].as_u64().unwrap();
    assert_eq!(
        release(session.handle, lease_id).status,
        OHAND_CORE_STATUS_OK
    );

    session.close();
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_store_without_a_file_cannot_be_leased() {
    let _guard = serial();
    let mut handle = 0;
    let memory = ":memory:";
    let opened = consume(unsafe { ohand_core_open(memory.as_ptr(), memory.len(), &mut handle) });
    assert_eq!(opened.status, OHAND_CORE_STATUS_OK);
    let session = Session::over(handle);

    let operation_id = session.next_operation_id;
    let request = json!({
        "command": "begin_removal", "item_id": "item", "audio_reference": EXPIRED_AUDIO,
    });
    assert_eq!(start(handle, operation_id, &request).status, 0);
    let event = session.events.recv_timeout(WAIT).expect("an outcome event");
    assert_ne!(event.status, OHAND_CORE_STATUS_OK);
    let failure: Value = serde_json::from_slice(&event.payload).unwrap();
    assert_eq!(failure["code"], "store_unavailable");

    session.close();
}
