//! Drives the connection check export the way the native layer does, against the real core
//! store, the real provider adapters and a recording stand-in for the native transport.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::privacy::routing::{grant_route_capability, ProcessingCapability};
use crate::providers::profiles::{create_profile_version, ProfileDraft};
use crate::providers::self_hosted::CHAT_VERIFIED_MODEL;
use crate::store::schema::Database;
use serde_json::{json, Value};

const ANTHROPIC_ORIGIN: &str = "https://api.anthropic.com";
const OPENAI_ORIGIN: &str = "https://api.openai.com";
const SELF_HOSTED_ORIGIN: &str = "https://spark.example.test";
const SELF_HOSTED_ENDPOINT: &str = "https://Spark.Example.Test/v1/chat";
const CREDENTIAL_REFERENCE: &str = "credential-ref/synthetic-check";
const MODEL: &str = "synthetic-check-model";
const NOTE_TEXT: &str = "call the roofer tomorrow at 9";
const CAPTURE_ID: &str = "5a1c0000-0000-4000-8000-0000000000c1";
const ITEM_ID: &str = "5a1c0000-0000-4000-8000-0000000000e1";
const ROUTE_ID: &str = "route-synthetic";

const ALL_OUTCOMES: [&str; 12] = [
    "ok",
    "credential_missing",
    "credential_rejected",
    "unreachable",
    "tls_failure",
    "timeout",
    "capability_mismatch",
    "protocol_not_enabled",
    "rate_limited",
    "provider_error",
    "unexpected_response",
    "cancelled",
];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Anthropic,
    OpenAi,
    SelfHosted,
}

impl Kind {
    fn draft(self, model: &str, capabilities: &[&str]) -> ProfileDraft {
        ProfileDraft {
            profile_id: format!("synthetic-check-profile-{}", self.key()),
            protocol: self.key().to_string(),
            endpoint: (self == Kind::SelfHosted).then(|| SELF_HOSTED_ENDPOINT.to_string()),
            model: model.to_string(),
            credential_ref: Some(CREDENTIAL_REFERENCE.to_string()),
            timeout_seconds: 30,
            retry_policy: None,
            capabilities: capabilities.iter().map(|key| key.to_string()).collect(),
        }
    }

    /// A model the profile's protocol accepts for text interpretation.
    fn serving_model(self) -> &'static str {
        match self {
            Kind::SelfHosted => CHAT_VERIFIED_MODEL,
            Kind::Anthropic | Kind::OpenAi => MODEL,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Kind::Anthropic => "anthropic",
            Kind::OpenAi => "open_ai",
            Kind::SelfHosted => "self_hosted",
        }
    }

    fn origin(self) -> &'static str {
        match self {
            Kind::Anthropic => ANTHROPIC_ORIGIN,
            Kind::OpenAi => OPENAI_ORIGIN,
            Kind::SelfHosted => SELF_HOSTED_ORIGIN,
        }
    }

    fn reply(self) -> Vec<u8> {
        let proposal = json!({
            "operation": { "kind": "annotate" },
            "item_type": "action",
            "source_spans": [{ "start": 0, "end": 4 }],
        });
        let reply = match self {
            Kind::Anthropic => json!({
                "id": "msg_synthetic_01",
                "type": "message",
                "role": "assistant",
                "model": MODEL,
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_synthetic_01",
                    "name": "interpret",
                    "input": proposal,
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 42, "output_tokens": 17 },
            }),
            Kind::OpenAi | Kind::SelfHosted => json!({
                "id": "chatcmpl-synthetic-01",
                "object": "chat.completion",
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "finish_reason": "stop",
                    "message": { "role": "assistant", "content": proposal.to_string() },
                }],
            }),
        };
        serde_json::to_vec(&reply).unwrap()
    }
}

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

fn payload_of(event: &Event) -> Value {
    serde_json::from_slice(&event.payload).unwrap()
}

