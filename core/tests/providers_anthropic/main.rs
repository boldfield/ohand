//! Synthetic Anthropic Messages protocol fixtures driven through `AnthropicAdapter` and the
//! shared `dispatch` harness with a scripted HTTP transport.

use chrono::{TimeZone, Utc};
use ohand_core::interpretation::instructions::{
    output_schema, M1_INSTRUCTION_TEXT, M1_INSTRUCTION_VERSION,
};
use ohand_core::providers::anthropic::fake::{FakeAnthropicStep, FakeAnthropicTransport};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, HttpMethod, HttpRequest, SettingsError,
    ANTHROPIC_API_VERSION, ANTHROPIC_MESSAGES_ENDPOINT, CREDENTIAL_HEADER,
    INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, FailureKind, InterpretationOutput,
    InterpretationRequest, ManualClock, ProfileValidationError, ProviderCapability,
    ProviderFailure, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    StructuredOutputMode, TextBasis, TransportError,
};
use ohand_core::time::TimeContext;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

mod diagnostic;

const ORIGIN: &str = "https://api.anthropic.com";
const CREDENTIAL_REF: &str = "credential-ref/anthropic-primary";
const MODEL: &str = "synthetic-model-one";
const NOTE_TEXT: &str = "call mom tomorrow at 9";

fn builder(mode: StructuredOutputMode) -> ProviderProfileBuilder {
    ProviderProfileBuilder::new("synthetic-anthropic", ProviderProtocol::Anthropic, MODEL)
        .credential_ref(CREDENTIAL_REF)
        .timeout_seconds(30)
        .authorized_destination(ORIGIN)
        .capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "protocol-fixtures:providers_anthropic",
            )
            .with_input_size_limit(500)
            .with_structured_output(mode),
        )
}

fn profile(mode: StructuredOutputMode) -> ProviderProfile {
    builder(mode).build().expect("valid profile")
}

fn request_for(profile: &ProviderProfile) -> InterpretationRequest {
    InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        NOTE_TEXT,
        Uuid::new_v4().to_string(),
        M1_INSTRUCTION_VERSION,
        profile,
        "route-private-name",
        TimeContext {
            timezone: "America/Chicago".to_string(),
            locale: "en-US".to_string(),
            reference_time: Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0).unwrap(),
            utc_offset_at_capture: -21_600,
            calendar: "gregorian".to_string(),
        },
    )
    .expect("valid request")
}

struct Harness {
    clock: Arc<ManualClock>,
    cancel: CancelToken,
    profile: ProviderProfile,
    transport: Arc<FakeAnthropicTransport>,
    adapter: AnthropicAdapter,
}

impl Harness {
    fn new(steps: Vec<FakeAnthropicStep>) -> Harness {
        Harness::with(
            profile(StructuredOutputMode::JsonSchema),
            AnthropicSettings::default(),
            steps,
        )
    }

    fn with(
        profile: ProviderProfile,
        settings: AnthropicSettings,
        steps: Vec<FakeAnthropicStep>,
    ) -> Harness {
        let clock = Arc::new(ManualClock::new());
        let transport = Arc::new(FakeAnthropicTransport::new(clock.clone(), steps));
        let adapter = AnthropicAdapter::new(transport.clone(), settings);
        Harness {
            clock,
            cancel: CancelToken::new(),
            profile,
            transport,
            adapter,
        }
    }

    fn run(&self) -> Result<InterpretationOutput, ProviderFailure> {
        self.run_with(&DispatchLimits::default())
    }

    fn run_with(&self, limits: &DispatchLimits) -> Result<InterpretationOutput, ProviderFailure> {
        dispatch(
            &self.adapter,
            &self.profile,
            &request_for(&self.profile),
            self.clock.as_ref(),
            &self.cancel,
            limits,
        )
    }

    fn failure_kind(&self) -> FailureKind {
        self.run().expect_err("expected failure").kind
    }
}

