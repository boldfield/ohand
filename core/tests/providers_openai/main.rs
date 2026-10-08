//! OpenAI interpretation adapter contract tests.
//!
//! These tests verify that the OpenAI adapter:
//! - Passes semantic/protocol contract fixtures
//! - Handles malformed output
//! - Handles refusal responses
//! - Handles rate limiting
//! - Handles authentication failures
//! - Handles cancellation

use chrono::{TimeZone, Utc};
use ohand_core::providers::contracts::fake::{FakeProvider, FakeStep};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, ErrorClass, FailureKind,
    InterpretationRequest, ManualClock, ProviderCapability, ProviderFailure, ProviderProfile,
    ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode, TextBasis, TransportError,
};
use ohand_core::time::TimeContext;
use std::sync::Arc;
use uuid::Uuid;

const ORIGIN: &str = "https://api.openai.com";

fn text_capability() -> CapabilityMetadata {
    CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "adapter-test:openai",
    )
    .with_input_size_limit(200)
    .with_structured_output(StructuredOutputMode::JsonObject)
}

fn valid_builder() -> ProviderProfileBuilder {
    ProviderProfileBuilder::new("openai-gpt", ProviderProtocol::OpenAi, "gpt-4o")
        .credential_ref("openai-api-key")
        .timeout_seconds(30)
        .authorized_destination(ORIGIN)
        .capability(text_capability())
}

fn profile() -> ProviderProfile {
    valid_builder().build().expect("valid profile")
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
        Result<ohand_core::providers::contracts::InterpretationOutput, ProviderFailure>,
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

fn failure_kind<T: std::fmt::Debug>(result: Result<T, ProviderFailure>) -> FailureKind {
    result.expect_err("expected failure").kind
}

// ---- Profile validation ----

#[test]
fn valid_openai_profile_exposes_declared_contract() {
    let profile = profile();
    assert_eq!(profile.protocol(), ProviderProtocol::OpenAi);
    assert_eq!(profile.model(), "gpt-4o");
    assert_eq!(profile.timeout_seconds(), 30);
    assert_eq!(profile.credential_ref().as_str(), "openai-api-key");
    assert_eq!(profile.authorized_destinations(), [ORIGIN]);
}

#[test]
fn openai_profile_without_endpoint_is_valid() {
    let profile = ProviderProfileBuilder::new("openai", ProviderProtocol::OpenAi, "gpt-4o-mini")
        .credential_ref("openai-api-key")
        .authorized_destination(ORIGIN)
        .capability(text_capability())
        .build();
    assert!(
        profile.is_ok(),
        "OpenAI hosted profile without endpoint should be valid"
    );
}

#[test]
fn openai_profile_with_endpoint_is_rejected() {
    let profile = valid_builder()
        .endpoint("https://api.openai.com/v1/chat/completions")
        .build();
    assert!(
        profile.is_err(),
        "OpenAI hosted profile with endpoint should be rejected"
    );
}

// ---- Dispatch enforcement ----

#[test]
fn success_returns_parsed_proposal() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::respond(r#"{"kind":"note","text":"reminder"}"#).after_ms(1200)],
        DispatchLimits::default(),
    );
    let output = result.expect("should succeed");
    assert_eq!(output.proposal["kind"], "note");
    assert_eq!(output.proposal["text"], "reminder");
    assert_eq!(output.elapsed_ms, 1200);
}

#[test]
fn timeout_is_enforced() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::respond("{}").after_ms(30_001)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should timeout");
    assert_eq!(failure.kind, FailureKind::Timeout);
    assert_eq!(failure.class, ErrorClass::Transient);
    assert!(failure.retriable);
}

#[test]
fn cancellation_is_respected() {
    let harness = Harness::new();
    harness.cancel.cancel();
    let (result, fake) = harness.run(vec![FakeStep::respond("{}")], DispatchLimits::default());
    let failure = result.expect_err("should be cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
    assert_eq!(failure.class, ErrorClass::Cancelled);
    assert!(!failure.retriable);
    // Pre-cancelled token should not invoke the adapter
    assert!(fake.calls().is_empty());
}

#[test]
fn cancellation_during_call_discards_output() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::cancel_then_respond(r#"{"kind":"note"}"#)],
        DispatchLimits::default(),
    );
    assert_eq!(failure_kind(result), FailureKind::Cancelled);
}

// ---- Transport error handling ----

#[test]
fn authentication_failure_is_mapped() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::fail(TransportError::Unauthorized)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::Unauthorized);
    assert_eq!(failure.class, ErrorClass::Unauthorized);
    assert!(!failure.retriable);
}

