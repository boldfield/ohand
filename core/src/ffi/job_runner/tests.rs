//! Drives the job drain exports the way the native layer does, against the real core store and the
//! real provider adapters, with a recording stand-in for the native host.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::jobs::queue::{complete_job, get_job, JobStatus};
use crate::providers::contracts::{
    CapabilityMetadata, ProviderCapability, ProviderProfile, ProviderProfileBuilder,
    StructuredOutputMode,
};
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const CREDENTIAL_REFERENCE: &str = "credential-ref/synthetic-primary";
const NOTE_TEXT: &str = "call the roofer tomorrow at 9";
const CAPTURE_ID: &str = "5a1c0000-0000-4000-8000-0000000000c1";
const ITEM_ID: &str = "5a1c0000-0000-4000-8000-0000000000e1";
const REQUEST_VERSION: &str = "5a1c0000-0000-4000-8000-0000000000a1";
const JOB_ID: &str = "job-synthetic-1";
const NATIVE_JOB_ID: &str = "job-synthetic-native";
const ROUTE_ID: &str = "route-synthetic";
const NATIVE_JOB_TYPE: &str = "transcribe";

struct Command {
    request_id: u64,
    command: u32,
    data: Vec<u8>,
    thread: std::thread::ThreadId,
}

struct NativeSide {
    sender: Mutex<std::sync::mpsc::Sender<Command>>,
}

unsafe extern "C" fn record_command(
    context: *mut c_void,
    request_id: u64,
    command: u32,
    data: *const u8,
    len: usize,
) {
    let native = &*(context as *const NativeSide);
    let data = if len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let _ = native.sender.lock().unwrap().send(Command {
        request_id,
        command,
        data,
        thread: std::thread::current().id(),
    });
}

fn temporary_store(name: &str) -> (std::path::PathBuf, String) {
    let directory = std::env::temp_dir().join(format!("ohand-jobs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    (directory, path)
}

fn anthropic_profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-profile",
        ProviderProtocol::Anthropic,
        "synthetic-model",
    )
    .credential_ref(CREDENTIAL_REFERENCE)
    .timeout_seconds(30)
    .authorized_destination(ANTHROPIC_ORIGIN)
    .capability(
        CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "ffi-fixtures:job_runner",
        )
        .with_input_size_limit(500)
        .with_structured_output(StructuredOutputMode::JsonSchema),
    )
    .build()
    .expect("valid profile")
}

