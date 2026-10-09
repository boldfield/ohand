//! Drives the provider exchange exports the way the native layer does, against the real core
//! store and the real provider adapters, with a recording stand-in for the native transport.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::providers::contracts::{
    CapabilityMetadata, ProviderCapability, ProviderProfileBuilder, StructuredOutputMode,
};
use crate::store::schema::Database;
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const OPENAI_ORIGIN: &str = "https://api.openai.com";
const CREDENTIAL_REFERENCE: &str = "credential-ref/synthetic-primary";
const NOTE_TEXT: &str = "call the roofer tomorrow at 9";
const CAPTURE_ID: &str = "5a1c0000-0000-4000-8000-0000000000c1";
const ITEM_ID: &str = "5a1c0000-0000-4000-8000-0000000000e1";
const REQUEST_VERSION: &str = "5a1c0000-0000-4000-8000-0000000000a1";
const JOB_ID: &str = "job-synthetic-1";
const ROUTE_ID: &str = "route-synthetic";

struct Command {
    operation_id: u64,
    command: u32,
    data: Vec<u8>,
    thread: std::thread::ThreadId,
}

struct NativeSide {
    sender: Mutex<std::sync::mpsc::Sender<Command>>,
}

unsafe extern "C" fn record_command(
    context: *mut c_void,
    operation_id: u64,
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
        operation_id,
        command,
        data,
        thread: std::thread::current().id(),
    });
}

fn temporary_store(name: &str) -> (std::path::PathBuf, String) {
    let directory =
        std::env::temp_dir().join(format!("ohand-provider-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("core.sqlite").to_string_lossy().into_owned();
    (directory, path)
}

fn profile_for(protocol: ProviderProtocol, origin: &str) -> ProviderProfile {
    ProviderProfileBuilder::new("synthetic-profile", protocol, "synthetic-model")
        .credential_ref(CREDENTIAL_REFERENCE)
        .timeout_seconds(30)
        .authorized_destination(origin)
        .capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "ffi-fixtures:provider_transport",
            )
            .with_input_size_limit(500)
            .with_structured_output(StructuredOutputMode::JsonSchema),
        )
        .build()
        .expect("valid profile")
}

#[derive(Clone)]
struct Scenario {
    profile: ProviderProfile,
    route_destinations: Vec<String>,
    authorized_destinations: Vec<String>,
    revoked: bool,
    item_revision: i64,
    text: Option<&'static str>,
}

impl Scenario {
    fn anthropic() -> Scenario {
        Scenario::over(profile_for(ProviderProtocol::Anthropic, ANTHROPIC_ORIGIN))
    }

    fn openai() -> Scenario {
        Scenario::over(profile_for(ProviderProtocol::OpenAi, OPENAI_ORIGIN))
    }

    fn over(profile: ProviderProfile) -> Scenario {
        let origins = profile.authorized_destinations().to_vec();
        Scenario {
            profile,
            route_destinations: origins.clone(),
            authorized_destinations: origins,
            revoked: false,
            item_revision: 0,
            text: Some(NOTE_TEXT),
        }
    }
}