fn message(stop_reason: &str, content: Value) -> Value {
    json!({
        "id": "msg_synthetic_01",
        "type": "message",
        "role": "assistant",
        "model": MODEL,
        "content": content,
        "stop_reason": stop_reason,
        "usage": { "input_tokens": 42, "output_tokens": 17 },
    })
}

fn tool_use(name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": "toolu_synthetic_01", "name": name, "input": input })
}

fn valid_proposal() -> Value {
    json!({
        "operation": { "kind": "annotate" },
        "item_type": "action",
        "source_spans": [{ "start": 0, "end": 4 }],
    })
}

fn ok_reply(proposal: Value) -> FakeAnthropicStep {
    FakeAnthropicStep::respond_json(
        200,
        &message(
            "tool_use",
            json!([tool_use(INTERPRETATION_TOOL_NAME, proposal)]),
        ),
    )
}

fn error_envelope(error_type: &str) -> Value {
    json!({
        "type": "error",
        "error": { "type": error_type, "message": "synthetic provider detail" },
    })
}

fn body_of(request: &HttpRequest) -> Value {
    serde_json::from_slice(&request.body).expect("request body is JSON")
}

// ---- request construction ----

#[test]
fn request_is_built_from_profile_and_protocol_settings() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    harness.run().expect("valid reply");

    let calls = harness.transport.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.url, ANTHROPIC_MESSAGES_ENDPOINT);
    assert_eq!(call.method, HttpMethod::Post);
    assert_eq!(call.timeout_ms, 30_000);
    assert_eq!(call.deadline_ms, 30_000);
    assert_eq!(
        call.max_response_bytes,
        DispatchLimits::default().max_response_bytes
    );
    assert!(call.headers.contains(&(
        "anthropic-version".to_string(),
        ANTHROPIC_API_VERSION.to_string()
    )));
    assert!(call
        .headers
        .contains(&("content-type".to_string(), "application/json".to_string())));

    let credential = call.credential.as_ref().expect("credential attachment");
    assert_eq!(credential.reference, CREDENTIAL_REF);
    assert_eq!(credential.header, CREDENTIAL_HEADER);

    let body = body_of(call);
    let system = body["system"].as_str().expect("system prompt").to_string();
    assert!(
        system.starts_with(M1_INSTRUCTION_TEXT),
        "the pinned instructions are sent byte for byte"
    );
    let delivery = &system[M1_INSTRUCTION_TEXT.len()..];
    assert!(delivery.contains(INTERPRETATION_TOOL_NAME));
    let user = body["messages"][0]["content"]
        .as_str()
        .expect("user message");
    let document: Value = serde_json::from_str(user).expect("user message is the context document");
    assert_eq!(document["instruction_version"], M1_INSTRUCTION_VERSION);
    assert_eq!(document["source"]["text"], NOTE_TEXT);
    assert_eq!(document["source"]["trust"], "untrusted_data");
    assert_eq!(document["time_context"]["timezone"], "America/Chicago");
    assert_eq!(document["time_context"]["locale"], "en-US");
    assert_eq!(document["time_context"]["calendar"], "gregorian");
    assert_eq!(document["time_context"]["utc_offset_at_capture"], -21_600);
    assert_eq!(
        document["time_context"]["reference_time"],
        "2026-01-05T15:00:00Z"
    );
    let settings = AnthropicSettings::default();
    assert_eq!(settings.proposal_schema, output_schema());
    let expected_schema = settings.proposal_schema.clone();
    assert_eq!(
        body,
        json!({
            "model": MODEL,
            "max_tokens": settings.max_output_tokens,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
            "tools": [{
                "name": INTERPRETATION_TOOL_NAME,
                "description": "Report the interpretation of the captured note.",
                "input_schema": expected_schema,
            }],
            "tool_choice": { "type": "auto" },
        })
    );
}

#[test]
fn unpublished_instruction_version_is_rejected_before_any_transport_call() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let request = InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        NOTE_TEXT,
        Uuid::new_v4().to_string(),
        "instructions-v7",
        &harness.profile,
        "route-private-name",
        request_for(&harness.profile).time_context().clone(),
    )
    .expect("valid request");
    let failure = dispatch(
        &harness.adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect_err("only the published instructions are sent");
    assert_eq!(failure.kind, FailureKind::Rejected);
    assert!(harness.transport.calls().is_empty());
}