/// Stands in for the import and profile installation, which create these rows in production.
fn seed(path: &str, with_native_job: bool) {
    let profile = anthropic_profile();
    let mut database =
        Database::open(path, std::sync::Arc::new(crate::store::schema::SystemClock)).unwrap();
    let transaction = database.transaction().unwrap();
    let record = serde_json::to_value(&profile).unwrap();
    let text_of = |key: &str| record[key].to_string();
    transaction
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, \
             capabilities, created_at, revoked_at) VALUES (?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, NULL)",
            rusqlite::params![
                profile.profile_version(),
                profile.profile_id(),
                record["protocol"].as_str().unwrap(),
                profile.model(),
                CREDENTIAL_REFERENCE,
                profile.timeout_seconds(),
                text_of("retry_policy"),
                text_of("authorized_destinations"),
                text_of("capabilities"),
                record["created_at"].as_str().unwrap(),
            ],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, \
             timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, \
             entry_locked, created_at) VALUES (?, ?, NULL, '2026-10-08T09:30:00Z', \
             'America/Chicago', -300, 'en_US', 'gregorian', 'personal', ?, 0, \
             '2026-10-08T09:30:01Z')",
            rusqlite::params![CAPTURE_ID, NOTE_TEXT, ROUTE_ID],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', 'unprocessed', \
             'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')",
            rusqlite::params![ITEM_ID, CAPTURE_ID],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
             created_at) VALUES (?, 'synthetic', 'personal', ?, '2026-10-08T09:00:00Z')",
            rusqlite::params![ROUTE_ID, json!([ANTHROPIC_ORIGIN]).to_string()],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO route_authorizations (route_id, capability, authorized_destinations, \
             created_at) VALUES (?, 'text_interpretation', ?, '2026-10-08T09:00:00Z')",
            rusqlite::params![ROUTE_ID, json!([ANTHROPIC_ORIGIN]).to_string()],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
             profile_version, request_version, status, attempt_count, created_at) \
             VALUES (?, 1, ?, 'interpret', 0, ?, ?, 'queued', 0, '2026-10-08T09:30:02Z')",
            rusqlite::params![JOB_ID, ITEM_ID, profile.profile_version(), REQUEST_VERSION],
        )
        .unwrap();
    if with_native_job {
        transaction
            .execute(
                "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, \
                 source_revision, profile_version, request_version, status, attempt_count, \
                 created_at) VALUES (?, 1, ?, ?, 0, NULL, NULL, 'queued', 0, \
                 '2026-10-08T09:30:03Z')",
                rusqlite::params![NATIVE_JOB_ID, ITEM_ID, NATIVE_JOB_TYPE],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

struct Session {
    handle: OhandCoreHandle,
    path: String,
    events: Receiver<Event>,
    commands: Receiver<Command>,
    native_context: Box<NativeSide>,
    _recorder: Box<Recorder>,
    directory: std::path::PathBuf,
}

impl Session {
    fn open(name: &str, with_native_job: bool) -> Session {
        let (directory, path) = temporary_store(name);
        seed(&path, with_native_job);
        let mut handle = 0;
        let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        let (sender, commands) = std::sync::mpsc::channel();
        Session {
            handle,
            path,
            events,
            commands,
            native_context: Box::new(NativeSide {
                sender: Mutex::new(sender),
            }),
            _recorder: recorder,
            directory,
        }
    }

    fn context(&self) -> *mut c_void {
        &*self.native_context as *const NativeSide as *mut c_void
    }

    fn register_host(&self, job_types: &[&str]) -> Outcome {
        let list = serde_json::to_vec(job_types).unwrap();
        consume(unsafe {
            ohand_core_set_job_host(
                self.handle,
                Some(record_command),
                self.context(),
                list.as_ptr(),
                list.len(),
            )
        })
    }

    fn clear_host(&self) {
        let outcome = consume(unsafe {
            ohand_core_set_job_host(self.handle, None, std::ptr::null_mut(), std::ptr::null(), 0)
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    }

    fn start_at(&self, operation_id: u64, path: &str) -> Outcome {
        consume(unsafe {
            ohand_core_start_job_drain(self.handle, operation_id, path.as_ptr(), path.len())
        })
    }

    fn start(&self, operation_id: u64) -> Outcome {
        self.start_at(operation_id, &self.path)
    }

    fn event(&self, operation_id: u64) -> Event {
        let event = self.events.recv_timeout(WAIT).expect("an event");
        assert_eq!(event.operation_id, operation_id);
        event
    }

    fn command(&self) -> Command {
        self.commands.recv_timeout(WAIT).expect("a native command")
    }

    fn assert_no_command(&self) {
        assert!(self
            .commands
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    fn complete(&self, request_id: u64, status: u32, body: &[u8]) -> Outcome {
        let headers = br#"[["content-type","application/json"]]"#;
        consume(unsafe {
            ohand_core_complete_job_exchange(
                self.handle,
                request_id,
                status,
                headers.as_ptr(),
                headers.len(),
                body.as_ptr(),
                body.len(),
            )
        })
    }

    fn fail(&self, request_id: u64, name: &str) -> Outcome {
        consume(unsafe {
            ohand_core_fail_job_exchange(self.handle, request_id, name.as_ptr(), name.len())
        })
    }

    fn finish(&self, request_id: u64, outcome: u32, reason: &str) -> Outcome {
        consume(unsafe {
            ohand_core_finish_job_capability(
                self.handle,
                request_id,
                outcome,
                reason.as_ptr(),
                reason.len(),
            )
        })
    }

    fn cancel_drain(&self) {
        assert_eq!(
            consume(ohand_core_cancel_job_drain(self.handle)).status,
            OHAND_CORE_STATUS_OK
        );
    }

    fn database(&self) -> Database {
        Database::open(
            &self.path,
            std::sync::Arc::new(crate::store::schema::SystemClock),
        )
        .unwrap()
    }

    fn job(&self, job_id: &str) -> Job {
        get_job(&self.database(), job_id).unwrap().unwrap()
    }

    fn close(self) {
        self.clear_host();
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn payload_of(event: &Event) -> Value {
    serde_json::from_slice(&event.payload).unwrap()
}

fn wait_for_drain_to_end(handle: u64) {
    let deadline = Instant::now() + WAIT;
    while lock(&DRAINS).contains_key(&handle) {
        assert!(Instant::now() < deadline, "drain slot was not released");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_until_idle(handle: u64) {
    let deadline = Instant::now() + WAIT;
    while lock(&EXCHANGES).keys().any(|(owner, _)| *owner == handle)
        || lock(&CAPABILITIES)
            .keys()
            .any(|(owner, _)| *owner == handle)
    {
        assert!(Instant::now() < deadline, "request slot was not released");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn anthropic_reply() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "msg_synthetic_01",
        "type": "message",
        "role": "assistant",
        "model": "synthetic-model",
        "content": [{
            "type": "tool_use",
            "id": "toolu_synthetic_01",
            "name": "interpret",
            "input": {
                "operation": { "kind": "annotate" },
                "item_type": "action",
                "source_spans": [{ "start": 0, "end": 4 }],
            },
        }],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 42, "output_tokens": 17 },
    }))
    .unwrap()
}

fn only_job(summary: &Value) -> &Value {
    let jobs = summary["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{summary}");
    &jobs[0]
}

#[test]
fn a_ready_job_is_sent_to_the_pinned_destination_off_the_worker_and_completes() {
    let _guard = serial();
    let session = Session::open("valid", false);
    assert_eq!(session.register_host(&[]).status, OHAND_CORE_STATUS_OK);

    assert_eq!(session.start(21).status, OHAND_CORE_STATUS_OK);
    let command = session.command();
    assert_eq!(command.command, OHAND_JOB_HOST_COMMAND_SEND);
    let send: Value = serde_json::from_slice(&command.data).unwrap();
    assert_eq!(send["operation_id"], command.request_id);
    assert_eq!(send["url"], "https://api.anthropic.com/v1/messages");
    assert_eq!(send["method"], "POST");
    assert_eq!(send["authorized_origins"], json!([ANTHROPIC_ORIGIN]));
    assert_eq!(
        send["credential"],
        json!({ "reference": CREDENTIAL_REFERENCE, "header": "x-api-key", "scheme": null })
    );
    assert!(send["body"].as_str().unwrap().contains(NOTE_TEXT));
    assert!(!String::from_utf8_lossy(&command.data).contains(ROUTE_ID));
    assert_eq!(session.job(JOB_ID).status, JobStatus::Running);

    assert_eq!(
        session
            .complete(command.request_id, 200, &anthropic_reply())
            .status,
        OHAND_CORE_STATUS_OK
    );
    let finished = session.event(21);
    assert_eq!(finished.status, OHAND_CORE_STATUS_OK);
    assert_ne!(
        finished.thread, command.thread,
        "the final event is delivered by the core, not the drain thread"
    );
    let summary = payload_of(&finished);
    assert_eq!(summary["stop"], "idle");
    let job = only_job(&summary);
    assert_eq!(job["job_id"], JOB_ID);
    assert_eq!(job["job_type"], "interpret");
    assert_eq!(job["result"], "settled");
    assert_eq!(job["settlement"], "completed");
    assert_eq!(session.job(JOB_ID).status, JobStatus::Completed);

    wait_until_idle(session.handle);
    assert_eq!(
        session.complete(command.request_id, 200, b"{}").code(),
        "not_found"
    );
    session.assert_no_command();
    session.close();
}

#[test]
fn overlapping_starts_are_refused_and_a_later_start_runs_again() {
    let _guard = serial();
    let session = Session::open("overlap", false);
    session.register_host(&[]);

    assert_eq!(session.start(31).status, OHAND_CORE_STATUS_OK);
    let command = session.command();
    for operation_id in [32, 33, 34] {
        let refused = session.start(operation_id);
        assert_eq!(refused.status, OHAND_CORE_STATUS_TRANSIENT);
        assert_eq!(refused.code(), "drain_in_progress");
    }
    session.assert_no_command();

    session.complete(command.request_id, 200, &anthropic_reply());
    assert_eq!(payload_of(&session.event(31))["stop"], "idle");
    wait_for_drain_to_end(session.handle);

    assert_eq!(session.start(35).status, OHAND_CORE_STATUS_OK);
    let summary = payload_of(&session.event(35));
    assert_eq!(summary["stop"], "idle");
    assert!(summary["jobs"].as_array().unwrap().is_empty());
    session.assert_no_command();
    session.close();
}

#[test]
fn cancelling_a_drain_requeues_the_job_at_a_checkpoint_without_spending_an_attempt() {
    let _guard = serial();
    let session = Session::open("cancel", false);
    session.register_host(&[]);

    session.start(41);
    let send = session.command();
    assert_eq!(send.command, OHAND_JOB_HOST_COMMAND_SEND);
    let attempts_while_running = session.job(JOB_ID).attempt_count;

    session.cancel_drain();
    session.cancel_drain();
    let cancel = session.command();
    assert_eq!(cancel.command, OHAND_JOB_HOST_COMMAND_CANCEL_EXCHANGE);
    assert_eq!(cancel.request_id, send.request_id);

    let summary = payload_of(&session.event(41));
    assert!(
        ["cancelled", "interrupted"].contains(&summary["stop"].as_str().unwrap()),
        "{summary}"
    );
    assert_eq!(only_job(&summary)["settlement"], "interrupted");
    let job = session.job(JOB_ID);
    assert_eq!(job.status, JobStatus::Queued);
    assert!(
        job.attempt_count <= attempts_while_running,
        "an interrupted job keeps its retry budget"
    );
    wait_for_drain_to_end(session.handle);
    assert_eq!(
        session.complete(send.request_id, 200, b"{}").code(),
        "not_found"
    );

    // The next drain resumes the job where it was put back.
    assert_eq!(session.start(42).status, OHAND_CORE_STATUS_OK);
    let resumed = session.command();
    session.complete(resumed.request_id, 200, &anthropic_reply());
    let summary = payload_of(&session.event(42));
    assert_eq!(only_job(&summary)["settlement"], "completed");
    assert_eq!(session.job(JOB_ID).status, JobStatus::Completed);
    session.close();
}

#[test]
fn a_native_transport_failure_counts_one_retry_and_schedules_backoff() {
    let _guard = serial();
    let session = Session::open("transient", false);
    session.register_host(&[]);

    session.start(51);
    let send = session.command();
    assert_eq!(session.fail(send.request_id, "unavailable").status, 0);
    assert_eq!(
        session.fail(send.request_id, "RateLimited").code(),
        "invalid_request"
    );
    let summary = payload_of(&session.event(51));
    assert_eq!(only_job(&summary)["settlement"], "backed_off");
    let job = session.job(JOB_ID);
    assert_eq!(job.status, JobStatus::Queued);
    assert!(job.next_attempt_at.is_some());
    session.close();
}

#[test]
fn a_native_cancelled_answer_puts_the_job_back_without_counting_a_failure() {
    let _guard = serial();
    let session = Session::open("offline", false);
    session.register_host(&[]);

    session.start(55);
    let send = session.command();
    let attempts_while_running = session.job(JOB_ID).attempt_count;
    assert_eq!(session.fail(send.request_id, "cancelled").status, 0);
    let summary = payload_of(&session.event(55));
    assert_eq!(summary["stop"], "interrupted");
    assert_eq!(only_job(&summary)["settlement"], "interrupted");
    let job = session.job(JOB_ID);
    assert_eq!(job.status, JobStatus::Queued);
    assert!(job.attempt_count <= attempts_while_running);
    assert!(job
        .next_attempt_at
        .is_none_or(|at| at <= chrono::Utc::now()));
    session.close();
}

#[test]
fn a_drain_is_refused_without_a_host_a_shareable_store_or_an_open_handle() {
    let _guard = serial();
    let session = Session::open("refused", false);

    let no_host = session.start(61);
    assert_eq!(no_host.status, OHAND_CORE_STATUS_UNSUPPORTED);
    assert_eq!(no_host.code(), "host_not_registered");

    session.register_host(&[]);
    assert_eq!(
        session.start_at(62, ":memory:").code(),
        "store_not_shareable"
    );
    assert_eq!(session.start_at(63, "").code(), "invalid_path");
    assert_eq!(session.job(JOB_ID).status, JobStatus::Queued);
    session.assert_no_command();

    session.clear_host();
    assert_eq!(session.start(64).code(), "host_not_registered");

    let stale = session.handle;
    let session_directory = session.directory.clone();
    let path = session.path.clone();
    session.close();
    let closed =
        consume(unsafe { ohand_core_start_job_drain(stale, 65, path.as_ptr(), path.len()) });
    assert_eq!(closed.code(), "invalid_handle");
    let _ = std::fs::remove_dir_all(session_directory);
}

#[test]
fn malformed_host_registrations_are_refused_and_change_nothing() {
    let _guard = serial();
    let session = Session::open("registration", false);

    for list in [
        r#"["interpret"]"#,
        r#"["Transcribe"]"#,
        r#"[""]"#,
        r#"[1]"#,
        r#"{"a":1}"#,
        r#"["a b"]"#,
    ] {
        let outcome = consume(unsafe {
            ohand_core_set_job_host(
                session.handle,
                Some(record_command),
                session.context(),
                list.as_ptr(),
                list.len(),
            )
        });
        assert_ne!(outcome.status, OHAND_CORE_STATUS_OK, "{list}");
    }
    assert_eq!(session.start(71).code(), "host_not_registered");

    let unknown = consume(unsafe {
        ohand_core_set_job_host(
            9_999_999,
            Some(record_command),
            session.context(),
            std::ptr::null(),
            0,
        )
    });
    assert_eq!(unknown.code(), "invalid_handle");
    session.close();
}

#[test]
fn a_superseded_registrant_cannot_clear_its_replacement() {
    let _guard = serial();
    let session = Session::open("supersede", false);
    session.register_host(&[]);

    let other = Box::new(NativeSide {
        sender: Mutex::new(std::sync::mpsc::channel().0),
    });
    let outcome = consume(unsafe {
        ohand_core_set_job_host(
            session.handle,
            None,
            &*other as *const NativeSide as *mut c_void,
            std::ptr::null(),
            0,
        )
    });
    assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    assert_eq!(session.start(81).status, OHAND_CORE_STATUS_OK);
    let send = session.command();
    session.complete(send.request_id, 200, &anthropic_reply());
    session.event(81);
    session.close();
}

#[test]
fn clearing_the_host_cancels_the_running_drain_and_requeues_the_job() {
    let _guard = serial();
    let session = Session::open("clear", false);
    session.register_host(&[]);

    session.start(91);
    session.command();
    session.clear_host();
    let summary = payload_of(&session.event(91));
    assert_eq!(only_job(&summary)["settlement"], "interrupted");
    assert_eq!(session.job(JOB_ID).status, JobStatus::Queued);
    session.close();
}

#[test]
fn closing_the_core_ends_a_running_drain_and_leaves_the_job_resumable() {
    let _guard = serial();
    let session = Session::open("close", false);
    session.register_host(&[]);

    session.start(101);
    session.command();
    let path = session.path.clone();
    let directory = session.directory.clone();
    let handle = session.handle;
    session.clear_host();
    assert_eq!(consume(ohand_core_close(handle)).status, 0);
    wait_for_drain_to_end(handle);

    let database = Database::open(
        &path,
        std::sync::Arc::new(crate::store::schema::SystemClock),
    )
    .unwrap();
    assert_eq!(
        get_job(&database, JOB_ID).unwrap().unwrap().status,
        JobStatus::Queued
    );
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn a_native_capability_runs_its_job_type_and_settles_through_the_core() {
    let _guard = serial();
    let session = Session::open("capability", true);
    session.register_host(&[NATIVE_JOB_TYPE]);

    session.start(111);
    let mut seen_native = false;
    let mut seen_send = false;
    for _ in 0..2 {
        let command = session.command();
        match command.command {
            OHAND_JOB_HOST_COMMAND_SEND => {
                seen_send = true;
                session.complete(command.request_id, 200, &anthropic_reply());
            }
            OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY => {
                seen_native = true;
                let run: Value = serde_json::from_slice(&command.data).unwrap();
                assert_eq!(run["job_id"], NATIVE_JOB_ID);
                assert_eq!(run["job_type"], NATIVE_JOB_TYPE);
                let attempt = run["attempt"].as_i64().unwrap() as i32;
                complete_job(&mut session.database(), NATIVE_JOB_ID, attempt).unwrap();
                assert_eq!(
                    session
                        .finish(command.request_id, OHAND_JOB_CAPABILITY_SETTLED, "")
                        .status,
                    OHAND_CORE_STATUS_OK
                );
            }
            other => panic!("unexpected command {other}"),
        }
    }
    assert!(seen_native && seen_send);
    let summary = payload_of(&session.event(111));
    assert_eq!(summary["jobs"].as_array().unwrap().len(), 2, "{summary}");
    assert!(summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|job| job["settlement"] == "completed"));
    assert_eq!(session.job(NATIVE_JOB_ID).status, JobStatus::Completed);
    assert_eq!(session.job(JOB_ID).status, JobStatus::Completed);
    session.close();
}

#[test]
fn capability_failures_use_the_core_retry_rules_and_never_store_content() {
    let _guard = serial();
    let session = Session::open("capability-failures", true);
    session.register_host(&[NATIVE_JOB_TYPE]);

    session.start(121);
    let mut handled = 0;
    while handled < 2 {
        let command = session.command();
        if command.command == OHAND_JOB_HOST_COMMAND_SEND {
            session.complete(command.request_id, 200, &anthropic_reply());
        } else {
            assert_eq!(
                session
                    .finish(
                        command.request_id,
                        OHAND_JOB_CAPABILITY_TRANSIENT,
                        "call the roofer at 9",
                    )
                    .status,
                OHAND_CORE_STATUS_OK
            );
        }
        handled += 1;
    }
    let summary = payload_of(&session.event(121));
    let native = summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_type"] == NATIVE_JOB_TYPE)
        .unwrap();
    assert_eq!(native["result"], "retry_scheduled");
    let job = session.job(NATIVE_JOB_ID);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.failure_reason.as_deref(), Some(FALLBACK_REASON));
    assert!(!format!("{job:?}").contains("roofer"));
    session.close();
}

#[test]
fn a_permanent_capability_failure_ends_the_job_and_an_interrupt_spends_nothing() {
    let _guard = serial();
    let session = Session::open("capability-permanent", true);
    session.register_host(&[NATIVE_JOB_TYPE]);

    session.start(131);
    let mut answers = vec![OHAND_JOB_CAPABILITY_PERMANENT];
    loop {
        let command = session.command();
        if command.command == OHAND_JOB_HOST_COMMAND_SEND {
            session.complete(command.request_id, 200, &anthropic_reply());
            continue;
        }
        let outcome = answers.pop().unwrap();
        assert_eq!(
            session
                .finish(command.request_id, outcome, "audio_unreadable")
                .status,
            OHAND_CORE_STATUS_OK
        );
        break;
    }
    let summary = payload_of(&session.event(131));
    assert!(summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["result"] == "failed_permanently"));
    let job = session.job(NATIVE_JOB_ID);
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.failure_reason.as_deref(), Some("audio_unreadable"));
    assert_eq!(
        session.finish(1, 9, "").code(),
        "invalid_request",
        "unknown outcomes are refused"
    );
    session.close();
}

#[test]
fn a_job_type_nobody_registered_is_deferred_not_failed_and_never_sent_to_the_host() {
    let _guard = serial();
    let session = Session::open("unregistered", true);
    session.register_host(&[]);

    session.start(141);
    let send = session.command();
    assert_eq!(send.command, OHAND_JOB_HOST_COMMAND_SEND);
    session.complete(send.request_id, 200, &anthropic_reply());
    let summary = payload_of(&session.event(141));
    let native = summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_type"] == NATIVE_JOB_TYPE)
        .expect("the unregistered job was reached");
    assert_eq!(native["result"], "capability_unavailable");
    session.assert_no_command();
    let job = session.job(NATIVE_JOB_ID);
    assert_eq!(job.status, JobStatus::Queued);
    session.close();
}

#[test]
fn labels_helpers_and_names_are_conservative() {
    assert_eq!(sanitized_reason("audio_unreadable"), "audio_unreadable");
    assert_eq!(sanitized_reason("call the roofer"), FALLBACK_REASON);
    assert_eq!(sanitized_reason(&"a".repeat(65)), FALLBACK_REASON);
    assert!(is_valid_job_type("transcribe"));
    assert!(!is_valid_job_type("interpret"));
    assert!(!is_valid_job_type("Transcribe"));
    assert_eq!(
        origin_of("HTTPS://API.anthropic.com:443/v1"),
        None,
        "the scheme must be lowercase https"
    );
    assert_eq!(
        origin_of("https://API.anthropic.com:443/v1/messages").as_deref(),
        Some(ANTHROPIC_ORIGIN)
    );
    assert_eq!(origin_of("https://user@api.anthropic.com/"), None);
    assert_eq!(origin_of("http://api.anthropic.com/"), None);
    assert!(parse_transport_error("rate_limited").is_some());
    assert!(parse_transport_error("RateLimited").is_none());
}

fn set_reachable(session: &Session, reachable: bool) -> Outcome {
    consume(ohand_core_set_job_network_reachable(
        session.handle,
        u32::from(reachable),
    ))
}

#[test]
fn offline_a_provider_job_is_deferred_without_cost_and_native_jobs_still_run() {
    let _guard = serial();
    let session = Session::open("offline-native", true);
    session.register_host(&[NATIVE_JOB_TYPE]);
    assert_eq!(set_reachable(&session, false).status, OHAND_CORE_STATUS_OK);

    session.start(121);
    let run = session.command();
    assert_eq!(run.command, OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY);
    let body: Value = serde_json::from_slice(&run.data).unwrap();
    assert_eq!(body["job_id"], NATIVE_JOB_ID);
    let attempt = body["attempt"].as_i64().unwrap() as i32;
    complete_job(&mut session.database(), NATIVE_JOB_ID, attempt).unwrap();
    session.finish(run.request_id, OHAND_JOB_CAPABILITY_SETTLED, "");
    let summary = payload_of(&session.event(121));
    assert_eq!(summary["stop"], "idle", "{summary}");
    let provider = summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == JOB_ID)
        .expect("the provider job is reported");
    assert_eq!(provider["settlement"], "backed_off");
    session.assert_no_command();

    let deferred = session.job(JOB_ID);
    assert_eq!(deferred.status, JobStatus::Queued);
    assert_eq!(deferred.attempt_count, 1, "only the claim, no failure");
    assert_eq!(
        deferred.failure_reason.as_deref(),
        Some(OFFLINE_DEFERRED_REASON)
    );
    assert_eq!(session.job(NATIVE_JOB_ID).status, JobStatus::Completed);

    set_reachable(&session, true);
    wait_for_drain_to_end(session.handle);
    session.start(122);
    let send = session.command();
    assert_eq!(send.command, OHAND_JOB_HOST_COMMAND_SEND);
    session.complete(send.request_id, 200, &anthropic_reply());
    let summary = payload_of(&session.event(122));
    assert_eq!(only_job(&summary)["settlement"], "completed", "{summary}");
    assert_eq!(session.job(JOB_ID).status, JobStatus::Completed);
    session.close();
}

#[test]
fn the_reachability_flag_needs_a_registered_host() {
    let _guard = serial();
    let session = Session::open("offline-no-host", false);
    assert_eq!(
        set_reachable(&session, false).status,
        failures::HOST_NOT_REGISTERED.status()
    );
    session.close();
}

#[test]
fn a_host_that_settled_the_job_as_failed_is_reported_as_failed() {
    let _guard = serial();
    let session = Session::open("settled-failed", true);
    set_reachable_after_register(&session);

    session.start(131);
    loop {
        let command = session.command();
        match command.command {
            OHAND_JOB_HOST_COMMAND_SEND => {
                session.complete(command.request_id, 200, &anthropic_reply());
            }
            OHAND_JOB_HOST_COMMAND_RUN_CAPABILITY => {
                session
                    .database()
                    .conn()
                    .execute(
                        "UPDATE jobs SET status = 'failed', lease_expires_at = NULL \
                         WHERE job_id = ?",
                        [NATIVE_JOB_ID],
                    )
                    .unwrap();
                session.finish(command.request_id, OHAND_JOB_CAPABILITY_SETTLED, "");
                break;
            }
            other => panic!("unexpected command {other}"),
        }
    }
    let summary = payload_of(&session.event(131));
    let native = summary["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["job_id"] == NATIVE_JOB_ID)
        .unwrap();
    assert_eq!(native["settlement"], "failed", "{summary}");
    session.close();
}

fn set_reachable_after_register(session: &Session) {
    session.register_host(&[NATIVE_JOB_TYPE]);
    assert_eq!(set_reachable(session, true).status, OHAND_CORE_STATUS_OK);
}
