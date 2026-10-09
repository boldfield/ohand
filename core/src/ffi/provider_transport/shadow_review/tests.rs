//! Drives the shadow review exports the way the native layer does, against the real core store,
//! the real provider adapters and a recording stand-in for the native transport.

use super::*;
use crate::ffi::core_handle::exports::*;
use crate::ffi::core_handle::failure::*;
use crate::ffi::core_handle::tests::{
    consume, recorder, register, serial, Event, Outcome, Recorder, WAIT,
};
use crate::providers::contracts::{
    CapabilityMetadata, ProviderCapability, ProviderProfileBuilder, StructuredOutputMode,
};
use serde_json::{json, Value};
use std::sync::mpsc::Receiver;

const ORIGIN: &str = "https://api.anthropic.com";
const CREDENTIAL_REFERENCE: &str = "credential-ref/synthetic-reviewer";
const NOTE_TEXT: &str = "call the roofer tomorrow at 9";
const CAPTURE_ID: &str = "5a1c0000-0000-4000-8000-0000000000c1";
const ITEM_ID: &str = "5a1c0000-0000-4000-8000-0000000000e1";
const REQUEST_VERSION: &str = "5a1c0000-0000-4000-8000-0000000000a1";
const PROPOSAL_ID: &str = "5a1c0000-0000-4000-8000-0000000000b1";
const INTERPRET_JOB_ID: &str = "job-interpret-1";
const ROUTE_ID: &str = "route-synthetic";
const REVIEW_MODEL: &str = "synthetic-review-model";

struct Command {
    operation_id: u64,
    command: u32,
    data: Vec<u8>,
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
    });
}

fn review_profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "synthetic-review-profile",
        ProviderProtocol::Anthropic,
        REVIEW_MODEL,
    )
    .credential_ref(CREDENTIAL_REFERENCE)
    .timeout_seconds(30)
    .authorized_destination(ORIGIN)
    .capability(
        CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "ffi-fixtures:shadow_review",
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
    review_granted: bool,
    with_primary: bool,
}

impl Scenario {
    fn standard() -> Scenario {
        Scenario {
            profile: review_profile(),
            review_granted: true,
            with_primary: true,
        }
    }

    fn profile_version(&self) -> String {
        self.profile.profile_version().to_string()
    }
}

fn open_connection(path: &str) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