#[test]
fn model_credential_and_timeout_follow_the_profile() {
    let other_profile = builder(StructuredOutputMode::JsonSchema)
        .model("synthetic-model-two")
        .credential_ref("credential-ref/anthropic-work")
        .timeout_seconds(12)
        .build()
        .expect("valid profile");
    let harness = Harness::with(
        other_profile,
        AnthropicSettings::default(),
        vec![ok_reply(valid_proposal())],
    );
    harness.run().expect("valid reply");

    let call = &harness.transport.calls()[0];
    assert_eq!(body_of(call)["model"], "synthetic-model-two");
    assert_eq!(
        call.credential.as_ref().unwrap().reference,
        "credential-ref/anthropic-work"
    );
    assert_eq!(call.timeout_ms, 12_000);
}

#[test]
fn max_output_tokens_comes_from_settings() {
    let settings = AnthropicSettings::default()
        .with_max_output_tokens(256)
        .expect("positive");
    let harness = Harness::with(
        profile(StructuredOutputMode::JsonSchema),
        settings,
        vec![ok_reply(valid_proposal())],
    );
    harness.run().expect("valid reply");
    assert_eq!(body_of(&harness.transport.calls()[0])["max_tokens"], 256);
    assert_eq!(
        AnthropicSettings::default().with_max_output_tokens(0),
        Err(SettingsError::InvalidMaxOutputTokens)
    );
}

#[test]
fn tool_schema_follows_the_declared_structured_output_mode() {
    let full_schema = AnthropicSettings::default().proposal_schema;
    for (mode, expected_schema) in [
        (StructuredOutputMode::JsonSchema, full_schema.clone()),
        (
            StructuredOutputMode::JsonObject,
            json!({ "type": "object" }),
        ),
        (StructuredOutputMode::None, json!({ "type": "object" })),
    ] {
        let harness = Harness::with(
            profile(mode),
            AnthropicSettings::default(),
            vec![ok_reply(valid_proposal())],
        );
        harness.run().expect("valid reply");
        let body = body_of(&harness.transport.calls()[0]);
        assert_eq!(
            body["tools"][0]["input_schema"], expected_schema,
            "{mode:?}"
        );
        assert_eq!(body["tool_choice"], json!({ "type": "auto" }));
    }
}

#[test]
fn secrets_and_private_routing_never_enter_the_request() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    harness.run().expect("valid reply");
    let call = &harness.transport.calls()[0];
    let body_text = String::from_utf8(call.body.clone()).unwrap();
    assert!(!body_text.contains(CREDENTIAL_REF));
    assert!(!body_text.contains("route-private-name"));
    for (name, value) in &call.headers {
        assert!(
            !value.contains(CREDENTIAL_REF),
            "{name} leaks the reference"
        );
    }
    assert!(!format!("{call:?}").contains(NOTE_TEXT));
}

// ---- valid structured output ----

#[test]
fn valid_tool_use_returns_only_the_proposal() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let output = harness.run().expect("valid reply");
    let proposal = Value::Object(output.proposal);
    assert_eq!(proposal, valid_proposal());
    assert!(proposal.get("stop_reason").is_none());
    assert!(proposal.get("content").is_none());
    assert!(proposal.get("usage").is_none());
}

#[test]
fn text_and_thinking_blocks_around_the_interpret_call_are_ignored() {
    let json_looking_text = json!({ "schema_version": 1, "annotation": { "kind": "other" } });
    let thinking = json!({ "type": "thinking", "thinking": "synthetic", "signature": "sig" });
    let redacted = json!({ "type": "redacted_thinking", "data": "opaque" });
    let preamble = json!({ "type": "text", "text": "I'll record this note." });
    let json_text = json!({ "type": "text", "text": json_looking_text.to_string() });
    let call = tool_use(INTERPRETATION_TOOL_NAME, valid_proposal());
    let shapes = [
        json!([preamble, call]),
        json!([thinking, call]),
        json!([thinking, redacted, preamble, call]),
        json!([json_text, call]),
        json!([call, preamble]),
    ];
    for content in shapes {
        let reply = FakeAnthropicStep::respond_json(200, &message("tool_use", content.clone()));
        let output = Harness::new(vec![reply]).run().expect("valid reply");
        assert_eq!(
            Value::Object(output.proposal),
            valid_proposal(),
            "{content}"
        );
    }
}

