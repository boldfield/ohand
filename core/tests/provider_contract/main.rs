use chrono::{TimeZone, Utc};
use ohand_core::providers::contracts::fake::{FakeProvider, FakeStep};
use ohand_core::providers::contracts::{
    dispatch, CancelToken, CapabilityMetadata, DispatchLimits, ErrorClass, FailureKind,
    InterpretationRequest, ManualClock, ProfileValidationError, ProviderCapability,
    ProviderFailure, ProviderProfile, ProviderProfileBuilder, ProviderProtocol,
    RequestValidationError, RetryPolicy, StructuredOutputMode, TextBasis, TransportError,
};
use ohand_core::time::TimeContext;
use std::sync::Arc;
use uuid::Uuid;

const ENDPOINT: &str = "https://llm.example.test:8443/v1/interpret";
const ORIGIN: &str = "https://llm.example.test:8443";

fn text_capability() -> CapabilityMetadata {
    CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "adapter-test:fake")
        .with_input_size_limit(200)
        .with_structured_output(StructuredOutputMode::JsonObject)
}

fn valid_builder() -> ProviderProfileBuilder {
    ProviderProfileBuilder::new(
        "synthetic-self-hosted",
        ProviderProtocol::SelfHosted,
        "model-a",
    )
    .endpoint(ENDPOINT)
    .credential_ref("cred-ref-1")
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

// ---- profile validation ----

#[test]
fn valid_profile_exposes_declared_contract_and_mints_uuid_version() {
    let profile = profile();
    assert!(Uuid::parse_str(profile.profile_version()).is_ok());
    assert_eq!(profile.protocol(), ProviderProtocol::SelfHosted);
    assert_eq!(profile.model(), "model-a");
    assert_eq!(profile.timeout_seconds(), 30);
    assert_eq!(profile.credential_ref().as_str(), "cred-ref-1");
    assert_eq!(profile.authorized_destinations(), [ORIGIN]);
    assert_eq!(profile.endpoint(), Some(ENDPOINT));
}

#[test]
fn well_formed_authorities_are_accepted() {
    for origin in [
        "https://llm.example.test",
        "https://llm.example.test:443",
        "https://a-b.example.test:65535",
        "https://127.0.0.1:8443",
        "https://[::1]:8443",
        "https://[2001:db8::1]",
    ] {
        valid_builder()
            .clear_authorized_destinations()
            .authorized_destination(origin)
            .endpoint(format!("{origin}/v1"))
            .build()
            .unwrap_or_else(|error| panic!("{origin} should be accepted: {error}"));
    }
}

#[test]
fn hosted_profile_without_endpoint_is_valid() {
    ProviderProfileBuilder::new("hosted", ProviderProtocol::Anthropic, "model-b")
        .credential_ref("cred-ref-2")
        .authorized_destination("https://api.example.test")
        .capability(text_capability())
        .capability(CapabilityMetadata::unsupported(
            ProviderCapability::Embeddings,
            "no embeddings in M1",
        ))
        .build()
        .expect("hosted profile");
}

#[test]
fn invalid_profiles_are_rejected_with_specific_errors() {
    use ProfileValidationError as E;
    let self_hosted_unverified = || {
        valid_builder().capability(
            CapabilityMetadata::unverified(ProviderCapability::TextInterpretation, "probe not run")
                .with_input_size_limit(200),
        )
    };
    let cases: Vec<(&str, ProviderProfileBuilder, ProfileValidationError)> = vec![
        (
            "empty id",
            ProviderProfileBuilder::new("  ", ProviderProtocol::SelfHosted, "m")
                .endpoint(ENDPOINT)
                .credential_ref("c")
                .authorized_destination(ORIGIN)
                .capability(text_capability()),
            E::EmptyProfileId,
        ),
        ("empty model", valid_builder().model(""), E::EmptyModel),
        (
            "zero timeout",
            valid_builder().timeout_seconds(0),
            E::InvalidTimeout,
        ),
        (
            "huge timeout",
            valid_builder().timeout_seconds(301),
            E::InvalidTimeout,
        ),
        (
            "retry zero attempts",
            valid_builder().retry_policy(RetryPolicy {
                max_attempts: 0,
                ..RetryPolicy::default()
            }),
            E::InvalidRetryPolicy,
        ),
        (
            "retry backoff inverted",
            valid_builder().retry_policy(RetryPolicy {
                max_attempts: 2,
                initial_backoff_ms: 900,
                max_backoff_ms: 100,
            }),
            E::InvalidRetryPolicy,
        ),
        (
            "no credential",
            ProviderProfileBuilder::new("p", ProviderProtocol::SelfHosted, "m")
                .endpoint(ENDPOINT)
                .authorized_destination(ORIGIN)
                .capability(text_capability()),
            E::MissingCredentialRef,
        ),
        (
            "blank credential",
            valid_builder().credential_ref(" "),
            E::MissingCredentialRef,
        ),
        (
            "no destinations",
            valid_builder().clear_authorized_destinations(),
            E::NoAuthorizedDestinations,
        ),
        (
            "cleartext destination",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("http://llm.example.test"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with path",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:8443/v1"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with nonnumeric port",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:notaport"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with out-of-range port",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:65536"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with zero port",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:0"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with empty port",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with empty host",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://:8443"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with invalid host characters",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm_example.test:8443"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with leading hyphen label",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://-llm.example.test"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with empty label",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm..example.test"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with malformed ipv6",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://[::zz]:8443"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "destination with multiple colons",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:8443:9"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "endpoint with nonnumeric port matching destination",
            valid_builder()
                .clear_authorized_destinations()
                .authorized_destination("https://llm.example.test:notaport")
                .endpoint("https://llm.example.test:notaport/v1"),
            E::InvalidAuthorizedDestination,
        ),
        (
            "endpoint with nonnumeric port",
            valid_builder().endpoint("https://llm.example.test:notaport/v1"),
            E::InvalidEndpoint,
        ),
        (
            "self-hosted without endpoint",
            ProviderProfileBuilder::new("p", ProviderProtocol::SelfHosted, "m")
                .credential_ref("c")
                .authorized_destination(ORIGIN)
                .capability(text_capability()),
            E::MissingEndpoint,
        ),
        (
            "endpoint cleartext",
            valid_builder().endpoint("http://llm.example.test:8443/v1"),
            E::InvalidEndpoint,
        ),
        (
            "endpoint with userinfo",
            valid_builder().endpoint("https://user:pw@llm.example.test:8443/v1"),
            E::InvalidEndpoint,
        ),
        (
            "endpoint outside destinations",
            valid_builder().endpoint("https://other.example.test/v1"),
            E::EndpointNotAuthorized,
        ),
        (
            "hosted with endpoint",
            ProviderProfileBuilder::new("p", ProviderProtocol::OpenAi, "m")
                .endpoint(ENDPOINT)
                .credential_ref("c")
                .authorized_destination(ORIGIN)
                .capability(text_capability()),
            E::UnexpectedEndpoint,
        ),
        (
            "no capabilities",
            ProviderProfileBuilder::new("p", ProviderProtocol::SelfHosted, "m")
                .endpoint(ENDPOINT)
                .credential_ref("c")
                .authorized_destination(ORIGIN),
            E::MissingTextInterpretation,
        ),
        (
            "only other capability",
            ProviderProfileBuilder::new("p", ProviderProtocol::SelfHosted, "m")
                .endpoint(ENDPOINT)
                .credential_ref("c")
                .authorized_destination(ORIGIN)
                .capability(CapabilityMetadata::unsupported(
                    ProviderCapability::Embeddings,
                    "n/a",
                )),
            E::MissingTextInterpretation,
        ),
        (
            "text unsupported",
            valid_builder().capability(CapabilityMetadata::unsupported(
                ProviderCapability::TextInterpretation,
                "model lacks it",
            )),
            E::TextInterpretationUnsupported,
        ),
        (
            "supported without evidence",
            valid_builder().capability(CapabilityMetadata {
                evidence: None,
                ..text_capability()
            }),
            E::SupportedWithoutEvidence,
        ),
        (
            "unsupported without reason",
            valid_builder().capability(CapabilityMetadata {
                reason: None,
                ..CapabilityMetadata::unsupported(ProviderCapability::Embeddings, "x")
            }),
            E::UnavailableWithoutReason,
        ),
        (
            "text without size limit",
            valid_builder().capability(CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "e",
            )),
            E::MissingInputSizeLimit,
        ),
        (
            "transcription claimed supported",
            valid_builder().capability(CapabilityMetadata::supported(
                ProviderCapability::Transcription,
                "e",
            )),
            E::CapabilityNotUsableInM1(ProviderCapability::Transcription),
        ),
        (
            "hosted unverified",
            ProviderProfileBuilder::new("p", ProviderProtocol::Anthropic, "m")
                .credential_ref("c")
                .authorized_destination(ORIGIN)
                .capability(
                    CapabilityMetadata::unverified(ProviderCapability::TextInterpretation, "r")
                        .with_input_size_limit(10),
                ),
            E::UnverifiedHostedCapability,
        ),
    ];
    for (name, builder, expected) in cases {
        assert_eq!(builder.build().expect_err(name), expected, "case: {name}");
    }
    // A self-hosted profile awaiting its probe is valid (but not dispatchable; see below).
    self_hosted_unverified()
        .build()
        .expect("unverified self-hosted is valid");
}

#[test]
fn record_level_validation_rejects_bad_versions_and_key_mismatch() {
    use ohand_core::providers::contracts::ProviderProfile as P;
    let original = profile();
    let base = serde_json::to_value(&original).unwrap();

    let mut unknown_schema = base.clone();
    unknown_schema["schema_version"] = 2.into();
    assert!(serde_json::from_value::<P>(unknown_schema)
        .unwrap_err()
        .to_string()
        .contains("schema version 2"));

    let mut bad_version = base.clone();
    bad_version["profile_version"] = "not-a-uuid".into();
    assert!(serde_json::from_value::<P>(bad_version)
        .unwrap_err()
        .to_string()
        .contains("UUID"));

    let mut mismatched_key = base.clone();
    let metadata = mismatched_key["capabilities"]["text_interpretation"].clone();
    mismatched_key["capabilities"] = serde_json::json!({ "embeddings": metadata });
    assert!(serde_json::from_value::<P>(mismatched_key)
        .unwrap_err()
        .to_string()
        .contains("does not match"));

    let roundtrip: P = serde_json::from_value(base).expect("valid json roundtrips");
    assert_eq!(roundtrip, original);
}

#[test]
fn changing_a_profile_mints_a_new_version_and_leaves_the_original_intact() {
    let original = profile();
    let changed = original
        .to_builder()
        .model("model-b")
        .timeout_seconds(10)
        .build()
        .unwrap();
    assert_eq!(changed.profile_id(), original.profile_id());
    assert_ne!(changed.profile_version(), original.profile_version());
    assert_eq!(original.model(), "model-a");
    assert_eq!(original.timeout_seconds(), 30);
    assert_eq!(changed.model(), "model-b");
}

#[test]
fn profile_serialization_contains_only_the_opaque_credential_reference() {
    let json = serde_json::to_string(&profile()).unwrap();
    assert!(json.contains("cred-ref-1"));
    assert!(!json.to_lowercase().contains("secret"));
}

// ---- request contract ----

#[test]
fn request_pins_profile_and_rejects_malformed_identity() {
    let profile = profile();
    let request = request_for(&profile, "text");
    assert!(request.is_pinned_to(&profile));
    assert!(!request.is_pinned_to(&profile.to_builder().build().unwrap()));

    let build = |capture: &str, text: &str, basis: TextBasis, version: &str| {
        InterpretationRequest::new(
            capture,
            1,
            basis,
            text,
            version,
            "i-v1",
            &profile,
            "route",
            time_context(),
        )
    };
    let ok_id = Uuid::new_v4().to_string();
    assert!(build(
        "nope",
        "t",
        TextBasis::Original { item_revision: 1 },
        &ok_id
    )
    .is_err());
    assert!(build(
        &ok_id,
        "t",
        TextBasis::Original { item_revision: 1 },
        "nope"
    )
    .is_err());
    assert!(build(
        &ok_id,
        "   ",
        TextBasis::Original { item_revision: 1 },
        &ok_id
    )
    .is_err());
    let bad_correction = TextBasis::Correction {
        correction_record_id: "x".into(),
        item_revision: 1,
    };
    assert!(build(&ok_id, "t", bad_correction, &ok_id).is_err());
    let good_correction = TextBasis::Correction {
        correction_record_id: ok_id.clone(),
        item_revision: 1,
    };
    assert!(build(&ok_id, "t", good_correction, &ok_id).is_ok());
}

#[test]
fn request_rejects_inconsistent_revisions_and_profile_identity() {
    let profile = profile();
    let id = Uuid::new_v4().to_string();
    let build = |source_revision: u64, basis: TextBasis| {
        InterpretationRequest::new(
            &id,
            source_revision,
            basis,
            "t",
            &id,
            "i-v1",
            &profile,
            "route",
            time_context(),
        )
    };
    assert_eq!(
        build(2, TextBasis::Original { item_revision: 1 }).unwrap_err(),
        RequestValidationError::RevisionMismatch
    );
    let correction = TextBasis::Correction {
        correction_record_id: id.clone(),
        item_revision: 3,
    };
    assert_eq!(
        build(2, correction).unwrap_err(),
        RequestValidationError::RevisionMismatch
    );
    assert!(build(2, TextBasis::Original { item_revision: 2 }).is_ok());
}

#[test]
fn deserialized_requests_are_validated_and_cannot_omit_the_route() {
    let profile = profile();
    let request = request_for(&profile, "call mom");
    let serialized = serde_json::to_value(&request).unwrap();

    // The route is never serialized, so a round trip cannot yield a dispatchable request.
    assert!(
        serde_json::from_value::<InterpretationRequest>(serialized.clone())
            .unwrap_err()
            .to_string()
            .contains("route id")
    );

    let mut with_route = serialized.clone();
    with_route["route_id"] = "route-secret-name".into();
    let restored: InterpretationRequest = serde_json::from_value(with_route.clone()).unwrap();
    assert_eq!(restored.route_id(), "route-secret-name");
    assert!(restored.is_pinned_to(&profile));

    let mutations: Vec<(&str, serde_json::Value, &str)> = vec![
        ("capture_id", "nope".into(), "capture_id"),
        ("request_version", "nope".into(), "request_version"),
        ("profile_version", "nope".into(), "profile_version"),
        ("profile_id", " ".into(), "profile id"),
        ("text", "  ".into(), "text must not be empty"),
        ("instruction_version", "".into(), "instruction version"),
        ("route_id", " ".into(), "route id"),
        ("source_revision", 9.into(), "revision"),
    ];
    for (field, value, expected) in mutations {
        let mut mutated = with_route.clone();
        mutated[field] = value;
        let error = serde_json::from_value::<InterpretationRequest>(mutated)
            .expect_err(field)
            .to_string();
        assert!(error.contains(expected), "{field}: {error}");
    }
}

#[test]
fn route_is_never_disclosed_to_the_provider() {
    let harness = Harness::new();
    let (result, fake) = harness.run(
        vec![FakeStep::respond(r#"{"ok":true}"#)],
        DispatchLimits::default(),
    );
    result.expect("success");
    let wire = &fake.calls()[0].wire_request;
    assert!(!wire.contains("route-secret-name"));
    assert!(wire.contains("call mom tomorrow"));
    assert!(wire.contains("instructions-v1"));
    assert!(wire.contains(harness.profile.profile_version()));
}

// ---- dispatch enforcement ----

#[test]
fn success_returns_parsed_proposal_tied_to_the_request_version() {
    let harness = Harness::new();
    let fake = FakeProvider::new(
        harness.clock.clone(),
        [FakeStep::respond(r#"{"kind":"note"}"#).after_ms(1200)],
    );
    let request = request_for(&harness.profile, "hello");
    let output = dispatch(
        &fake,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .unwrap();
    assert_eq!(output.request_version, request.request_version());
    assert_eq!(output.proposal["kind"], "note");
    assert_eq!(output.elapsed_ms, 1200);
}

#[test]
fn timeout_boundary_is_enforced_from_the_profile_timeout() {
    // timeout_seconds = 30 -> 30_000 ms allowed, 30_001 ms is a timeout.
    let harness = Harness::new();
    let (at_limit, _) = harness.run(
        vec![FakeStep::respond("{}").after_ms(30_000)],
        DispatchLimits::default(),
    );
    at_limit.expect("exactly at the deadline is accepted");

    let (over, _) = harness.run(
        vec![FakeStep::respond("{}").after_ms(30_001)],
        DispatchLimits::default(),
    );
    let failure = over.expect_err("late response must not succeed");
    assert_eq!(failure.kind, FailureKind::Timeout);
    assert_eq!(failure.class, ErrorClass::Transient);
    assert!(failure.retriable);
}

#[test]
fn timeout_follows_a_changed_profile_not_a_constant() {
    let clock = Arc::new(ManualClock::new());
    let short = profile().to_builder().timeout_seconds(2).build().unwrap();
    let fake = FakeProvider::new(clock.clone(), [FakeStep::respond("{}").after_ms(2_001)]);
    let request = request_for(&short, "x");
    let result = dispatch(
        &fake,
        &short,
        &request,
        clock.as_ref(),
        &CancelToken::new(),
        &DispatchLimits::default(),
    );
    assert_eq!(failure_kind(result), FailureKind::Timeout);
    assert_eq!(fake.calls()[0].deadline_ms, 2_000);
}

#[test]
fn adapter_reported_timeout_maps_to_timeout() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Timeout)],
        DispatchLimits::default(),
    );
    assert_eq!(failure_kind(result), FailureKind::Timeout);
}

#[test]
fn pre_cancelled_token_never_invokes_the_adapter() {
    let harness = Harness::new();
    harness.cancel.cancel();
    let (result, fake) = harness.run(vec![FakeStep::respond("{}")], DispatchLimits::default());
    let failure = result.expect_err("cancelled");
    assert_eq!(failure.kind, FailureKind::Cancelled);
    assert_eq!(failure.class, ErrorClass::Cancelled);
    assert!(!failure.retriable);
    assert!(fake.calls().is_empty());
}

#[test]
fn cancellation_during_the_call_discards_the_output() {
    let harness = Harness::new();
    let (result, fake) = harness.run(
        vec![FakeStep::cancel_then_respond(r#"{"kind":"note"}"#)],
        DispatchLimits::default(),
    );
    assert_eq!(failure_kind(result), FailureKind::Cancelled);
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn adapter_reported_cancellation_maps_to_cancelled() {
    let harness = Harness::new();
    let (result, _) = harness.run(
        vec![FakeStep::fail(TransportError::Cancelled)],
        DispatchLimits::default(),
    );
    assert_eq!(failure_kind(result), FailureKind::Cancelled);
}

#[test]
fn transport_errors_map_to_normalized_classes() {
    let cases = [
        (
            TransportError::Unavailable,
            FailureKind::Unavailable,
            ErrorClass::Transient,
            true,
        ),
        (
            TransportError::RateLimited,
            FailureKind::RateLimited,
            ErrorClass::Transient,
            true,
        ),
        (
            TransportError::Unauthorized,
            FailureKind::Unauthorized,
            ErrorClass::Unauthorized,
            false,
        ),
        (
            TransportError::Rejected,
            FailureKind::Rejected,
            ErrorClass::Permanent,
            false,
        ),
    ];
    for (transport, kind, class, retriable) in cases {
        let harness = Harness::new();
        let (result, _) = harness.run(vec![FakeStep::fail(transport)], DispatchLimits::default());
        let failure = result.expect_err("transport failure");
        assert_eq!(
            (failure.kind, failure.class, failure.retriable),
            (kind, class, retriable)
        );
    }
}

#[test]
fn invalid_output_is_rejected_by_parsing_not_by_the_fake() {
    let bodies: [&[u8]; 6] = [
        b"not json {]",
        b"",
        b"[1,2]",
        b"\"text\"",
        b"null",
        &[0xff, 0xfe, 0x7b],
    ];
    for body in bodies {
        let harness = Harness::new();
        let (result, _) = harness.run(vec![FakeStep::respond(body)], DispatchLimits::default());
        let failure = result.expect_err("invalid output");
        assert_eq!(failure.kind, FailureKind::InvalidOutput, "body {body:?}");
        assert_eq!(failure.class, ErrorClass::Permanent);
        assert!(!failure.retriable);
    }
}

#[test]
fn response_size_bound_accepts_exactly_limit_and_rejects_limit_plus_one() {
    let limit = 64;
    let limits = DispatchLimits {
        max_response_bytes: limit,
    };
    let padded = |total: usize| {
        let prefix = br#"{"p":""#;
        let suffix = br#""}"#;
        let mut body = prefix.to_vec();
        body.extend(std::iter::repeat_n(
            b'a',
            total - prefix.len() - suffix.len(),
        ));
        body.extend_from_slice(suffix);
        assert_eq!(body.len(), total);
        body
    };

    let harness = Harness::new();
    let (at_limit, fake) = harness.run(vec![FakeStep::respond(padded(limit))], limits);
    at_limit.expect("exactly at the limit is accepted");
    assert_eq!(fake.calls()[0].max_response_bytes, limit);

    let (over, _) = harness.run(vec![FakeStep::respond(padded(limit + 1))], limits);
    let failure = over.expect_err("limit + 1 is rejected");
    assert_eq!(failure.kind, FailureKind::OutputTooLarge);
    assert!(
        !failure.message.contains("aaaa"),
        "payload must not be retained"
    );
}

#[test]
fn oversized_output_cannot_hide_behind_a_valid_json_or_error_path() {
    // Oversized garbage is OutputTooLarge, not InvalidOutput: the bound is checked before parsing.
    let harness = Harness::new();
    let limits = DispatchLimits {
        max_response_bytes: 8,
    };
    let (result, _) = harness.run(vec![FakeStep::respond(vec![b'x'; 9])], limits);
    assert_eq!(failure_kind(result), FailureKind::OutputTooLarge);
}

#[test]
fn input_over_the_profile_limit_is_rejected_before_any_call() {
    let harness = Harness::new();
    let fake = FakeProvider::new(harness.clock.clone(), [FakeStep::respond("{}")]);
    let at_limit = request_for(&harness.profile, &"a".repeat(200));
    let over = request_for(&harness.profile, &"a".repeat(201));
    let run = |request: &InterpretationRequest| {
        dispatch(
            &fake,
            &harness.profile,
            request,
            harness.clock.as_ref(),
            &harness.cancel,
            &DispatchLimits::default(),
        )
    };
    assert_eq!(failure_kind(run(&over)), FailureKind::InputTooLarge);
    assert!(fake.calls().is_empty());
    run(&at_limit).expect("input at the limit is dispatched");
}

#[test]
fn unverified_capability_stops_the_job_without_calling_the_provider() {
    let unverified = valid_builder()
        .capability(
            CapabilityMetadata::unverified(ProviderCapability::TextInterpretation, "probe not run")
                .with_input_size_limit(200),
        )
        .build()
        .unwrap();
    let clock = Arc::new(ManualClock::new());
    let fake = FakeProvider::new(clock.clone(), [FakeStep::respond("{}")]);
    let request = request_for(&unverified, "x");
    let result = dispatch(
        &fake,
        &unverified,
        &request,
        clock.as_ref(),
        &CancelToken::new(),
        &DispatchLimits::default(),
    );
    let failure = result.expect_err("unverified");
    assert_eq!(
        (failure.kind, failure.class),
        (FailureKind::CapabilityUnavailable, ErrorClass::Unsupported)
    );
    assert!(fake.calls().is_empty());
}

#[test]
fn request_pinned_to_another_profile_version_is_never_dispatched() {
    let harness = Harness::new();
    let newer = harness
        .profile
        .to_builder()
        .model("model-b")
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
    assert_eq!(failure_kind(result), FailureKind::ProfileMismatch);
    assert!(fake.calls().is_empty());
}

#[test]
fn each_call_is_self_contained_and_carries_no_earlier_content() {
    let harness = Harness::new();
    let fake = FakeProvider::new(
        harness.clock.clone(),
        [
            FakeStep::respond(r#"{"answer":"FIRST-RESULT"}"#),
            FakeStep::respond("{}"),
        ],
    );
    let first = request_for(&harness.profile, "first capture text");
    let second = request_for(&harness.profile, "second capture text");
    let run = |request: &InterpretationRequest| {
        dispatch(
            &fake,
            &harness.profile,
            request,
            harness.clock.as_ref(),
            &harness.cancel,
            &DispatchLimits::default(),
        )
    };
    let first_output = run(&first).unwrap();
    assert!(first_output.proposal.contains_key("answer"));
    run(&second).unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].wire_request.contains("second capture text"));
    assert!(!calls[1].wire_request.contains("first capture text"));
    assert!(!calls[1].wire_request.contains("FIRST-RESULT"));
}
