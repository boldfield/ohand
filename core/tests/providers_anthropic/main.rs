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

fn text_capability() -> CapabilityMetadata {
    CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "adapter-test:anthropic",
    )
    .with_input_size_limit(200_000)
    .with_structured_output(StructuredOutputMode::JsonObject)
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