#[test]
fn proposal_with_unrelated_extra_fields_is_kept_for_semantic_validation() {
    let mut proposal = valid_proposal();
    proposal["session_topic"] = json!({ "name": "errands" });
    let harness = Harness::with(
        profile(StructuredOutputMode::JsonObject),
        AnthropicSettings::default(),
        vec![ok_reply(proposal.clone())],
    );
    assert_eq!(Value::Object(harness.run().unwrap().proposal), proposal);
}

// ---- invalid structured output ----

#[test]
fn malformed_and_unexpected_replies_are_invalid_output() {
    let interpret = |input: Value| tool_use(INTERPRETATION_TOOL_NAME, input);
    let replies: Vec<(&str, FakeAnthropicStep)> = vec![
        (
            "not json",
            FakeAnthropicStep::respond(200, "<html>gateway</html>"),
        ),
        ("json array", FakeAnthropicStep::respond(200, "[1,2]")),
        (
            "proposal as the whole body",
            FakeAnthropicStep::respond_json(200, &valid_proposal()),
        ),
        (
            "wrong envelope type",
            FakeAnthropicStep::respond_json(
                200,
                &json!({ "type": "completion", "completion": "x" }),
            ),
        ),
        (
            "empty content",
            FakeAnthropicStep::respond_json(200, &message("tool_use", json!([]))),
        ),
        (
            "missing content",
            FakeAnthropicStep::respond_json(
                200,
                &json!({ "type": "message", "stop_reason": "tool_use" }),
            ),
        ),
        (
            "missing stop reason",
            FakeAnthropicStep::respond_json(
                200,
                &json!({ "type": "message", "content": [interpret(valid_proposal())] }),
            ),
        ),
        (
            "text only without the interpret call",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "end_turn",
                    json!([{ "type": "text", "text": valid_proposal().to_string() }]),
                ),
            ),
        ),
        (
            "text only with tool_use stop reason",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([{ "type": "text", "text": valid_proposal().to_string() }]),
                ),
            ),
        ),
        (
            "wrong tool name",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([tool_use("some_other_tool", valid_proposal())]),
                ),
            ),
        ),
        (
            "unknown content block type",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([{ "type": "server_tool_use", "name": "web_search" }, interpret(valid_proposal())]),
                ),
            ),
        ),
        (
            "text and thinking but no interpret call",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([
                        { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                        { "type": "text", "text": "done" },
                    ]),
                ),
            ),
        ),
        (
            "two interpret calls around a text block",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([
                        interpret(valid_proposal()),
                        { "type": "text", "text": "again" },
                        interpret(valid_proposal()),
                    ]),
                ),
            ),
        ),
        (
            "two interpret calls",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([interpret(valid_proposal()), interpret(valid_proposal())]),
                ),
            ),
        ),
        (
            "interpret plus another tool",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([interpret(valid_proposal()), tool_use("other", json!({}))]),
                ),
            ),
        ),
        (
            "tool input is an array",
            FakeAnthropicStep::respond_json(
                200,
                &message("tool_use", json!([interpret(json!([valid_proposal()]))])),
            ),
        ),
        (
            "tool input is a string",
            FakeAnthropicStep::respond_json(
                200,
                &message("tool_use", json!([interpret(json!("done"))])),
            ),
        ),
        (
            "tool block without input",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([{ "type": "tool_use", "id": "t", "name": INTERPRETATION_TOOL_NAME }]),
                ),
            ),
        ),
        (
            "missing required operation",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([interpret(json!({ "item_type": "note" }))]),
                ),
            ),
        ),
        (
            "operation has the wrong type",
            FakeAnthropicStep::respond_json(
                200,
                &message(
                    "tool_use",
                    json!([interpret(json!({ "operation": "annotate" }))]),
                ),
            ),
        ),
        (
            "output truncated by max_tokens",
            FakeAnthropicStep::respond_json(
                200,
                &message("max_tokens", json!([interpret(valid_proposal())])),
            ),
        ),
        (
            "stop reason end_turn with a tool block",
            FakeAnthropicStep::respond_json(
                200,
                &message("end_turn", json!([interpret(valid_proposal())])),
            ),
        ),
        (
            "paused turn",
            FakeAnthropicStep::respond_json(
                200,
                &message("pause_turn", json!([interpret(valid_proposal())])),
            ),
        ),
    ];
    for (label, reply) in replies {
        let harness = Harness::new(vec![reply]);
        let failure = harness.run().expect_err(label);
        assert_eq!(failure.kind, FailureKind::InvalidOutput, "{label}");
    }
}