fn seed(path: &str, scenario: &Scenario) {
    let mut database =
        Database::open(path, std::sync::Arc::new(crate::store::schema::SystemClock)).unwrap();
    let transaction = database.transaction().unwrap();
    let record = serde_json::to_value(&scenario.profile).unwrap();
    let text_of = |key: &str| record[key].to_string();
    transaction
        .execute(
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, \
             model, credential_ref, timeout_seconds, retry_policy, authorized_destinations, \
             capabilities, created_at, revoked_at) VALUES (?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, NULL)",
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
            rusqlite::params![ROUTE_ID, json!([ORIGIN]).to_string()],
        )
        .unwrap();
    transaction
        .execute(
            "INSERT INTO route_authorizations (auth_id, route_id, capability, \
             authorized_destinations, created_at) VALUES ('auth-interpret', ?, \
             'text_interpretation', ?, '2026-10-08T09:00:00Z')",
            rusqlite::params![ROUTE_ID, json!([ORIGIN]).to_string()],
        )
        .unwrap();
    if scenario.review_granted {
        transaction
            .execute(
                "INSERT INTO route_authorizations (auth_id, route_id, capability, \
                 authorized_destinations, created_at) VALUES ('auth-review', ?, 'review', ?, \
                 '2026-10-08T09:00:00Z')",
                rusqlite::params![ROUTE_ID, json!([ORIGIN]).to_string()],
            )
            .unwrap();
    }
    transaction
        .execute(
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, \
             profile_version, request_version, status, attempt_count, created_at) \
             VALUES (?, 1, ?, 'interpret', 0, ?, ?, 'completed', 1, '2026-10-08T09:30:02Z')",
            rusqlite::params![
                INTERPRET_JOB_ID,
                ITEM_ID,
                scenario.profile.profile_version(),
                REQUEST_VERSION
            ],
        )
        .unwrap();
    if scenario.with_primary {
        transaction
            .execute(
                "INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, \
                 schema_version, text_basis_kind, text_basis_id, applied_state, proposal_type, \
                 reminder_proposal, session_topic_proposal, source_spans, abstained, \
                 request_version, created_at) VALUES (?, ?, ?, 0, 1, 'original', NULL, \
                 'applied', 'action', NULL, NULL, '[]', 0, ?, '2026-10-08T09:31:00Z')",
                rusqlite::params![PROPOSAL_ID, ITEM_ID, CAPTURE_ID, REQUEST_VERSION],
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
    scenario: Scenario,
}

impl Session {
    fn open(name: &str, scenario: &Scenario) -> Session {
        let directory =
            std::env::temp_dir().join(format!("ohand-shadow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("core.sqlite").to_string_lossy().into_owned();
        seed(&path, scenario);
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
            scenario: scenario.clone(),
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

    fn policy(max_requests: u32, max_attempts: u32) -> Value {
        json!({
            "enabled": true,
            "sample_per_mille": 1000,
            "window_seconds": 3600,
            "max_requests_per_window": max_requests,
            "max_attempts_per_sample": max_attempts,
        })
    }

    fn select_with(&self, operation_id: u64, policy: Value) -> Value {
        let request = serde_json::to_vec(&json!({
            "item_id": ITEM_ID,
            "source_revision": 0,
            "request_version": REQUEST_VERSION,
            "review_profile_version": self.scenario.profile_version(),
            "policy": policy,
        }))
        .unwrap();
        let outcome = consume(unsafe {
            ohand_core_start_shadow_selection(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        let event = self.event(operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK, "{:?}", event.payload);
        payload_of(&event)
    }

    /// Selects the case under the standard policy and returns its job id.
    fn select(&self, operation_id: u64) -> String {
        let payload = self.select_with(operation_id, Self::policy(4, 2));
        assert_eq!(payload["selection"], "selected", "{payload}");
        payload["record"]["job_id"].as_str().unwrap().to_string()
    }

    fn start_review_with(&self, operation_id: u64, job_id: &str, policy: Value) -> Outcome {
        let request = serde_json::to_vec(&json!({ "job_id": job_id, "policy": policy })).unwrap();
        consume(unsafe {
            ohand_core_start_shadow_review(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        })
    }

    fn start_review(&self, operation_id: u64, job_id: &str) -> Outcome {
        self.start_review_with(operation_id, job_id, Self::policy(4, 2))
    }

    fn read_record(&self, operation_id: u64, job_id: &str) -> Event {
        let request = serde_json::to_vec(&json!({ "job_id": job_id })).unwrap();
        let outcome = consume(unsafe {
            ohand_core_start_shadow_record(
                self.handle,
                operation_id,
                request.as_ptr(),
                request.len(),
            )
        });
        assert_eq!(outcome.status, OHAND_CORE_STATUS_OK);
        self.event(operation_id)
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

    fn complete(&self, operation_id: u64, body: &[u8]) {
        let headers = br#"[["content-type","application/json"]]"#;
        let outcome = consume(unsafe {
            ohand_core_complete_provider_exchange(
                self.handle,
                operation_id,
                200,
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

    /// Starts a review and answers its `dispatched` event; returns the native send description.
    fn dispatch(&self, operation_id: u64, job_id: &str) -> Value {
        assert_eq!(
            self.start_review(operation_id, job_id).status,
            OHAND_CORE_STATUS_OK
        );
        let dispatched = self.event(operation_id);
        assert_eq!(dispatched.status, OHAND_CORE_STATUS_OK);
        assert_eq!(
            payload_of(&dispatched),
            json!({ "operation_id": operation_id, "phase": "dispatched" })
        );
        let command = self.command(operation_id);
        assert_eq!(command.command, OHAND_PROVIDER_COMMAND_SEND);
        serde_json::from_slice(&command.data).unwrap()
    }

    fn final_record(&self, operation_id: u64) -> Value {
        let event = self.event(operation_id);
        assert_eq!(event.status, OHAND_CORE_STATUS_OK, "{:?}", event.payload);
        let payload = payload_of(&event);
        assert_eq!(payload["phase"], "completed");
        assert_eq!(payload["denial"], Value::Null);
        wait_until_idle(self.handle);
        payload["record"].clone()
    }

    /// Everything a shadow verdict must never touch.
    fn authoritative_state(&self) -> Vec<String> {
        let connection = open_connection(&self.path);
        let mut rows = Vec::new();
        for table in ["items", "proposals", "corrections", "route_authorizations"] {
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

    fn revoke_review_grant(&self) {
        open_connection(&self.path)
            .execute(
                "DELETE FROM route_authorizations WHERE capability = 'review'",
                [],
            )
            .unwrap();
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

fn wait_until_idle(handle: u64) {
    let deadline = Instant::now() + WAIT;
    while lock(&EXCHANGES).keys().any(|(owner, _)| *owner == handle) {
        assert!(Instant::now() < deadline, "exchange slot was not released");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn anthropic_reply(item_type: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "msg_synthetic_01",
        "type": "message",
        "role": "assistant",
        "model": REVIEW_MODEL,
        "content": [{
            "type": "tool_use",
            "id": "toolu_synthetic_01",
            "name": "interpret",
            "input": {
                "operation": { "kind": "annotate" },
                "item_type": item_type,
                "source_spans": [{ "start": 0, "end": 4 }],
            },
        }],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 42, "output_tokens": 17 },
    }))
    .unwrap()
}

fn assert_redacted(payloads: &[Value]) {
    for payload in payloads {
        let text = payload.to_string();
        for secret in [ROUTE_ID, CREDENTIAL_REFERENCE, NOTE_TEXT, "x-api-key"] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}

#[test]
fn selection_reserves_a_case_once_and_reports_every_skip_with_a_fixed_code() {
    let _guard = serial();
    let session = Session::open("selection", &Scenario::standard());

    let disabled = session.select_with(
        1,
        json!({
            "enabled": false,
            "sample_per_mille": 1000,
            "window_seconds": 3600,
            "max_requests_per_window": 4,
            "max_attempts_per_sample": 2,
        }),
    );
    assert_eq!(disabled["selection"], "skipped");
    assert_eq!(disabled["skip"], "disabled");

    let budget = session.select_with(2, Session::policy(1, 2));
    assert_eq!(
        (budget["selection"].clone(), budget["skip"].clone()),
        (json!("skipped"), json!("budget_exhausted"))
    );

    let selected = session.select_with(3, Session::policy(4, 2));
    assert_eq!(selected["selection"], "selected");
    assert_eq!(selected["record"]["outcome"], "pending");
    assert_eq!(selected["record"]["item_id"], ITEM_ID);
    assert_eq!(selected["record"]["attempts_used"], 0);
    let job_id = selected["record"]["job_id"].as_str().unwrap().to_string();

    let again = session.select_with(4, Session::policy(4, 2));
    assert_eq!(again["selection"], "already_selected");
    assert_eq!(again["record"]["job_id"], job_id.as_str());

    let stored = payload_of(&session.read_record(5, &job_id));
    assert_eq!(stored["record"], selected["record"]);
    assert_redacted(&[disabled, budget, selected, again, stored]);
    session.close();
}

#[test]
fn selection_is_skipped_without_a_primary_result_or_a_review_grant() {
    let _guard = serial();
    let mut scenario = Scenario::standard();
    scenario.with_primary = false;
    let session = Session::open("no-primary", &scenario);
    let skipped = session.select_with(1, Session::policy(4, 2));
    assert_eq!(
        (skipped["selection"].clone(), skipped["skip"].clone()),
        (json!("skipped"), json!("primary_missing"))
    );
    session.close();

    let mut scenario = Scenario::standard();
    scenario.review_granted = false;
    let session = Session::open("no-grant", &scenario);
    let skipped = session.select_with(1, Session::policy(4, 2));
    assert_eq!(skipped["selection"], "skipped");
    assert_eq!(skipped["skip"], "not_authorized");
    assert!(skipped["denial"].is_string());
    session.close();
}

#[test]
fn an_agreeing_review_is_one_request_and_records_a_verdict_without_changing_authoritative_state() {
    let _guard = serial();
    let session = Session::open("agree", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    let before = session.authoritative_state();

    let send = session.dispatch(2, &job_id);
    assert_eq!(send["url"], "https://api.anthropic.com/v1/messages");
    assert_eq!(send["authorized_origins"], json!([ORIGIN]));
    assert_eq!(send["capability"], "review");
    assert_eq!(
        send["credential"],
        json!({ "reference": CREDENTIAL_REFERENCE, "header": "x-api-key", "scheme": null })
    );
    let body = send["body"].as_str().unwrap();
    assert!(body.contains(NOTE_TEXT));
    assert!(body.contains(REVIEW_MODEL));

    session.complete(2, &anthropic_reply("action"));
    let record = session.final_record(2);
    assert_eq!(record["outcome"], "reviewed");
    assert_eq!(record["verdict"], "agreement");
    assert_eq!(record["differences"], json!([]));
    assert_eq!(record["attempts_used"], 1);
    assert_redacted(std::slice::from_ref(&record));

    let stored = payload_of(&session.read_record(3, &job_id));
    assert_eq!(stored["record"], record);
    assert_eq!(session.authoritative_state(), before);
    session.assert_no_command();
    session.close();
}

#[test]
fn a_disagreeing_review_stores_only_difference_codes_and_never_a_second_round() {
    let _guard = serial();
    let session = Session::open("disagree", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    let before = session.authoritative_state();

    session.dispatch(2, &job_id);
    session.complete(2, &anthropic_reply("idea"));
    let record = session.final_record(2);
    assert_eq!(record["outcome"], "reviewed");
    assert_eq!(record["verdict"], "disagreement");
    assert_eq!(record["differences"], json!(["item_type"]));
    assert_redacted(&[record]);

    assert_eq!(session.authoritative_state(), before);
    session.assert_no_command();
    let again = session.start_review(3, &job_id);
    assert_eq!(again.status, OHAND_CORE_STATUS_OK);
    let refused = session.event(3);
    assert_ne!(refused.status, OHAND_CORE_STATUS_OK);
    assert_eq!(payload_of(&refused)["code"], "not_leasable");
    assert_eq!(
        payload_of(&session.read_record(4, &job_id))["record"]["outcome"],
        "reviewed"
    );
    session.assert_no_command();
    session.close();
}

#[test]
fn a_timeout_or_cancellation_leaves_the_case_unreviewed_with_no_second_request() {
    let _guard = serial();
    let session = Session::open("timeout", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    session.dispatch(2, &job_id);
    session.fail(2, "timeout");
    let record = session.final_record(2);
    assert_eq!(record["outcome"], "unreviewed");
    assert_eq!(record["reason"], "timeout");
    assert_eq!(record["verdict"], Value::Null);
    session.assert_no_command();
    session.close();

    let session = Session::open("cancel", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    session.dispatch(2, &job_id);
    assert_eq!(
        consume(ohand_core_cancel_provider_exchange(session.handle, 2)).status,
        OHAND_CORE_STATUS_OK
    );
    let cancel = session.command(2);
    assert_eq!(cancel.command, OHAND_PROVIDER_COMMAND_CANCEL);
    let record = session.final_record(2);
    assert_eq!(record["outcome"], "unreviewed");
    assert_eq!(record["reason"], "cancelled");
    session.assert_no_command();
    session.close();
}

#[test]
fn a_transient_failure_stays_pending_for_a_later_attempt_inside_the_attempt_budget() {
    let _guard = serial();
    let session = Session::open("transient", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    session.dispatch(2, &job_id);
    session.fail(2, "unavailable");
    let record = session.final_record(2);
    assert_eq!(record["outcome"], "pending");
    assert_eq!(record["attempts_used"], 1);
    assert_eq!(record["last_failure"], "unavailable");

    assert_eq!(
        session.start_review(3, &job_id).status,
        OHAND_CORE_STATUS_OK
    );
    let refused = session.event(3);
    assert_ne!(refused.status, OHAND_CORE_STATUS_OK);
    assert_eq!(payload_of(&refused)["code"], "not_leasable");
    session.assert_no_command();
    session.close();
}

#[test]
fn a_revoked_review_grant_denies_the_dispatch_before_any_request_is_prepared() {
    let _guard = serial();
    let session = Session::open("denied", &Scenario::standard());
    session.register_native();
    let job_id = session.select(1);
    session.revoke_review_grant();

    assert_eq!(
        session.start_review(2, &job_id).status,
        OHAND_CORE_STATUS_OK
    );
    let event = session.event(2);
    assert_eq!(event.status, OHAND_CORE_STATUS_OK);
    let payload = payload_of(&event);
    assert_eq!(payload["phase"], "completed");
    assert!(payload["denial"].is_string(), "{payload}");
    assert_eq!(payload["record"]["outcome"], "unreviewed");
    assert_eq!(payload["record"]["reason"], "dispatch_denied");
    assert_redacted(&[payload]);
    session.assert_no_command();
    wait_until_idle(session.handle);
    session.close();
}

#[test]
fn a_disabled_policy_denies_dispatch_and_an_unregistered_transport_is_refused() {
    let _guard = serial();
    let session = Session::open("disabled", &Scenario::standard());
    let job_id = session.select(1);
    assert_eq!(
        session.start_review(2, &job_id).code(),
        "transport_not_registered"
    );

    session.register_native();
    let disabled = json!({
        "enabled": false,
        "sample_per_mille": 1000,
        "window_seconds": 3600,
        "max_requests_per_window": 4,
        "max_attempts_per_sample": 2,
    });
    assert_eq!(
        session.start_review_with(3, &job_id, disabled).status,
        OHAND_CORE_STATUS_OK
    );
    let payload = payload_of(&session.event(3));
    assert_eq!(payload["denial"], "disabled");
    assert_eq!(payload["record"]["outcome"], "unreviewed");
    session.assert_no_command();
    session.close();
}

#[test]
fn only_shadow_cases_can_be_reviewed_and_unknown_cases_are_not_found() {
    let _guard = serial();
    let session = Session::open("kinds", &Scenario::standard());
    session.register_native();
    session.select(1);

    assert_eq!(
        session.start_review(2, INTERPRET_JOB_ID).status,
        OHAND_CORE_STATUS_OK
    );
    let refused = session.event(2);
    assert_ne!(refused.status, OHAND_CORE_STATUS_OK);
    assert_eq!(payload_of(&refused)["code"], "not_shadow_job");

    assert_eq!(
        session.start_review(3, "no-such-job").status,
        OHAND_CORE_STATUS_OK
    );
    let missing = session.event(3);
    assert_ne!(missing.status, OHAND_CORE_STATUS_OK);
    assert_eq!(payload_of(&missing)["code"], "not_found");

    let missing = session.read_record(4, "no-such-job");
    assert_ne!(missing.status, OHAND_CORE_STATUS_OK);
    session.assert_no_command();
    wait_until_idle(session.handle);
    session.close();
}

#[test]
fn malformed_requests_are_rejected_without_events() {
    let _guard = serial();
    let session = Session::open("malformed", &Scenario::standard());
    session.register_native();
    let unknown_field = serde_json::to_vec(&json!({
        "job_id": "x", "policy": Session::policy(4, 2), "route_id": ROUTE_ID
    }))
    .unwrap();
    let outcome = consume(unsafe {
        ohand_core_start_shadow_review(
            session.handle,
            1,
            unknown_field.as_ptr(),
            unknown_field.len(),
        )
    });
    assert_eq!(outcome.code(), "invalid_request");
    let empty =
        consume(unsafe { ohand_core_start_shadow_review(session.handle, 2, std::ptr::null(), 0) });
    assert_eq!(empty.code(), "invalid_request");
    assert!(session
        .events
        .recv_timeout(Duration::from_millis(200))
        .is_err());
    session.close();
}