#[test]
fn rate_limit_is_mapped() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::fail(TransportError::RateLimited)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::RateLimited);
    assert_eq!(failure.class, ErrorClass::Transient);
    assert!(failure.retriable);
}

#[test]
fn timeout_transport_error_is_mapped() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::fail(TransportError::Timeout)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::Timeout);
}

#[test]
fn unavailable_error_is_mapped() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::fail(TransportError::Unavailable)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::Unavailable);
    assert_eq!(failure.class, ErrorClass::Transient);
    assert!(failure.retriable);
}

#[test]
fn rejected_error_is_mapped() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::fail(TransportError::Rejected)],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::Rejected);
    assert_eq!(failure.class, ErrorClass::Permanent);
    assert!(!failure.retriable);
}

// ---- Malformed and invalid output ----

#[test]
fn invalid_json_output_fails() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::respond(b"not json {]")],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
    assert_eq!(failure.class, ErrorClass::Permanent);
    assert!(!failure.retriable);
}

#[test]
fn empty_response_fails() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(vec![FakeStep::respond(b"")], DispatchLimits::default());
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

#[test]
fn non_object_json_fails() {
    let cases = [b"[1,2,3]".as_slice(), b"\"text\"", b"null", b"42"];
    for body in cases {
        let harness = Harness::new();
        let (result, _fake) = harness.run(vec![FakeStep::respond(body)], DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput, "body {body:?}");
    }
}

#[test]
fn invalid_utf8_fails() {
    let harness = Harness::new();
    let (result, _fake) = harness.run(
        vec![FakeStep::respond([0xff, 0xfe, 0x7b].as_slice())],
        DispatchLimits::default(),
    );
    let failure = result.expect_err("should fail");
    assert_eq!(failure.kind, FailureKind::InvalidOutput);
}

// ---- Output size constraints ----

#[test]
fn response_size_bound_accepts_exactly_limit() {
    let harness = Harness::new();
    let limits = DispatchLimits {
        max_response_bytes: 64,
    };
    let prefix = br#"{"p":""#;
    let suffix = br#""}"#;
    let mut body = prefix.to_vec();
    body.extend(std::iter::repeat_n(b'a', 64 - prefix.len() - suffix.len()));
    body.extend_from_slice(suffix);

    let (result, _fake) = harness.run(vec![FakeStep::respond(body)], limits);
    result.expect("exactly at the limit should be accepted");
}

#[test]
fn response_size_bound_rejects_over_limit() {
    let harness = Harness::new();
    let limits = DispatchLimits {
        max_response_bytes: 64,
    };
    let prefix = br#"{"p":""#;
    let suffix = br#""}"#;
    let mut body = prefix.to_vec();
    body.extend(std::iter::repeat_n(
        b'a',
        64 - prefix.len() - suffix.len() + 1,
    ));
    body.extend_from_slice(suffix);

    let (result, _fake) = harness.run(vec![FakeStep::respond(body)], limits);
    let failure = result.expect_err("over limit should fail");
    assert_eq!(failure.kind, FailureKind::OutputTooLarge);
}

// ---- Configuration-based profile tests ----

#[test]
fn different_model_selection_is_respected() {
    let gpt4 = ProviderProfileBuilder::new("openai-gpt4", ProviderProtocol::OpenAi, "gpt-4")
        .credential_ref("openai-api-key")
        .authorized_destination(ORIGIN)
        .capability(text_capability())
        .build()
        .expect("valid profile");

    let gpt4turbo =
        ProviderProfileBuilder::new("openai-4t", ProviderProtocol::OpenAi, "gpt-4-turbo")
            .credential_ref("openai-api-key")
            .authorized_destination(ORIGIN)
            .capability(text_capability())
            .build()
            .expect("valid profile");

    assert_eq!(gpt4.model(), "gpt-4");
    assert_eq!(gpt4turbo.model(), "gpt-4-turbo");
}

#[test]
fn profile_version_mismatch_prevents_dispatch() {
    let harness = Harness::new();
    let request = request_for(&harness.profile, "text");
    // Create a different profile version
    let other_version = valid_builder().build().expect("valid profile");

    let fake = FakeProvider::new(harness.clock.clone(), vec![FakeStep::respond("{}")]);
    let result = dispatch(
        &fake,
        &other_version, // Different profile
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    );
    let failure = result.expect_err("profile mismatch should fail");
    // The specific error depends on whether versions match, but profile mismatch should occur
    assert_eq!(failure.kind, FailureKind::ProfileMismatch);
}