fn wait_until_idle(handle: u64) {
    let deadline = Instant::now() + WAIT;
    while lock(&EXCHANGES).keys().any(|(owner, _)| *owner == handle) {
        assert!(Instant::now() < deadline, "exchange slot was not released");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The expected outcome and how the fake native side answers the one send.
type Case = (&'static str, fn(&Session, u64));

const SNAPSHOT_TABLES: [&str; 9] = [
    "captures",
    "items",
    "jobs",
    "routes",
    "route_authorizations",
    "proposals",
    "events",
    "reminders",
    "provider_profiles",
];

struct Session {
    handle: OhandCoreHandle,
    path: String,
    events: std::sync::mpsc::Receiver<Event>,
    commands: std::sync::mpsc::Receiver<Command>,
    native_context: Box<NativeSide>,
    _recorder: Box<Recorder>,
    directory: std::path::PathBuf,
    profile_version: String,
}

impl Session {
    /// Stores one profile of `kind` and a capture whose text must never reach a check. No route
    /// authorizes the profile: a check must not need one.
    fn open(name: &str, kind: Kind) -> Session {
        Session::open_model(name, kind, kind.serving_model())
    }

    fn open_model(name: &str, kind: Kind, model: &str) -> Session {
        let directory = std::env::temp_dir().join(format!(
            "ohand-connection-check-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        let profile_version = {
            let mut database =
                Database::open(&path, Arc::new(crate::store::schema::SystemClock)).unwrap();
            let stored = create_profile_version(
                &mut database,
                &kind.draft(model, &["text_interpretation"]),
                Utc::now(),
            )
            .unwrap();
            let transaction = database.transaction().unwrap();
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
                    "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, \
                     save_state, sync_state, processing_state, transcription_state, created_at, \
                     updated_at) VALUES (?, ?, 0, 'active', 'saved_local', 'not_configured', \
                     'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', \
                     '2026-10-08T09:30:01Z')",
                    rusqlite::params![ITEM_ID, CAPTURE_ID],
                )
                .unwrap();
            transaction.commit().unwrap();
            stored.profile_version
        };
        let mut handle = 0;
        let outcome = consume(unsafe { ohand_core_open(path.as_ptr(), path.len(), &mut handle) });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let (recorder, events) = recorder();
        register(handle, &recorder);
        if kind == Kind::SelfHosted {
            protocols::enable_for_test(handle, &["anthropic", "open_ai", "self_hosted"]);
            protocols::register_for_test(handle, &[(&profile_version, model)]);
        }
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
            profile_version,
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

    fn start_with(&self, operation_id: u64, request: &Value) -> Outcome {
        let request = serde_json::to_vec(request).unwrap();
        consume(unsafe {
            ohand_core_start_provider_connection_check(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        })
    }

    fn start(&self, operation_id: u64) -> Outcome {
        self.start_with(
            operation_id,
            &json!({ "profile_version": self.profile_version }),
        )
    }

    fn event(&self, operation_id: u64) -> Event {
        let event = self.events.recv_timeout(WAIT).expect("an event");
        assert_eq!(event.operation_id, operation_id);
        event
    }

    fn command(&self, operation_id: u64) -> Command {
        let command = match self.commands.recv_timeout(WAIT) {
            Ok(command) => command,
            Err(_) => {
                let pending = self.events.try_recv().ok().map(|event| event.payload);
                panic!("no native command for {operation_id}; next event: {pending:?}")
            }
        };
        assert_eq!(command.operation_id, operation_id);
        command
    }

    fn assert_no_command(&self) {
        assert!(self
            .commands
            .recv_timeout(Duration::from_millis(200))
            .is_err());
    }

    fn respond(&self, operation_id: u64, status: u32, body: &[u8]) {
        let headers = br#"[["content-type","application/json"]]"#;
        let outcome = consume(unsafe {
            ohand_core_complete_provider_exchange(
                self.handle,
                operation_id,
                status,
                headers.as_ptr(),
                headers.len(),
                body.as_ptr(),
                body.len(),
            )
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    }

    fn fail(&self, operation_id: u64, name: &str) {
        let outcome = consume(unsafe {
            ohand_core_fail_provider_exchange(self.handle, operation_id, name.as_ptr(), name.len())
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
    }

    /// Starts a check and returns the `dispatched` event's payload and the native send command.
    fn dispatch(&self, operation_id: u64) -> (Event, Value) {
        assert_eq!(self.start(operation_id).status, OHAND_CORE_STATUS_OK);
        let dispatched = self.event(operation_id);
        assert_eq!(dispatched.status, OHAND_CORE_STATUS_OK);
        assert_eq!(
            payload_of(&dispatched),
            json!({ "operation_id": operation_id, "phase": "dispatched" })
        );
        let command = self.command(operation_id);
        assert_eq!(command.command, OHAND_PROVIDER_COMMAND_SEND);
        let description = serde_json::from_slice(&command.data).unwrap();
        (dispatched, description)
    }

    fn outcome_of(&self, operation_id: u64) -> String {
        let event = self.event(operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK, "{:?}", event.payload);
        let payload = payload_of(&event);
        assert_eq!(payload["phase"], "completed");
        assert_eq!(payload["operation_id"], operation_id);
        assert_eq!(payload.as_object().unwrap().len(), 3, "{payload}");
        wait_until_idle(self.handle);
        let outcome = payload["outcome"].as_str().unwrap().to_string();
        assert!(ALL_OUTCOMES.contains(&outcome.as_str()), "{outcome}");
        outcome
    }

    /// Runs a check whose single request is answered by `answer`; returns the outcome code.
    fn check_answered(&self, operation_id: u64, answer: impl FnOnce(&Session, u64)) -> String {
        let (_, _) = self.dispatch(operation_id);
        answer(self, operation_id);
        self.outcome_of(operation_id)
    }

    fn snapshot(&self) -> Vec<String> {
        let connection = rusqlite::Connection::open(&self.path).unwrap();
        let mut rows = Vec::new();
        for table in SNAPSHOT_TABLES {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
                .unwrap();
            let columns = statement.column_count();
            let mapped = statement
                .query_map([], |row| {
                    let mut text = String::new();
                    for column in 0..columns {
                        let value: rusqlite::types::Value = row.get(column)?;
                        text.push_str(&format!("{value:?}|"));
                    }
                    Ok(text)
                })
                .unwrap();
            rows.extend(mapped.map(|row| format!("{table}:{}", row.unwrap())));
        }
        rows
    }

    fn close(self) {
        assert_eq!(consume(ohand_core_close(self.handle)).status, 0);
        let _ = ohand_core_set_provider_transport(self.handle, None, std::ptr::null_mut());
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn failure_code(outcome: &Outcome) -> String {
    let value: Value = serde_json::from_slice(&outcome.payload).unwrap();
    value["code"].as_str().unwrap().to_string()
}

fn failure_event_code(event: &Event) -> String {
    payload_of(event)["code"].as_str().unwrap().to_string()
}

fn assert_redacted(text: &str) {
    for secret in [
        CREDENTIAL_REFERENCE,
        NOTE_TEXT,
        "x-api-key",
        "Authorization",
        "Bearer",
        "api.anthropic.com",
        "api.openai.com",
        "spark.example.test",
        MODEL,
        CHAT_VERIFIED_MODEL,
    ] {
        assert!(!text.contains(secret), "{secret} leaked into {text}");
    }
}

#[test]
fn a_healthy_provider_is_ok_and_the_request_is_fixed_synthetic_and_off_the_worker() {
    let _guard = serial();
    for kind in [Kind::Anthropic, Kind::OpenAi, Kind::SelfHosted] {
        let session = Session::open("ok", kind);
        session.register_native();
        let before = session.snapshot();

        let (dispatched, description) = session.dispatch(11);
        let body = description["body"].as_str().unwrap();
        assert!(body.contains(CHECK_TEXT), "the fixed text is the payload");
        for private in [NOTE_TEXT, CAPTURE_ID, ITEM_ID, ROUTE_ID] {
            assert!(!body.contains(private), "{private} reached the payload");
        }
        assert!(description["url"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .starts_with(kind.origin()));
        assert_eq!(description["authorized_origins"], json!([kind.origin()]));
        assert_eq!(description["capability"], "text_interpretation");
        assert_eq!(description["credential"]["reference"], CREDENTIAL_REFERENCE);
        assert_eq!(dispatched.operation_id, 11);

        session.respond(11, 200, &kind.reply());
        assert_eq!(session.outcome_of(11), "ok");
        assert_eq!(session.snapshot(), before, "a check writes nothing");
        session.assert_no_command();
        session.close();
    }
}

#[test]
fn the_send_runs_on_an_exchange_thread_never_the_worker() {
    let _guard = serial();
    let session = Session::open("thread", Kind::Anthropic);
    session.register_native();
    assert_eq!(session.start(12).status, OHAND_CORE_STATUS_OK);
    let dispatched = session.event(12);
    let command = session.command(12);
    assert_ne!(
        command.thread, dispatched.thread,
        "never on the core worker"
    );
    session.respond(12, 200, &Kind::Anthropic.reply());
    let completed = session.event(12);
    assert_eq!(payload_of(&completed)["outcome"], "ok");
    wait_until_idle(session.handle);
    session.close();
}

#[test]
fn each_outcome_is_reported_from_what_the_transport_returned() {
    let _guard = serial();
    let cases: [Case; 17] = [
        ("credential_missing", |s, op| s.fail(op, "unauthorized")),
        ("credential_rejected", |s, op| s.respond(op, 401, b"{}")),
        ("credential_rejected", |s, op| s.respond(op, 403, b"{}")),
        ("unreachable", |s, op| s.fail(op, "unavailable")),
        ("tls_failure", |s, op| s.fail(op, "rejected")),
        ("timeout", |s, op| s.fail(op, "timeout")),
        ("timeout", |s, op| s.respond(op, 408, b"{}")),
        ("capability_mismatch", |s, op| s.respond(op, 404, b"{}")),
        ("capability_mismatch", |s, op| s.respond(op, 400, b"{}")),
        ("rate_limited", |s, op| s.respond(op, 429, b"{}")),
        ("rate_limited", |s, op| s.fail(op, "rate_limited")),
        ("provider_error", |s, op| s.respond(op, 500, b"{}")),
        ("provider_error", |s, op| s.respond(op, 503, b"{}")),
        ("unexpected_response", |s, op| {
            s.respond(op, 200, b"<html>not a provider</html>")
        }),
        ("unexpected_response", |s, op| s.fail(op, "invalid_output")),
        ("provider_error", |s, op| s.respond(op, 502, b"{}")),
        ("ok", |s, op| s.respond(op, 200, &Kind::Anthropic.reply())),
    ];
    for (index, (expected, answer)) in cases.into_iter().enumerate() {
        let session = Session::open("outcome", Kind::Anthropic);
        session.register_native();
        let operation_id = 100 + index as u64;
        let actual = session.check_answered(operation_id, answer);
        assert_eq!(actual, expected, "case {index}");
        session.close();
    }
}

#[test]
fn every_protocol_classifies_the_same_http_and_native_answers() {
    let _guard = serial();
    for kind in [Kind::OpenAi, Kind::SelfHosted] {
        let cases: [Case; 6] = [
            ("credential_rejected", |s, op| s.respond(op, 401, b"{}")),
            ("credential_missing", |s, op| s.fail(op, "unauthorized")),
            ("unreachable", |s, op| s.fail(op, "unavailable")),
            ("tls_failure", |s, op| s.fail(op, "rejected")),
            ("timeout", |s, op| s.fail(op, "timeout")),
            ("capability_mismatch", |s, op| s.respond(op, 404, b"{}")),
        ];
        for (index, (expected, answer)) in cases.into_iter().enumerate() {
            let session = Session::open("protocols", kind);
            session.register_native();
            assert_eq!(
                session.check_answered(200 + index as u64, answer),
                expected,
                "{} case {index}",
                kind.key()
            );
            session.close();
        }
    }
}

#[test]
fn a_model_the_adapter_cannot_serve_is_a_capability_mismatch_and_sends_nothing() {
    let _guard = serial();
    let session = Session::open_model("capability", Kind::SelfHosted, MODEL);
    session.register_native();
    assert_eq!(session.start(21).status, OHAND_CORE_STATUS_OK);
    let dispatched = session.event(21);
    assert_eq!(dispatched.status, OHAND_CORE_STATUS_OK);
    assert_eq!(session.outcome_of(21), "capability_mismatch");
    session.assert_no_command();
    session.close();
}

#[test]
fn a_protocol_the_host_has_not_enabled_is_reported_and_nothing_is_sent() {
    let _guard = serial();
    let session = Session::open("not-enabled", Kind::SelfHosted);
    session.register_native();
    protocols::enable_for_test(session.handle, &["anthropic", "open_ai"]);
    assert_eq!(session.start(31).status, OHAND_CORE_STATUS_OK);
    let completed = session.event(31);
    assert_eq!(completed.status, OHAND_CORE_STATUS_OK);
    assert_eq!(
        payload_of(&completed),
        json!({
            "operation_id": 31,
            "phase": "completed",
            "outcome": "protocol_not_enabled",
        })
    );
    session.assert_no_command();
    wait_until_idle(session.handle);
    session.close();
}

#[test]
fn a_protocol_disabled_after_preparation_and_before_the_send_is_not_sent() {
    let _guard = serial();
    let session = Session::open("disabled-before-send", Kind::Anthropic);
    session.register_native();
    *lock(&BEFORE_SEND) = Some(|handle| protocols::enable_for_test(handle, &["open_ai"]));
    let started = session.start(32);
    assert_eq!(started.status, OHAND_CORE_STATUS_OK);
    let dispatched = session.event(32);
    assert_eq!(dispatched.status, OHAND_CORE_STATUS_OK);
    let completed = session.event(32);
    *lock(&BEFORE_SEND) = None;
    assert_eq!(completed.status, OHAND_CORE_STATUS_OK);
    assert_eq!(
        payload_of(&completed),
        json!({
            "operation_id": 32,
            "phase": "completed",
            "outcome": "protocol_not_enabled",
        })
    );
    session.assert_no_command();
    wait_until_idle(session.handle);
    session.close();
}

#[test]
fn cancelling_the_exchange_abandons_the_send_and_reports_cancelled() {
    let _guard = serial();
    let session = Session::open("cancel", Kind::Anthropic);
    session.register_native();
    let (_, _) = session.dispatch(41);
    assert_eq!(
        consume(ohand_core_cancel_provider_exchange(session.handle, 41)).status,
        OHAND_CORE_STATUS_OK
    );
    let cancel = session.command(41);
    assert_eq!(cancel.command, OHAND_PROVIDER_COMMAND_CANCEL);
    assert_eq!(session.outcome_of(41), "cancelled");
    session.close();
}

#[test]
fn closing_the_core_while_a_check_runs_does_not_hang() {
    let _guard = serial();
    let session = Session::open("close", Kind::Anthropic);
    session.register_native();
    let (_, _) = session.dispatch(42);
    let handle = session.handle;
    session.close();
    wait_until_idle(handle);
}

#[test]
fn refusals_happen_before_anything_is_sent_and_are_fixed_failures() {
    let _guard = serial();
    let session = Session::open("refusals", Kind::Anthropic);

    let unregistered = session.start(51);
    assert_ne!(unregistered.status, OHAND_CORE_STATUS_OK);
    assert_eq!(failure_code(&unregistered), "transport_not_registered");
    session.register_native();

    let unknown = session.start_with(52, &json!({ "profile_version": "no-such-version" }));
    assert_eq!(unknown.status, OHAND_CORE_STATUS_OK);
    let unknown_event = session.event(52);
    assert_ne!(unknown_event.status, OHAND_CORE_STATUS_OK);
    assert_eq!(failure_event_code(&unknown_event), "not_found");

    rusqlite::Connection::open(&session.path)
        .unwrap()
        .execute(
            "UPDATE provider_profiles SET revoked_at = '2026-10-09T00:00:00Z'",
            [],
        )
        .unwrap();
    assert_eq!(session.start(53).status, OHAND_CORE_STATUS_OK);
    let revoked = session.event(53);
    assert_ne!(revoked.status, OHAND_CORE_STATUS_OK);
    assert_eq!(failure_event_code(&revoked), "profile_revoked");
    session.assert_no_command();

    for request in [
        json!({}),
        json!({ "profile_version": "" }),
        json!({ "profile_version": session.profile_version, "route_id": ROUTE_ID }),
        json!({ "profile_version": session.profile_version, "text": NOTE_TEXT }),
        json!({ "profile_version": session.profile_version, "endpoint": "https://example.test" }),
    ] {
        let refused = session.start_with(54, &request);
        assert_ne!(refused.status, OHAND_CORE_STATUS_OK, "{request}");
    }
    assert_eq!(
        consume(unsafe {
            ohand_core_start_provider_connection_check(session.handle, 55, std::ptr::null(), 0)
        })
        .status,
        OHAND_CORE_STATUS_PERMANENT
    );
    session.assert_no_command();
    session.close();
}

#[test]
fn a_running_operation_id_cannot_be_reused() {
    let _guard = serial();
    let session = Session::open("duplicate", Kind::Anthropic);
    session.register_native();
    let (_, _) = session.dispatch(61);
    let duplicate = session.start(61);
    assert_eq!(failure_code(&duplicate), "duplicate_operation");
    session.respond(61, 200, &Kind::Anthropic.reply());
    assert_eq!(session.outcome_of(61), "ok");
    session.close();
}

#[test]
fn no_event_or_failure_carries_a_secret_endpoint_header_or_body() {
    let _guard = serial();
    let mut texts = Vec::new();
    for (index, (kind, answer)) in [
        (Kind::Anthropic, 0),
        (Kind::OpenAi, 1),
        (Kind::SelfHosted, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let session = Session::open("redaction", kind);
        session.register_native();
        let operation_id = 300 + index as u64;
        assert_eq!(session.start(operation_id).status, OHAND_CORE_STATUS_OK);
        texts.push(String::from_utf8(session.event(operation_id).payload).unwrap());
        let _ = session.command(operation_id);
        match answer {
            0 => session.respond(
                operation_id,
                401,
                format!("{{\"error\":\"bad key {CREDENTIAL_REFERENCE} {NOTE_TEXT}\"}}").as_bytes(),
            ),
            1 => session.respond(
                operation_id,
                500,
                format!("<html>{NOTE_TEXT} Bearer sk-secret</html>").as_bytes(),
            ),
            _ => session.fail(operation_id, "rejected"),
        }
        texts.push(String::from_utf8(session.event(operation_id).payload).unwrap());
        wait_until_idle(session.handle);
        session.close();
    }
    for text in texts {
        assert_redacted(&text);
    }
}

#[test]
fn the_derived_destinations_match_the_route_ceiling_a_grant_would_store() {
    let _guard = serial();
    for kind in [Kind::Anthropic, Kind::OpenAi, Kind::SelfHosted] {
        let session = Session::open("drift", kind);
        rusqlite::Connection::open(&session.path)
            .unwrap()
            .execute(
                "INSERT INTO routes (route_id, route_name, scope, processing_destinations, \
                 created_at) VALUES (?, 'synthetic', 'personal', '[]', '2026-10-08T09:00:00Z')",
                [ROUTE_ID],
            )
            .unwrap();
        let mut database =
            Database::open(&session.path, Arc::new(crate::store::schema::SystemClock)).unwrap();
        grant_route_capability(
            &mut database,
            ROUTE_ID,
            ProcessingCapability::TextInterpretation,
            &session.profile_version,
            "2026-10-08T09:01:00Z",
        )
        .unwrap();
        let ceiling: String = rusqlite::Connection::open(&session.path)
            .unwrap()
            .query_row(
                "SELECT processing_destinations FROM routes WHERE route_id = ?",
                [ROUTE_ID],
                |row| row.get(0),
            )
            .unwrap();
        let ceiling: Vec<String> = serde_json::from_str(&ceiling).unwrap();

        let connection = rusqlite::Connection::open(&session.path).unwrap();
        let plan = prepare_check(&connection, &session.profile_version).unwrap();
        assert_eq!(plan.destinations, ceiling, "{}", kind.key());
        assert_eq!(plan.destinations, vec![kind.origin().to_string()]);
        drop(database);
        session.close();
    }
}

#[test]
fn the_classification_never_depends_on_text_of_the_provider_response() {
    for status in [
        200u16, 204, 301, 400, 401, 403, 404, 408, 422, 429, 500, 502, 503,
    ] {
        let outcome = outcome_for_status(status);
        assert!(ALL_OUTCOMES.contains(&serde_json::to_value(outcome).unwrap().as_str().unwrap()));
    }
    assert_eq!(outcome_for_status(301), ConnectionOutcome::ProviderError);
}

#[test]
fn a_self_hosted_check_needs_its_profile_and_model_registered() {
    let _guard = serial();
    let session = Session::open("not-registered", Kind::SelfHosted);
    session.register_native();
    let model = Kind::SelfHosted.serving_model();
    let profile_version = session.profile_version.clone();

    let refusals: [(u64, &[(&str, &str)]); 3] = [
        (61, &[]),
        (62, &[("00000000-0000-4000-8000-00000000aaaa", model)]),
        (63, &[(profile_version.as_str(), "another-model")]),
    ];
    for (operation_id, pairs) in refusals {
        protocols::register_for_test(session.handle, pairs);
        assert_eq!(session.start(operation_id).status, OHAND_CORE_STATUS_OK);
        assert_eq!(
            payload_of(&session.event(operation_id)),
            json!({
                "operation_id": operation_id,
                "phase": "completed",
                "outcome": "self_hosted_not_registered",
            })
        );
        session.assert_no_command();
        wait_until_idle(session.handle);
    }

    protocols::register_for_test(session.handle, &[(&profile_version, model)]);
    let (_, _) = session.dispatch(64);
    session.close();
}