/// Stands in for the import and profile installation, which create these rows in production.
fn seed(path: &str, scenario: &Scenario) {
    let mut database =
        Database::open(path, std::sync::Arc::new(crate::store::schema::SystemClock)).unwrap();
    let transaction = database.transaction().unwrap();
    let record = serde_json::to_value(&scenario.profile).unwrap();
    let text_of = |key: &str| record[key].to_string();
    let revoked_at = scenario.revoked.then_some("2026-10-08T09:40:00Z");
    transaction
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, \
             capabilities, created_at, revoked_at) VALUES (?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                scenario.profile.profile_version(),
                scenario.profile.profile_id(),
                record["protocol"].as_str().unwrap(),
                scenario.profile.model(),
                CREDENTIAL_REFERENCE,
                scenario.profile.timeout_seconds(),
                text_of("retry_policy"),
                text_of("authorized_destinations"),
                text_of("capabilities"),
                record["created_at"].as_str().unwrap(),
                revoked_at,
            ],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO captures (capture_id, text, audio_reference, capture_instant, \
             timezone_id, utc_offset_minutes, locale, calendar, item_scope, route_id, \
             entry_locked, created_at) VALUES (?, ?, ?, '2026-10-08T09:30:00Z', \
             'America/Chicago', -300, 'en_US', 'gregorian', 'personal', ?, 0, \
             '2026-10-08T09:30:01Z')",
            rusqlite::params![
                CAPTURE_ID,
                scenario.text,
                scenario.text.is_none().then_some("staging/synthetic.m4a"),
                ROUTE_ID
            ],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, \
             sync_state, processing_state, transcription_state, created_at, updated_at) \
             VALUES (?, ?, ?, 'active', 'saved_local', 'not_configured', 'unprocessed', \
             'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')",
            rusqlite::params![ITEM_ID, CAPTURE_ID, scenario.item_revision],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
             created_at) VALUES (?, 'synthetic', 'personal', ?, '2026-10-08T09:00:00Z')",
            rusqlite::params![
                ROUTE_ID,
                serde_json::to_string(&scenario.route_destinations).unwrap()
            ],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO route_authorizations (route_id, capability, authorized_destinations, \
             created_at) VALUES (?, 'text_interpretation', ?, '2026-10-08T09:00:00Z')",
            rusqlite::params![
                ROUTE_ID,
                serde_json::to_string(&scenario.authorized_destinations).unwrap()
            ],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
             profile_version, request_version, status, attempt_count, created_at) \
             VALUES (?, 1, ?, 'interpret', 0, ?, ?, 'queued', 0, '2026-10-08T09:30:02Z')",
            rusqlite::params![
                JOB_ID,
                ITEM_ID,
                scenario.profile.profile_version(),
                REQUEST_VERSION
            ],
        )
        .unwrap();
    transaction.commit().unwrap();
}

struct Session {
    handle: OhandCoreHandle,
    events: Receiver<Event>,
    commands: Receiver<Command>,
    native_context: Box<NativeSide>,
    _recorder: Box<Recorder>,
    directory: std::path::PathBuf,
}

impl Session {
    fn open(name: &str, scenario: &Scenario) -> Session {
        let (directory, path) = temporary_store(name);
        seed(&path, scenario);
        let mut handle = 0;
        let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        let (sender, commands) = std::sync::mpsc::channel();
        Session {
            handle,
            events,
            commands,
            native_context: Box::new(NativeSide {
                sender: Mutex::new(sender),
            }),
            _recorder: recorder,
            directory,
        }
    }

    fn register_native(&self) {
        let context = &*self.native_context as *const NativeSide as *mut c_void;
        let outcome = consume(ohand_core_set_provider_transport(
            self.handle,
            Some(record_command),
            context,
        ));
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    }

    fn clear_native(&self) {
        let outcome = consume(ohand_core_set_provider_transport(
            self.handle,
            None,
            std::ptr::null_mut(),
        ));
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    }

    fn start_raw(&self, operation_id: u64, request: &[u8]) -> Outcome {
        consume(unsafe {
            ohand_core_start_provider_exchange(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        })
    }

    fn start(&self, operation_id: u64, job_id: &str) -> Outcome {
        self.start_raw(
            operation_id,
            &serde_json::to_vec(&json!({ "job_id": job_id })).unwrap(),
        )
    }

    fn event(&self, operation_id: u64) -> Event {
        let event = self.events.recv_timeout(WAIT).expect("an event");
        assert_eq!(event.operation_id, operation_id);
        event
    }

    fn command(&self, operation_id: u64) -> Command {
        let command = self.commands.recv_timeout(WAIT).expect("a native command");
        assert_eq!(command.operation_id, operation_id);
        command
    }

    fn assert_no_command(&self) {
        assert!(self
            .commands
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    fn complete(&self, operation_id: u64, status: u32, body: &[u8]) -> Outcome {
        let headers = br#"[["content-type","application/json"]]"#;
        consume(unsafe {
            ohand_core_complete_provider_exchange(
                self.handle,
                operation_id,
                status,
                headers.as_ptr(),
                headers.len(),
                body.as_ptr(),
                body.len(),
            )
        })
    }

    fn fail(&self, operation_id: u64, name: &str) -> Outcome {
        consume(unsafe {
            ohand_core_fail_provider_exchange(self.handle, operation_id, name.as_ptr(), name.len())
        })
    }

    fn cancel(&self, operation_id: u64) -> Outcome {
        consume(ohand_core_cancel_provider_exchange(
            self.handle,
            operation_id,
        ))
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
        let _ = ohand_core_set_provider_transport(self.handle, None, std::ptr::null_mut());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn payload_of(event: &Event) -> Value {
    serde_json::from_slice(&event.payload).unwrap()
}

fn failure_of(event: &Event) -> (u32, String, String) {
    assert_ne!(
        event.status,
        OHAND_CORE_STATUS_OK,
        "{:?}",
        payload_of(event)
    );
    let value = payload_of(event);
    (
        event.status,
        value["class"].as_str().unwrap().to_string(),
        value["code"].as_str().unwrap().to_string(),
    )
}

fn send_description(command: &Command) -> Value {
    assert_eq!(command.command, OHAND_PROVIDER_COMMAND_SEND);
    serde_json::from_slice(&command.data).unwrap()
}

fn wait_until_idle(handle: u64) {
    let deadline = Instant::now() + WAIT;
    while lock(&EXCHANGES).keys().any(|(owner, _)| *owner == handle) {
        assert!(Instant::now() < deadline, "exchange slot was not released");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn anthropic_reply(proposal: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "msg_synthetic_01",
        "type": "message",
        "role": "assistant",
        "model": "synthetic-model",
        "content": [{
            "type": "tool_use",
            "id": "toolu_synthetic_01",
            "name": "interpret",
            "input": proposal,
        }],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 42, "output_tokens": 17 },
    }))
    .unwrap()
}

fn valid_proposal() -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [{ "start": 0, "end": 4 }],
    })
}