#[test]
fn refusal_is_a_permanent_rejection() {
    let reply = FakeAnthropicStep::respond_json(
        200,
        &message(
            "refusal",
            json!([{ "type": "text", "text": "I cannot help with that." }]),
        ),
    );
    let failure = Harness::new(vec![reply]).run().unwrap_err();
    assert_eq!(failure.kind, FailureKind::Rejected);
    assert!(!failure.retriable);
}

#[test]
fn object_modes_accept_any_object_but_still_reject_non_objects() {
    let harness = Harness::with(
        profile(StructuredOutputMode::JsonObject),
        AnthropicSettings::default(),
        vec![ok_reply(json!({ "anything": true }))],
    );
    assert!(harness.run().is_ok());

    let harness = Harness::with(
        profile(StructuredOutputMode::JsonObject),
        AnthropicSettings::default(),
        vec![FakeAnthropicStep::respond_json(
            200,
            &message(
                "tool_use",
                json!([tool_use(INTERPRETATION_TOOL_NAME, json!([1]))]),
            ),
        )],
    );
    assert_eq!(harness.failure_kind(), FailureKind::InvalidOutput);
}

#[test]
fn custom_proposal_schema_is_enforced_for_json_schema_profiles() {
    let schema = json!({
        "type": "object",
        "properties": {
            "schema_version": { "type": "integer" },
            "annotation": {
                "type": "object",
                "properties": { "kind": { "type": "string", "enum": ["note", "action", "idea"] } },
                "required": ["kind"],
            },
            "source_spans": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "start": { "type": "integer" }, "end": { "type": "integer" } },
                    "required": ["start", "end"],
                },
            },
        },
        "required": ["schema_version"],
        "additionalProperties": false,
    });
    let settings = AnthropicSettings::default()
        .with_proposal_schema(schema.clone())
        .expect("object schema");
    let good = json!({
        "schema_version": 1,
        "annotation": { "kind": "idea" },
        "source_spans": [{ "start": 0, "end": 4 }],
    });
    let bad_inputs = [
        json!({ "schema_version": 1, "annotation": { "kind": "poem" } }),
        json!({ "schema_version": 1, "annotation": {} }),
        json!({ "schema_version": 1, "source_spans": [{ "start": 0 }] }),
        json!({ "schema_version": 1, "unexpected": true }),
    ];

    let harness = Harness::with(
        profile(StructuredOutputMode::JsonSchema),
        settings.clone(),
        vec![ok_reply(good.clone())],
    );
    assert_eq!(Value::Object(harness.run().unwrap().proposal), good);
    assert_eq!(
        body_of(&harness.transport.calls()[0])["tools"][0]["input_schema"],
        schema
    );

    for bad in bad_inputs {
        let harness = Harness::with(
            profile(StructuredOutputMode::JsonSchema),
            settings.clone(),
            vec![ok_reply(bad.clone())],
        );
        assert_eq!(harness.failure_kind(), FailureKind::InvalidOutput, "{bad}");
    }
    assert_eq!(
        AnthropicSettings::default().with_proposal_schema(json!({ "type": "array" })),
        Err(SettingsError::InvalidProposalSchema)
    );
}

