use chrono::{TimeZone, Utc};
use ohand_core::providers::anthropic::{
    AnthropicAdapter, FakeAnthropicStep, FakeAnthropicTransport,
};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, FailureKind, InterpretationRequest,
    ManualClock, ProviderCapability, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    StructuredOutputMode, TextBasis, TransportError,
};
use ohand_core::time::TimeContext;
use std::sync::Arc;
use uuid::Uuid;

const ORIGIN: &str = "https://api.anthropic.com";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

fn text_capability() -> CapabilityMetadata {
    CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "adapter-test:anthropic",
    )
    .with_input_size_limit(200_000)
    .with_structured_output(StructuredOutputMode::JsonObject)
}

fn profile_with_different_origin() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "anthropic-custom",
        ProviderProtocol::Anthropic,
        "claude-3-sonnet-20240229",
    )
    .credential_ref("anthropic-key-ref")
    .timeout_seconds(30)
    .authorized_destination("https://example.com")
    .capability(text_capability())
    .build()
    .expect("valid profile")
}

fn profile() -> ProviderProfile {
    ProviderProfileBuilder::new(
        "anthropic-hosted",
        ProviderProtocol::Anthropic,
        "claude-3-sonnet-20240229",
    )
    .credential_ref("anthropic-key-ref")
    .timeout_seconds(30)
    .authorized_destination(ORIGIN)
    .capability(text_capability())
    .build()
    .expect("valid profile")
}

fn time_context() -> TimeContext {
    TimeContext {
        timezone: "UTC".to_string(),
        locale: "en-US".to_string(),
        reference_time: Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap(),
        utc_offset_at_capture: 0,
        calendar: "gregorian".to_string(),
    }
}

fn request_for(profile: &ProviderProfile, text: &str) -> InterpretationRequest {
    InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        text,
        Uuid::new_v4().to_string(),
        "instructions-v1",
        profile,
        "route-secret-name",
        time_context(),
    )
    .expect("valid request")
}

struct Harness {
    clock: Arc<ManualClock>,
    cancel: CancelToken,
    profile: ProviderProfile,
}

impl Harness {
    fn new() -> Harness {
        Harness {
            clock: Arc::new(ManualClock::new()),
            cancel: CancelToken::new(),
            profile: profile(),
        }
    }

    fn run_anthropic(
        &self,
        steps: Vec<FakeAnthropicStep>,
        limits: DispatchLimits,
    ) -> Result<
        ohand_core::providers::contracts::InterpretationOutput,
        ohand_core::providers::contracts::ProviderFailure,
    > {
        let fake_transport = FakeAnthropicTransport::new(steps);
        let adapter = AnthropicAdapter::new(Box::new(fake_transport));
        let request = request_for(&self.profile, "call mom tomorrow");
        dispatch(
            &adapter,
            &self.profile,
            &request,
            self.clock.as_ref(),
            &self.cancel,
            &limits,
        )
    }
}

// ---- valid structured output ----

#[test]
fn valid_json_object_from_text_block() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"kind\":\"note\",\"text\":\"Call Mom\",\"confidence\":0.95}"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "note");
    assert_eq!(output.proposal["text"], "Call Mom");
    assert_eq!(output.proposal["confidence"], 0.95);
}

#[test]
fn valid_json_object_from_tool_use_block() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "tool_use", "id": "tool_1", "name": "interpret", "input": {"kind":"action","target":"mom"}}
        ],
        "stop_reason": "tool_use"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "action");
    assert_eq!(output.proposal["target"], "mom");
}

#[test]
fn empty_json_object_is_valid() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{}"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert!(output.proposal.is_empty());
}

#[test]
fn nested_structured_output_is_preserved() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"kind\":\"action\",\"target\":{\"name\":\"John\",\"type\":\"person\"}}"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "action");
    assert!(output.proposal.contains_key("target"));
}

// ---- invalid structured output ----

#[test]
fn invalid_envelope_is_rejected() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(b"not json")],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("invalid json");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn missing_content_array_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("missing content");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn empty_content_array_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("empty content");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn text_block_with_non_json_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "just plain text"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("non-json text");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn text_block_with_json_array_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "[1,2,3]"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("array");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn max_tokens_stop_reason_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"kind\":\"note\"}"}
        ],
        "stop_reason": "max_tokens"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("max_tokens");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

// ---- auth failure ----

#[test]
fn auth_failure_returns_unauthorized() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::fail(TransportError::Unauthorized)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("unauthorized");
    assert_eq!(failure.kind, FailureKind::Unauthorized);
}

// ---- rate limit ----

#[test]
fn rate_limit_returns_rate_limited() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::fail(TransportError::RateLimited)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("rate limited");
    assert_eq!(failure.kind, FailureKind::RateLimited);
}

// ---- timeout ----

#[test]
fn adapter_reported_timeout_is_mapped() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::fail(TransportError::Timeout)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("timeout");
    assert_eq!(failure.kind, FailureKind::Timeout);
}

// ---- cancellation ----

#[test]
fn pre_cancelled_token_prevents_adapter_invocation() {
    let harness = Harness::new();
    harness.cancel.cancel();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond("{}")],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
}

#[test]
fn adapter_reported_cancellation_is_mapped() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::fail(TransportError::Cancelled)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
}

// ---- output size bounds ----