#[test]
fn a_valid_job_is_sent_to_the_pinned_destination_off_the_worker_and_completes() {
    let _guard = serial();
    let scenario = Scenario::anthropic();
    let session = Session::open("valid", &scenario);
    session.register_native();

    assert_eq!(session.start(11, JOB_ID).status, OHAND_CORE_STATUS_OK);
    let dispatched = session.event(11);
    assert_eq!(dispatched.status, OHAND_CORE_STATUS_OK);
    assert_eq!(
        payload_of(&dispatched),
        json!({ "operation_id": 11, "phase": "dispatched" })
    );

    let command = session.command(11);
    assert_ne!(
        command.thread, dispatched.thread,
        "never on the core worker"
    );
    let send = send_description(&command);
    assert_eq!(send["operation_id"], 11);
    assert_eq!(send["url"], "https://api.anthropic.com/v1/messages");
    assert_eq!(send["method"], "POST");
    assert_eq!(send["authorized_origins"], json!([ANTHROPIC_ORIGIN]));
    assert_eq!(
        send["credential"],
        json!({ "reference": CREDENTIAL_REFERENCE, "header": "x-api-key", "scheme": null })
    );
    assert_eq!(
        send["max_response_bytes"],
        DEFAULT_MAX_RESPONSE_BYTES_FOR_TEST
    );
    assert!(send["timeout_ms"].as_u64().unwrap() > 0);
    let body = send["body"].as_str().unwrap();
    assert!(body.contains(NOTE_TEXT));
    assert!(body.contains("synthetic-model"));
    assert!(!command.data.is_empty());
    let header_names: Vec<String> = send["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| pair[0].as_str().unwrap().to_ascii_lowercase())
        .collect();
    assert!(header_names.contains(&"anthropic-version".to_string()));
    assert!(
        !header_names.contains(&"x-api-key".to_string()),
        "the core never places a secret in the request"
    );
    assert!(!String::from_utf8_lossy(&command.data).contains(ROUTE_ID));

    let answer = session.complete(11, 200, &anthropic_reply(valid_proposal()));
    assert_eq!(answer.status, OHAND_CORE_STATUS_OK);
    let completed = session.event(11);
    assert_eq!(
        completed.status, OHAND_CORE_STATUS_OK,
        "{:?}",
        completed.payload
    );
    let payload = payload_of(&completed);
    assert_eq!(payload["phase"], "completed");
    assert_eq!(payload["operation_id"], 11);
    assert_eq!(payload["output"]["proposal"], valid_proposal());

    wait_until_idle(session.handle);
    assert_eq!(session.complete(11, 200, b"{}").code(), "not_found");
    session.assert_no_command();
    session.close();
}

#[test]
fn an_openai_job_attaches_its_credential_as_a_bearer_reference() {
    let _guard = serial();
    let scenario = Scenario::openai();
    let session = Session::open("openai", &scenario);
    session.register_native();

    assert_eq!(session.start(21, JOB_ID).status, OHAND_CORE_STATUS_OK);
    assert_eq!(session.event(21).status, OHAND_CORE_STATUS_OK);
    let send = send_description(&session.command(21));
    assert!(send["url"]
        .as_str()
        .unwrap()
        .starts_with("https://api.openai.com/"));
    assert_eq!(
        send["credential"],
        json!({
            "reference": CREDENTIAL_REFERENCE,
            "header": "Authorization",
            "scheme": "Bearer",
        })
    );
    assert_eq!(send["authorized_origins"], json!([OPENAI_ORIGIN]));

    assert_eq!(
        session.complete(21, 401, b"{}").status,
        OHAND_CORE_STATUS_OK
    );
    let rejected = session.event(21);
    let (status, class, _) = failure_of(&rejected);
    assert_eq!((status, class.as_str()), (3, "unauthorized"));
    session.close();
}

#[test]
fn provider_http_and_native_failures_keep_the_provider_error_contract() {
    let _guard = serial();
    let session = Session::open("mapping", &Scenario::anthropic());
    session.register_native();

    let cases: [(u64, &str, &str); 3] = [
        (1, "rate_limited", "transient"),
        (2, "unavailable", "transient"),
        (3, "timeout", "transient"),
    ];
    for (operation_id, name, class) in cases {
        assert_eq!(
            session.start(operation_id, JOB_ID).status,
            OHAND_CORE_STATUS_OK
        );
        session.event(operation_id);
        session.command(operation_id);
        assert_eq!(
            session.fail(operation_id, name).status,
            OHAND_CORE_STATUS_OK
        );
        let (_, reported_class, code) = failure_of(&session.event(operation_id));
        assert_eq!((reported_class.as_str(), code.as_str()), (class, name));
        wait_until_idle(session.handle);
    }

    assert_eq!(session.start(4, JOB_ID).status, OHAND_CORE_STATUS_OK);
    session.event(4);
    session.command(4);
    session.complete(4, 200, b"<html>gateway</html>");
    let (_, class, code) = failure_of(&session.event(4));
    assert_eq!(
        (class.as_str(), code.as_str()),
        ("permanent", "invalid_output")
    );
    wait_until_idle(session.handle);

    assert_eq!(session.fail(5, "timeout").code(), "not_found");
    session.start(6, JOB_ID);
    session.event(6);
    session.command(6);
    assert_eq!(
        session.fail(6, "not-a-transport-error").code(),
        "invalid_request"
    );
    assert_eq!(session.fail(6, "").code(), "invalid_request");
    session.fail(6, "unavailable");
    session.event(6);
    session.close();
}

#[test]
fn cancellation_reaches_native_and_discards_a_late_answer() {
    let _guard = serial();
    let session = Session::open("cancel", &Scenario::anthropic());
    session.register_native();

    session.start(31, JOB_ID);
    assert_eq!(session.event(31).status, OHAND_CORE_STATUS_OK);
    assert_eq!(session.command(31).command, OHAND_PROVIDER_COMMAND_SEND);

    assert_eq!(session.cancel(31).status, OHAND_CORE_STATUS_OK);
    let cancel = session.command(31);
    assert_eq!(cancel.command, OHAND_PROVIDER_COMMAND_CANCEL);
    assert!(cancel.data.is_empty());
    let (status, class, code) = failure_of(&session.event(31));
    assert_eq!(
        (status, class.as_str(), code.as_str()),
        (4, "cancelled", "cancelled")
    );

    wait_until_idle(session.handle);
    assert_eq!(
        session
            .complete(31, 200, &anthropic_reply(valid_proposal()))
            .code(),
        "not_found"
    );
    assert_eq!(
        session.cancel(31).status,
        OHAND_CORE_STATUS_OK,
        "idempotent"
    );
    assert_eq!(session.cancel(9999).status, OHAND_CORE_STATUS_OK);
    session.close();
}

#[test]
fn a_cancel_racing_the_answer_never_yields_a_completed_event() {
    let _guard = serial();
    let session = Session::open("race", &Scenario::anthropic());
    session.register_native();

    session.start(41, JOB_ID);
    session.event(41);
    session.command(41);
    session.cancel(41);
    let _ = session.complete(41, 200, &anthropic_reply(valid_proposal()));
    let (status, _, code) = failure_of(&session.event(41));
    assert_eq!((status, code.as_str()), (4, "cancelled"));
    session.close();
}

#[test]
fn denied_and_revoked_work_never_reaches_native() {
    let _guard = serial();
    let mut off_route = Scenario::anthropic();
    off_route.route_destinations = vec![OPENAI_ORIGIN.to_string()];
    off_route.authorized_destinations = vec![OPENAI_ORIGIN.to_string()];
    let mut unauthorized_capability = Scenario::anthropic();
    unauthorized_capability.authorized_destinations = vec![OPENAI_ORIGIN.to_string()];
    let mut revoked = Scenario::anthropic();
    revoked.revoked = true;

    let cases: [(&str, Scenario, &str, u32); 3] = [
        ("off-route", off_route, "destination_not_in_route", 3),
        (
            "capability",
            unauthorized_capability,
            "destination_not_authorized",
            3,
        ),
        ("revoked", revoked, "profile_revoked", 3),
    ];
    for (name, scenario, expected_code, expected_status) in cases {
        let session = Session::open(name, &scenario);
        session.register_native();
        assert_eq!(session.start(51, JOB_ID).status, OHAND_CORE_STATUS_OK);
        let denial = session.event(51);
        let (status, _, code) = failure_of(&denial);
        assert_eq!(
            (status, code.as_str()),
            (expected_status, expected_code),
            "{name}"
        );
        session.assert_no_command();
        wait_until_idle(session.handle);
        session.close();
    }
}

#[test]
fn a_job_that_cannot_form_a_request_is_refused_before_sending() {
    let _guard = serial();
    let mut stale = Scenario::anthropic();
    stale.item_revision = 3;
    let mut textless = Scenario::anthropic();
    textless.text = None;
    let cases: [(&str, Scenario, &str); 2] = [
        ("stale", stale, "stale_source"),
        ("textless", textless, "no_text"),
    ];
    for (name, scenario, expected_code) in cases {
        let session = Session::open(name, &scenario);
        session.register_native();
        session.start(61, JOB_ID);
        let (status, class, code) = failure_of(&session.event(61));
        assert_eq!(
            (class.as_str(), code.as_str()),
            ("permanent", expected_code),
            "{name}"
        );
        assert_eq!(status, 2);
        session.assert_no_command();
        session.close();
    }
}

#[test]
fn start_rejects_bad_requests_unknown_jobs_and_missing_transport() {
    let _guard = serial();
    let session = Session::open("rejects", &Scenario::anthropic());

    let missing = session.start(71, JOB_ID);
    assert_eq!(
        (missing.status, missing.code().as_str()),
        (5, "transport_not_registered")
    );
    session.register_native();

    assert_eq!(session.start_raw(72, b"").code(), "invalid_request");
    assert_eq!(session.start_raw(72, b"{").code(), "invalid_request");
    assert_eq!(
        session
            .start_raw(72, br#"{"job_id":"j","url":"https://evil.example"}"#)
            .code(),
        "invalid_request",
        "a caller cannot name a destination"
    );
    assert_eq!(
        session.start_raw(72, &vec![b' '; 4097]).code(),
        "request_too_large"
    );
    assert_eq!(
        consume(unsafe {
            ohand_core_start_provider_exchange(session.handle, 72, std::ptr::null(), 4)
        })
        .code(),
        "null_argument"
    );
    assert_eq!(
        consume(unsafe { ohand_core_start_provider_exchange(0, 72, b"{}".as_ptr(), 2) }).code(),
        "invalid_handle"
    );

    assert_eq!(
        session.start(73, "no-such-job").status,
        OHAND_CORE_STATUS_OK
    );
    let (status, _, code) = failure_of(&session.event(73));
    assert_eq!((status, code.as_str()), (2, "not_found"));
    session.assert_no_command();
    session.close();
}

#[test]
fn an_operation_id_cannot_be_reused_while_it_is_running() {
    let _guard = serial();
    let session = Session::open("duplicate", &Scenario::anthropic());
    session.register_native();

    session.start(81, JOB_ID);
    session.event(81);
    session.command(81);
    let duplicate = session.start(81, JOB_ID);
    assert_eq!(duplicate.code(), "duplicate_operation");
    session.fail(81, "unavailable");
    session.event(81);
    wait_until_idle(session.handle);

    assert_eq!(session.start(81, JOB_ID).status, OHAND_CORE_STATUS_OK);
    session.event(81);
    session.command(81);
    session.fail(81, "unavailable");
    session.event(81);
    session.close();
}

#[test]
fn clearing_the_transport_cancels_running_exchanges() {
    let _guard = serial();
    let session = Session::open("clear", &Scenario::anthropic());
    session.register_native();

    session.start(91, JOB_ID);
    session.event(91);
    session.command(91);
    session.clear_native();
    let (status, _, code) = failure_of(&session.event(91));
    assert_eq!((status, code.as_str()), (4, "cancelled"));
    wait_until_idle(session.handle);

    assert_eq!(session.start(92, JOB_ID).code(), "transport_not_registered");
    session.close();
}

#[test]
fn closing_the_core_ends_a_running_exchange_and_releases_its_slot() {
    let _guard = serial();
    let session = Session::open("close", &Scenario::anthropic());
    session.register_native();

    session.start(101, JOB_ID);
    session.event(101);
    session.command(101);
    let handle = session.handle;
    assert_eq!(
        consume(ohand_core_close(handle)).status,
        OHAND_CORE_STATUS_OK
    );
    wait_until_idle(handle);
    assert!(lock(&TRANSPORTS).contains_key(&handle));
    session.clear_native();
    assert!(!lock(&TRANSPORTS).contains_key(&handle));
    let _ = std::fs::remove_dir_all(&session.directory);
}

#[test]
fn the_registry_is_empty_after_every_scenario() {
    let _guard = serial();
    assert!(lock(&EXCHANGES).is_empty());
}

#[test]
fn a_destination_outside_the_authorized_origins_is_refused_before_native_is_called() {
    let _guard = serial();
    let (sender, commands) = std::sync::mpsc::channel();
    let native = Box::new(NativeSide {
        sender: Mutex::new(sender),
    });
    let registration = Arc::new(Registration {
        callback: record_command,
        context: &*native as *const NativeSide as usize,
        active: Mutex::new(true),
    });
    let (_answers, receiver) = channel();
    let transport = NativeTransport {
        end: Arc::new(ExchangeEnd {
            handle: 0,
            operation_id: 1,
            registration,
            receiver: Mutex::new(receiver),
            authorized_origins: vec![ANTHROPIC_ORIGIN.to_string()],
        }),
    };
    let cancel = CancelToken::new();
    for url in [
        "https://api.anthropic.com.evil.example/v1/messages",
        "https://evil.example/v1/messages",
        "http://api.anthropic.com/v1/messages",
        "https://user@api.anthropic.com/v1/messages",
        "https://api.anthropic.com:8443/v1/messages",
    ] {
        let outgoing = Outgoing {
            url,
            headers: Vec::new(),
            body: b"{}",
            credential: None,
            timeout_ms: 1000,
            max_response_bytes: 1024,
        };
        assert_eq!(
            transport.exchange(outgoing, &cancel).unwrap_err(),
            TransportError::Rejected,
            "{url}"
        );
    }
    assert!(commands.recv_timeout(Duration::from_millis(100)).is_err());
    cancel.cancel();
    let outgoing = Outgoing {
        url: "https://api.anthropic.com/v1/messages",
        headers: Vec::new(),
        body: b"{}",
        credential: None,
        timeout_ms: 1000,
        max_response_bytes: 1024,
    };
    assert_eq!(
        transport.exchange(outgoing, &cancel).unwrap_err(),
        TransportError::Cancelled
    );
    assert!(commands.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn origins_are_compared_by_parsed_origin_not_by_prefix() {
    assert_eq!(
        origin_of("https://API.Anthropic.com:443/v1/messages").as_deref(),
        Some("https://api.anthropic.com")
    );
    assert_eq!(origin_of("http://api.anthropic.com"), None);
    assert_eq!(origin_of("https://a@api.anthropic.com"), None);
    assert_eq!(origin_of("https:// api.anthropic.com"), None);
    assert_eq!(origin_of("https://"), None);
}

#[test]
fn native_failure_names_are_the_snake_case_transport_errors() {
    assert!(parse_transport_error("rate_limited").is_some());
    assert!(parse_transport_error("RateLimited").is_none());
}

const DEFAULT_MAX_RESPONSE_BYTES_FOR_TEST: u64 =
    crate::providers::contracts::DEFAULT_MAX_RESPONSE_BYTES as u64;