// ---- HTTP status mapping ----

#[test]
fn http_statuses_map_to_shared_failures() {
    let cases = [
        (400, FailureKind::Rejected),
        (401, FailureKind::Unauthorized),
        (403, FailureKind::Unauthorized),
        (404, FailureKind::Rejected),
        (408, FailureKind::Timeout),
        (413, FailureKind::Rejected),
        (422, FailureKind::Rejected),
        (429, FailureKind::RateLimited),
        (500, FailureKind::Unavailable),
        (502, FailureKind::Unavailable),
        (503, FailureKind::Unavailable),
        (504, FailureKind::Timeout),
        (529, FailureKind::Unavailable),
        (302, FailureKind::Rejected),
    ];
    for (status, expected) in cases {
        let harness = Harness::new(vec![FakeAnthropicStep::respond(status, "")]);
        assert_eq!(harness.failure_kind(), expected, "HTTP {status}");
    }
}

#[test]
fn error_envelopes_map_to_shared_failures() {
    let cases = [
        (401, "authentication_error", FailureKind::Unauthorized),
        (403, "permission_error", FailureKind::Unauthorized),
        (429, "rate_limit_error", FailureKind::RateLimited),
        (529, "overloaded_error", FailureKind::Unavailable),
        (500, "api_error", FailureKind::Unavailable),
        (504, "timeout_error", FailureKind::Timeout),
        (400, "invalid_request_error", FailureKind::Rejected),
        (404, "not_found_error", FailureKind::Rejected),
        (413, "request_too_large", FailureKind::Rejected),
        (402, "billing_error", FailureKind::Rejected),
        (503, "unknown_future_error", FailureKind::Unavailable),
        (401, "unknown_future_error", FailureKind::Unauthorized),
    ];
    for (status, error_type, expected) in cases {
        let harness = Harness::new(vec![FakeAnthropicStep::respond_json(
            status,
            &error_envelope(error_type),
        )]);
        assert_eq!(
            harness.failure_kind(),
            expected,
            "HTTP {status} {error_type}"
        );
    }
}

#[test]
fn error_envelope_delivered_with_a_success_status_is_still_an_error() {
    let harness = Harness::new(vec![FakeAnthropicStep::respond_json(
        200,
        &error_envelope("overloaded_error"),
    )]);
    assert_eq!(harness.failure_kind(), FailureKind::Unavailable);
}

#[test]
fn auth_and_rate_limit_classes_drive_retry_decisions() {
    let unauthorized = Harness::new(vec![FakeAnthropicStep::respond_json(
        401,
        &error_envelope("authentication_error"),
    )])
    .run()
    .unwrap_err();
    assert!(!unauthorized.retriable);

    let rate_limited = Harness::new(vec![FakeAnthropicStep::respond_json(
        429,
        &error_envelope("rate_limit_error"),
    )])
    .run()
    .unwrap_err();
    assert!(rate_limited.retriable);
}

#[test]
fn failures_never_carry_provider_payloads_or_note_text() {
    let harness = Harness::new(vec![FakeAnthropicStep::respond_json(
        400,
        &error_envelope("invalid_request_error"),
    )]);
    let failure = harness.run().unwrap_err();
    let rendered = format!("{failure:?} {failure}");
    assert!(!rendered.contains("synthetic provider detail"));
    assert!(!rendered.contains(NOTE_TEXT));
}

#[test]
fn transport_level_failures_pass_through_the_shared_contract() {
    for (error, expected) in [
        (TransportError::Unavailable, FailureKind::Unavailable),
        (TransportError::Timeout, FailureKind::Timeout),
        (TransportError::Rejected, FailureKind::Rejected),
    ] {
        let harness = Harness::new(vec![FakeAnthropicStep::fail(error)]);
        assert_eq!(harness.failure_kind(), expected);
    }
}

// ---- timeout and cancellation ----