#[test]
fn response_exceeding_size_limit_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"data\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits {
            max_response_bytes: 10,
        },
    );
    let failure = result.expect_err("too large");
    assert_eq!(failure.kind, FailureKind::OutputTooLarge);
}

// ---- availability errors ----

#[test]
fn unavailable_provider_returns_transient_error() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::fail(TransportError::Unavailable)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("unavailable");
    assert_eq!(failure.kind, FailureKind::Unavailable);
}

// ---- request building verification ----

#[test]
fn request_includes_tool_use_for_structured_output() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "call mom");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    let body_str = String::from_utf8_lossy(&call.body);
    assert!(
        body_str.contains("\"tools\""),
        "Request should include tools"
    );
    assert!(
        body_str.contains("\"tool_choice\""),
        "Request should include tool_choice"
    );
}

#[test]
fn request_includes_system_prompt_with_context() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "test");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    let body_str = String::from_utf8_lossy(&call.body);
    assert!(
        body_str.contains("\"system\""),
        "Request should include system prompt"
    );
    assert!(
        body_str.contains("interpreter"),
        "System prompt should mention interpreter"
    );
    assert!(
        body_str.contains("instructions-v1"),
        "System prompt should include instruction version"
    );
    assert!(
        body_str.contains("UTC"),
        "System prompt should include timezone"
    );
}

#[test]
fn request_uses_profile_endpoint_and_credential() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "test");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.endpoint, ENDPOINT);
    assert_eq!(call.api_key_ref, "anthropic-key-ref");
}

#[test]
fn refusal_stop_reason_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"kind\":\"note\"}"}
        ],
        "stop_reason": "refusal"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("refusal");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn endpoint_must_be_authorized() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "test");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert!(
        harness
            .profile
            .authorized_destinations()
            .iter()
            .any(|dest| call.endpoint.starts_with(dest)),
        "Endpoint should be authorized by profile destinations"
    );
}

#[test]
fn wrong_tool_name_is_rejected() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "tool_use", "id": "tool_1", "name": "wrong_tool", "input": {"kind":"action"}}
        ],
        "stop_reason": "tool_use"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("wrong tool name");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn tool_use_is_preferred_over_preceding_text() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "{\"wrong\":true}"},
            {"type": "tool_use", "id": "tool_1", "name": "interpret", "input": {"kind":"action","target":"mom"}}
        ],
        "stop_reason": "tool_use"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "action");
    assert_eq!(output.proposal["target"], "mom");
}

#[test]
fn multiple_tool_use_blocks_uses_first_interpret() {
    let harness = Harness::new();
    let anthropic_response = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "tool_use", "id": "tool_1", "name": "other", "input": {"data":"wrong"}},
            {"type": "tool_use", "id": "tool_2", "name": "interpret", "input": {"kind":"action","target":"dad"}}
        ],
        "stop_reason": "tool_use"
    }"#;
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(anthropic_response)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "action");
    assert_eq!(output.proposal["target"], "dad");
}

#[test]
fn request_uses_profile_model() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "test");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    let body_str = String::from_utf8_lossy(&call.body);
    let body: serde_json::Value = serde_json::from_str(&body_str).expect("valid json");
    assert_eq!(
        body["model"],
        harness.profile.model(),
        "Request should use profile model"
    );
}

#[test]
fn request_body_structure_is_valid() {
    let harness = Harness::new();
    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake.clone()));
    let request = request_for(&harness.profile, "test");
    dispatch(
        &adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("success");

    let calls = fake.calls();
    let call = &calls[0];
    let body_str = String::from_utf8_lossy(&call.body);
    let body: serde_json::Value = serde_json::from_str(&body_str).expect("valid json");

    assert!(body.is_object(), "Request body should be a JSON object");
    assert!(body["model"].is_string(), "model should be a string");
    assert!(
        body["max_tokens"].is_number(),
        "max_tokens should be a number"
    );
    assert!(body["system"].is_string(), "system should be a string");
    assert!(body["messages"].is_array(), "messages should be an array");
    assert_eq!(
        body["messages"][0]["role"], "user",
        "First message role should be user"
    );
}

#[test]
fn unauthorized_endpoint_is_rejected() {
    let profile = profile_with_different_origin();
    let clock = Arc::new(ManualClock::new());
    let cancel = CancelToken::new();

    let fake = FakeAnthropicTransport::new(vec![FakeAnthropicStep::respond(
        r#"{"id":"msg_1","type":"message","content":[{"type":"tool_use","id":"t1","name":"interpret","input":{}}],"stop_reason":"tool_use"}"#,
    )]);
    let adapter = AnthropicAdapter::new(Box::new(fake));
    let request = request_for(&profile, "test");

    let result = dispatch(
        &adapter,
        &profile,
        &request,
        clock.as_ref(),
        &cancel,
        &DispatchLimits::default(),
    );

    let failure = result.expect_err("unauthorized endpoint");
    assert_eq!(failure.kind, FailureKind::Rejected);
}

#[test]
fn transport_delay_exceeding_timeout_budget_is_timeout() {
    let harness = Harness::new();
    let result = harness.run_anthropic(
        vec![FakeAnthropicStep::respond(
            r#"{"id":"msg_1","type":"message","content":[{"type":"text","text":"{}"}],"stop_reason":"end_turn"}"#,
        )
        .after_ms(31000)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("timeout from delay");
    assert_eq!(failure.kind, FailureKind::Timeout);
}
