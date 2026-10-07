use crate::providers::contracts::fake::{FakeProvider, FakeStep};
use crate::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, FailureKind, InterpretationRequest,
    ManualClock, ProviderCapability, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    StructuredOutputMode, TextBasis, TransportError,
};
use crate::time::TimeContext;
use chrono::{TimeZone, Utc};
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
    .credential_ref("sk-ant-v4-test-key")
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

    fn run(
        &self,
        steps: Vec<FakeStep>,
        limits: DispatchLimits,
    ) -> (
        Result<
            crate::providers::contracts::InterpretationOutput,
            crate::providers::contracts::ProviderFailure,
        >,
        FakeProvider,
    ) {
        let fake = FakeProvider::new(self.clock.clone(), steps);
        let request = request_for(&self.profile, "call mom tomorrow");
        let result = dispatch(
            &fake,
            &self.profile,
            &request,
            self.clock.as_ref(),
            &self.cancel,
            &limits,
        );
        (result, fake)
    }
}

// ---- valid structured output ----

#[test]
fn valid_structured_output_is_parsed_as_json_object() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(
            r#"{"kind":"note","text":"Call Mom","confidence":0.95}"#,
        )],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "note");
    assert_eq!(output.proposal["text"], "Call Mom");
    assert_eq!(output.proposal["confidence"], 0.95);
}

#[test]
fn empty_structured_output_is_valid() {
    let harness = Harness::new();
    let (result, _) = harness.run(vec![FakeStep::respond(r#"{}"#)], DispatchLimits::default());
    let output = result.expect("success");
    assert!(output.proposal.is_empty());
}

#[test]
fn nested_structured_output_is_preserved() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(
            r#"{"kind":"action","target":{"name":"John","type":"person"}}"#,
        )],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["kind"], "action");
    assert!(output.proposal.contains_key("target"));
}

#[test]
fn array_values_are_preserved_in_output() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(
            r#"{"kind":"note","tags":["meeting","urgent"]}"#,
        )],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert_eq!(output.proposal["tags"].as_array().unwrap().len(), 2);
    assert_eq!(output.proposal["tags"][0], "meeting");
}

// ---- invalid structured output ----

#[test]
fn invalid_json_is_rejected() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(b"not json")],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("invalid json");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn json_array_is_rejected() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(r#"[1,2,3]"#)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("array");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn json_string_is_rejected() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(r#""just a string""#)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("string");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn json_null_is_rejected() {
    let harness = Harness::new();
    let (result, _) = harness.run(vec![FakeStep::respond(b"null")], DispatchLimits::default());
    let failure = result.expect_err("null");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn json_number_is_rejected() {
    let harness = Harness::new();
    let (result, _) = harness.run(vec![FakeStep::respond(b"42")], DispatchLimits::default());
    let failure = result.expect_err("number");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

// ---- auth failure ----

#[test]
fn auth_failure_returns_unauthorized() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Unauthorized)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("unauthorized");
    assert_eq!(failure.kind, FailureKind::Unauthorized);
}

// ---- rate limit ----

#[test]
fn rate_limit_returns_rate_limited() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::RateLimited)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("rate limited");
    assert_eq!(failure.kind, FailureKind::RateLimited);
}

// ---- timeout ----

#[test]
fn timeout_is_detected_after_deadline() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::respond(r#"{"kind":"note"}"#).after_ms(30_001)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("timeout");
    assert_eq!(failure.kind, FailureKind::Timeout);
}

#[test]
fn adapter_reported_timeout_is_mapped() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Timeout)],
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
    let (result, fake) = harness.run(vec![FakeStep::respond("{}")], DispatchLimits::default());
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
    assert!(fake.calls().is_empty());
}

#[test]
fn cancellation_during_call_discards_output() {
    let harness = Harness::new();
    let (result, fake) = harness.run(
        vec![FakeStep::cancel_then_respond(r#"{"kind":"note"}"#)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn adapter_reported_cancellation_is_mapped() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Cancelled)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
}

// ---- output size bounds ----

#[test]
fn response_within_size_limit_is_accepted() {
    let harness = Harness::new();
    let response = r#"{"kind":"note","data":"x"}"#;
    let (result, _) = harness.run(
        vec![FakeStep::respond(response)],
        DispatchLimits {
            max_response_bytes: response.len(),
        },
    );
    result.expect("within limit");
}

#[test]
fn response_exceeding_size_limit_is_rejected() {
    let harness = Harness::new();
    let response = r#"{"kind":"note","data":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#;
    let limit = 10;
    let (result, _) = harness.run(
        vec![FakeStep::respond(response)],
        DispatchLimits {
            max_response_bytes: limit,
        },
    );
    let failure = result.expect_err("too large");
    assert_eq!(failure.kind, FailureKind::OutputTooLarge);
}

// ---- profile contract enforcement ----

#[test]
fn profile_version_mismatch_prevents_dispatch() {
    let harness = Harness::new();
    let newer = harness
        .profile
        .to_builder()
        .model("claude-3-opus-20240229")
        .build()
        .unwrap();
    let fake = FakeProvider::new(harness.clock.clone(), [FakeStep::respond("{}")]);
    let request = request_for(&harness.profile, "x");
    let result = dispatch(
        &fake,
        &newer,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    );
    let failure = result.expect_err("profile mismatch");
    assert_eq!(failure.kind, FailureKind::ProfileMismatch);
    assert!(fake.calls().is_empty());
}

#[test]
fn successful_dispatch_records_call_and_returns_output() {
    let harness = Harness::new();
    let (result, fake) = harness.run(
        vec![FakeStep::respond(r#"{"ok":true}"#)],
        DispatchLimits::default(),
    );
    let output = result.expect("success");
    assert!(!fake.calls().is_empty());
    assert_eq!(output.proposal["ok"], true);
}

// ---- availability errors ----

#[test]
fn unavailable_provider_returns_transient_error() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Unavailable)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("unavailable");
    assert_eq!(failure.kind, FailureKind::Unavailable);
}

#[test]
fn rejected_request_returns_permanent_error() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Rejected)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("rejected");
    assert_eq!(failure.kind, FailureKind::Rejected);
}