#[test]
fn transport_enforcing_the_request_timeout_reports_timeout() {
    let harness = Harness::new(vec![ok_reply(valid_proposal()).after_ms(30_001)]);
    let failure = harness.run().unwrap_err();
    assert_eq!(failure.kind, FailureKind::Timeout);
    assert!(failure.retriable);
    assert_eq!(harness.transport.calls().len(), 1);
}

#[test]
fn late_reply_from_a_transport_ignoring_the_deadline_is_discarded() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())
        .after_ms(30_001)
        .ignoring_deadline()]);
    assert_eq!(harness.failure_kind(), FailureKind::Timeout);
}

#[test]
fn reply_just_inside_the_deadline_is_accepted() {
    let harness = Harness::new(vec![ok_reply(valid_proposal()).after_ms(30_000)]);
    assert!(harness.run().is_ok());
}

#[test]
fn pre_cancelled_call_never_reaches_the_transport() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    harness.cancel.cancel();
    assert_eq!(harness.failure_kind(), FailureKind::Cancelled);
    assert!(harness.transport.calls().is_empty());
}

#[test]
fn in_flight_cancellation_is_observed_by_the_transport() {
    let harness = Harness::new(vec![ok_reply(valid_proposal()).cancelling_in_flight()]);
    let failure = harness.run().unwrap_err();
    assert_eq!(failure.kind, FailureKind::Cancelled);
    assert!(harness.cancel.is_cancelled());
    assert_eq!(harness.transport.calls().len(), 1);
}

#[test]
fn reply_from_a_transport_ignoring_cancellation_is_discarded() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())
        .cancelling_in_flight()
        .ignoring_cancellation()]);
    assert_eq!(harness.failure_kind(), FailureKind::Cancelled);
}

// ---- destination authorization ----

fn hosted_profile_authorizing(origin: &str) -> ProviderProfile {
    builder(StructuredOutputMode::JsonSchema)
        .clear_authorized_destinations()
        .authorized_destination(origin)
        .build()
        .expect("valid profile")
}

#[test]
fn unauthorized_destination_is_refused_before_any_transport_call() {
    let rejected_endpoints = [
        "https://api.anthropic.com.evil.example/v1/messages",
        "https://api.anthropic.com@evil.example/v1/messages",
        "https://evil.example/v1/messages",
        "http://api.anthropic.com/v1/messages",
        "https://api.anthropic.com:8443/v1/messages",
        "api.anthropic.com/v1/messages",
        "https://",
    ];
    for endpoint in rejected_endpoints {
        let harness = Harness::with(
            profile(StructuredOutputMode::JsonSchema),
            AnthropicSettings::default().with_endpoint(endpoint),
            vec![ok_reply(valid_proposal())],
        );
        assert_eq!(harness.failure_kind(), FailureKind::Rejected, "{endpoint}");
        assert!(harness.transport.calls().is_empty(), "{endpoint}");
    }
}

#[test]
fn profile_that_does_not_authorize_the_default_endpoint_is_refused() {
    let harness = Harness::with(
        hosted_profile_authorizing("https://proxy.example.test"),
        AnthropicSettings::default(),
        vec![ok_reply(valid_proposal())],
    );
    assert_eq!(harness.failure_kind(), FailureKind::Rejected);
    assert!(harness.transport.calls().is_empty());
}

#[test]
fn configured_endpoint_is_used_when_its_origin_is_authorized() {
    let endpoint = "https://proxy.example.test/anthropic/v1/messages";
    let harness = Harness::with(
        hosted_profile_authorizing("https://proxy.example.test"),
        AnthropicSettings::default().with_endpoint(endpoint),
        vec![ok_reply(valid_proposal())],
    );
    harness.run().expect("authorized proxy");
    assert_eq!(harness.transport.calls()[0].url, endpoint);
}

#[test]
fn default_https_port_and_case_do_not_defeat_origin_matching() {
    let harness = Harness::with(
        hosted_profile_authorizing("https://api.anthropic.com:443"),
        AnthropicSettings::default().with_endpoint("https://API.Anthropic.com/v1/messages"),
        vec![ok_reply(valid_proposal())],
    );
    assert!(harness.run().is_ok());
}

#[test]
fn profile_for_another_protocol_is_refused_before_any_transport_call() {
    let openai_profile =
        ProviderProfileBuilder::new("synthetic-openai", ProviderProtocol::OpenAi, "other-model")
            .credential_ref("credential-ref/other")
            .authorized_destination(ORIGIN)
            .capability(
                CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "fixture")
                    .with_input_size_limit(500),
            )
            .build()
            .expect("valid profile");
    let harness = Harness::with(
        openai_profile,
        AnthropicSettings::default(),
        vec![ok_reply(valid_proposal())],
    );
    assert_eq!(harness.failure_kind(), FailureKind::Rejected);
    assert!(harness.transport.calls().is_empty());
}

// ---- size bounds ----

#[test]
fn oversized_http_body_is_rejected_by_the_size_bound() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let limits = DispatchLimits {
        max_response_bytes: 40,
    };
    let failure = harness.run_with(&limits).unwrap_err();
    assert_eq!(failure.kind, FailureKind::OutputTooLarge);
}

#[test]
fn oversized_error_envelope_with_a_success_status_is_bounded_before_it_is_parsed() {
    for error_type in ["rate_limit_error", "authentication_error"] {
        let mut envelope = error_envelope(error_type);
        envelope["error"]["message"] = json!("x".repeat(200));
        let harness = Harness::new(vec![FakeAnthropicStep::respond_json(200, &envelope)]);
        let limits = DispatchLimits {
            max_response_bytes: 64,
        };
        let failure = harness.run_with(&limits).unwrap_err();
        assert_eq!(failure.kind, FailureKind::OutputTooLarge, "{error_type}");
    }
}

#[test]
fn oversized_non_success_bodies_keep_the_failure_class_of_their_status() {
    let cases = [
        (502, FailureKind::Unavailable),
        (503, FailureKind::Unavailable),
        (429, FailureKind::RateLimited),
        (401, FailureKind::Unauthorized),
        (504, FailureKind::Timeout),
    ];
    for (status, expected) in cases {
        let page = format!("<html>{}</html>", "gateway ".repeat(100));
        let harness = Harness::new(vec![FakeAnthropicStep::respond(status, page)]);
        let limits = DispatchLimits {
            max_response_bytes: 64,
        };
        let failure = harness.run_with(&limits).unwrap_err();
        assert_eq!(failure.kind, expected, "HTTP {status}");
    }
}

#[test]
fn request_never_forces_a_tool_choice() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    harness.run().expect("valid reply");
    let body = body_of(&harness.transport.calls()[0]);
    assert_eq!(body["tool_choice"]["type"], "auto");
    assert!(body["tool_choice"].get("name").is_none());
}

// ---- capability claims ----

#[test]
fn speech_capability_cannot_be_claimed_for_the_hosted_profile() {
    let result = builder(StructuredOutputMode::JsonSchema)
        .capability(CapabilityMetadata::supported(
            ProviderCapability::SpeechGeneration,
            "claimed without evidence",
        ))
        .build();
    assert_eq!(
        result.unwrap_err(),
        ProfileValidationError::CapabilityNotUsableInM1(ProviderCapability::SpeechGeneration)
    );

    let harness = Harness::with(
        builder(StructuredOutputMode::JsonSchema)
            .capability(CapabilityMetadata::unsupported(
                ProviderCapability::SpeechGeneration,
                "not offered by this adapter",
            ))
            .build()
            .expect("valid profile"),
        AnthropicSettings::default(),
        vec![ok_reply(valid_proposal())],
    );
    assert!(harness.run().is_ok());
}

#[test]
fn adapter_sources_embed_no_service_keys() {
    let sources = [
        include_str!("../../src/providers/anthropic/mod.rs"),
        include_str!("../../src/providers/anthropic/wire.rs"),
        include_str!("../../src/providers/anthropic/fake.rs"),
    ];
    for source in sources {
        assert!(!source.contains("sk-ant"));
    }
}
