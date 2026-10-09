//! Diagnostic call contract: explicit instructions and settings, honest metadata, and
//! explicit refusal where an adapter cannot honor the request.

use super::{profile, request_for, text_capability, time_context, valid_builder, ENDPOINT, ORIGIN};
use ohand_core::interpretation::instructions::content_version;
use ohand_core::providers::contracts::fake::{FakeProvider, FakeStep};
use ohand_core::providers::contracts::{
    dispatch, dispatch_diagnostic, CallObservations, CancelToken, DiagnosticFailure,
    DiagnosticFailureKind, DiagnosticMetadata, DiagnosticOutput, DiagnosticRequest,
    DiagnosticRequestError, DiagnosticSetting, DiagnosticSupport, DispatchLimits, EffectiveSetting,
    ErrorClass, FailureKind, InterpretationRequest, ManualClock, ProviderProfile,
    ProviderProfileBuilder, ProviderProtocol, ReportedModel, RequestedSettings, SettingValue,
    TextBasis, TokenUsage, TransportError, UsageAvailability, MAX_DIAGNOSTIC_CONTEXT_BYTES,
    MAX_TEMPERATURE_MILLI,
};
use std::sync::Arc;
use uuid::Uuid;

const INSTRUCTIONS: &str = "Synthetic experiment instructions: reply with one JSON object.";
const CONTEXT: &str = r#"{"source":{"text":"call mom tomorrow"}}"#;
const PROVIDER_BODY: &str = r#"{"operation":{"kind":"annotate"}}"#;

fn all_settings() -> DiagnosticSupport {
    DiagnosticSupport::supporting([
        DiagnosticSetting::Temperature,
        DiagnosticSetting::MaxOutputTokens,
        DiagnosticSetting::Seed,
    ])
}

fn interpretation_request(profile: &ProviderProfile, instructions: &str) -> InterpretationRequest {
    InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        "call mom tomorrow",
        Uuid::new_v4().to_string(),
        content_version(instructions),
        profile,
        "route-secret-name",
        time_context(),
    )
    .expect("valid request")
}

fn requested() -> RequestedSettings {
    RequestedSettings::new()
        .with_temperature_milli(0)
        .unwrap()
        .with_max_output_tokens(512)
        .unwrap()
}

fn diagnostic_request(profile: &ProviderProfile, settings: RequestedSettings) -> DiagnosticRequest {
    DiagnosticRequest::new(
        interpretation_request(profile, INSTRUCTIONS),
        INSTRUCTIONS,
        CONTEXT,
        settings,
    )
    .expect("valid diagnostic request")
}

struct Run {
    clock: Arc<ManualClock>,
    cancel: CancelToken,
    profile: ProviderProfile,
}

impl Run {
    fn new() -> Run {
        Run {
            clock: Arc::new(ManualClock::new()),
            cancel: CancelToken::new(),
            profile: profile(),
        }
    }

    fn fake(&self, steps: Vec<FakeStep>) -> FakeProvider {
        FakeProvider::new(self.clock.clone(), steps).with_diagnostic_support(all_settings())
    }

    fn call(
        &self,
        fake: &FakeProvider,
        settings: RequestedSettings,
    ) -> Result<DiagnosticOutput, DiagnosticFailure> {
        dispatch_diagnostic(
            fake,
            &self.profile,
            &diagnostic_request(&self.profile, settings),
            self.clock.as_ref(),
            &self.cancel,
            &DispatchLimits::default(),
        )
    }
}

fn provider_failure_kind(failure: &DiagnosticFailure) -> FailureKind {
    match &failure.kind {
        DiagnosticFailureKind::Provider { failure } => failure.kind,
        other => panic!("expected a provider failure, got {other:?}"),
    }
}

#[test]
fn explicit_instructions_context_and_settings_reach_the_adapter_unchanged() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    run.call(&fake, requested()).expect("diagnostic succeeds");

    assert!(fake.calls().is_empty(), "production path must not be used");
    let calls = fake.diagnostic_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].instructions, INSTRUCTIONS);
    assert_eq!(calls[0].context, CONTEXT);
    assert_eq!(
        calls[0].requested_settings,
        vec![
            SettingValue::TemperatureMilli(0),
            SettingValue::MaxOutputTokens(512)
        ]
    );
    assert!(
        !calls[0].wire_request.contains("route-secret-name"),
        "route is never disclosed"
    );
}

#[test]
fn metadata_distinguishes_requested_effective_and_unknown_settings() {
    let run = Run::new();
    let observations = CallObservations {
        effective_settings: vec![SettingValue::TemperatureMilli(300), SettingValue::Seed(7)],
        usage: Some(TokenUsage {
            input_tokens: Some(120),
            output_tokens: None,
        }),
        reported_model: Some("model-a-2026-01-05".to_string()),
    };
    let fake = run.fake(vec![
        FakeStep::respond(PROVIDER_BODY).with_observations(observations)
    ]);
    let outcome = run.call(&fake, requested()).expect("diagnostic succeeds");
    let metadata = outcome.metadata;

    let by_setting = |setting: DiagnosticSetting| {
        metadata
            .settings
            .iter()
            .find(|outcome| outcome.setting == setting)
            .unwrap_or_else(|| panic!("{setting:?} missing"))
    };
    let temperature = by_setting(DiagnosticSetting::Temperature);
    assert_eq!(
        temperature.requested,
        Some(SettingValue::TemperatureMilli(0))
    );
    assert_eq!(
        temperature.effective,
        EffectiveSetting::Reported {
            value: SettingValue::TemperatureMilli(300)
        }
    );
    let max_tokens = by_setting(DiagnosticSetting::MaxOutputTokens);
    assert_eq!(
        max_tokens.requested,
        Some(SettingValue::MaxOutputTokens(512))
    );
    assert_eq!(max_tokens.effective, EffectiveSetting::Unknown);
    let seed = by_setting(DiagnosticSetting::Seed);
    assert_eq!(seed.requested, None);
    assert_eq!(
        seed.effective,
        EffectiveSetting::Reported {
            value: SettingValue::Seed(7)
        }
    );
    assert_eq!(
        metadata.usage,
        UsageAvailability::Reported {
            usage: TokenUsage {
                input_tokens: Some(120),
                output_tokens: None
            }
        }
    );
    assert_eq!(
        metadata.provenance.reported_model,
        ReportedModel::PinnedRevision
    );
    assert_eq!(
        metadata.provenance.profile_version,
        run.profile.profile_version()
    );
    assert_eq!(outcome.output.proposal["operation"]["kind"], "annotate");
}

#[test]
fn absent_provider_metadata_stays_unknown_and_never_becomes_zero_usage() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    let metadata = run
        .call(&fake, requested())
        .expect("diagnostic succeeds")
        .metadata;

    assert_eq!(metadata.usage, UsageAvailability::Unavailable);
    assert_eq!(
        metadata.provenance.reported_model,
        ReportedModel::NotReported
    );
    assert_eq!(metadata.settings.len(), 2);
    assert!(metadata
        .settings
        .iter()
        .all(|outcome| outcome.effective == EffectiveSetting::Unknown));
    let json = serde_json::to_value(&metadata).unwrap();
    assert_eq!(
        json["usage"],
        serde_json::json!({"availability": "unavailable"})
    );
}

#[test]
fn usage_with_no_counts_is_unavailable_not_reported() {
    let run = Run::new();
    let observations = CallObservations {
        usage: Some(TokenUsage::default()),
        ..CallObservations::default()
    };
    let fake = run.fake(vec![
        FakeStep::respond(PROVIDER_BODY).with_observations(observations)
    ]);
    let metadata = run.call(&fake, requested()).unwrap().metadata;
    assert_eq!(metadata.usage, UsageAvailability::Unavailable);
}

#[test]
fn public_metadata_carries_no_credentials_endpoints_or_unsafe_model_names() {
    let run = Run::new();
    let observations = CallObservations {
        reported_model: Some("https://llm.example.test:8443/secret?key=abc".to_string()),
        ..CallObservations::default()
    };
    let fake = run.fake(vec![
        FakeStep::respond(PROVIDER_BODY).with_observations(observations)
    ]);
    let metadata = run.call(&fake, requested()).unwrap().metadata;
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Different);

    let rendered = serde_json::to_string(&metadata).unwrap();
    for forbidden in [
        "cred-ref-1",
        "llm.example.test",
        "route-secret-name",
        "key=abc",
    ] {
        assert!(!rendered.contains(forbidden), "{forbidden} leaked");
    }
    let too_long = CallObservations {
        reported_model: Some("m".repeat(129)),
        ..CallObservations::default()
    };
    let fake = run.fake(vec![
        FakeStep::respond(PROVIDER_BODY).with_observations(too_long)
    ]);
    let metadata = run.call(&fake, requested()).unwrap().metadata;
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Different);
}

fn reported_model_metadata(run: &Run, reported_model: &str) -> DiagnosticMetadata {
    let observations = CallObservations {
        reported_model: Some(reported_model.to_string()),
        ..CallObservations::default()
    };
    let fake = run.fake(vec![
        FakeStep::respond(PROVIDER_BODY).with_observations(observations)
    ]);
    run.call(&fake, requested()).unwrap().metadata
}

#[test]
fn reported_models_unrelated_to_the_pinned_model_are_withheld() {
    let run = Run::new();
    let unsafe_names = [
        "https://llm.internal.test:8443/private-model",
        "llm.internal.test:8443/private-model",
        "llm.internal.test",
        "llm-proxy.corp.example.com",
        "internal-gw.example.com",
        "prod-llm.acme.net",
        "api2.internal.test",
        "llm.corp7.example",
        "10.0.0.1",
        "192.168.1.20",
        "localhost-model",
        "sk-abc123secret",
        "sess_abcdef0123456789",
        "apikey-0123abcd",
        "pat-1234567890abcdefghij",
        "x-request-0123456789abcdef",
        "req_01H8XYZ",
        "chatcmpl-9fA3kLm",
        "123e4567-e89b-12d3-a456-426614174000",
        "/private-model",
        "org/model",
        "model-b",
    ];
    for unsafe_name in unsafe_names {
        let metadata = reported_model_metadata(&run, unsafe_name);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::Different,
            "{unsafe_name} must be recorded as a different model"
        );
        assert!(
            !serde_json::to_string(&metadata)
                .unwrap()
                .contains(unsafe_name),
            "{unsafe_name} leaked"
        );
    }
}

#[test]
fn pinned_model_extended_only_by_date_or_short_revision_suffixes_is_recognized() {
    let run = Run::new();
    let metadata = reported_model_metadata(&run, "model-a");
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Pinned);
    for revision in [
        "model-a-20250929",
        "model-a-2026-01-05",
        "model-a-0125",
        "model-a-1",
    ] {
        let metadata = reported_model_metadata(&run, revision);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::PinnedRevision,
            "{revision}"
        );
        assert!(!serde_json::to_string(&metadata).unwrap().contains(revision));
    }
    for suffix_shaped_identifier in [
        "model-a-0123456789abcdef",
        "model-a-12345",
        "model-a-1234567890123456",
        "model-a-2026-01",
        "model-a-latest",
        "model-a.internal.test",
        "model-a-",
    ] {
        let metadata = reported_model_metadata(&run, suffix_shaped_identifier);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::Different,
            "{suffix_shaped_identifier} must not count as the pinned model"
        );
    }
}

/// Profiles accept any non-empty model string, so the pinned model may be far longer than any
/// bound a provider-controlled echo could reasonably be held to. Equality and revision suffixes
/// are judged relative to the pinned model, never against the reported name's total length.
#[test]
fn long_pinned_models_are_classified_by_relationship_not_total_length() {
    let long_pinned_model = format!("synthetic-long-model-{}", "x".repeat(200));
    assert!(long_pinned_model.len() > 128);

    let metadata = configured_metadata(
        "synthetic-self-hosted",
        &long_pinned_model,
        &long_pinned_model,
    );
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Pinned);
    assert_only_core_assigned_provenance(&metadata, &long_pinned_model);

    for revision_suffix in ["20250929", "2026-01-05", "0125", "1"] {
        let revision = format!("{long_pinned_model}-{revision_suffix}");
        let metadata = configured_metadata("synthetic-self-hosted", &long_pinned_model, &revision);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::PinnedRevision,
            "{revision_suffix}"
        );
        assert_only_core_assigned_provenance(&metadata, &long_pinned_model);
    }

    for other_suffix in ["0123456789abcdef", "12345", "latest", "2026-01", ""] {
        let other = format!("{long_pinned_model}-{other_suffix}");
        let metadata = configured_metadata("synthetic-self-hosted", &long_pinned_model, &other);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::Different,
            "{other:?} must not count as the pinned model"
        );
        assert_only_core_assigned_provenance(&metadata, &long_pinned_model);
    }

    let truncated = &long_pinned_model[..128];
    let metadata = configured_metadata("synthetic-self-hosted", &long_pinned_model, truncated);
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Different);
    let unrelated_long_name = "m".repeat(long_pinned_model.len());
    let metadata = configured_metadata(
        "synthetic-self-hosted",
        &long_pinned_model,
        &unrelated_long_name,
    );
    assert_eq!(metadata.provenance.reported_model, ReportedModel::Different);
    assert!(!serde_json::to_string(&metadata)
        .unwrap()
        .contains(&unrelated_long_name));
}

fn configured_metadata(profile_id: &str, model: &str, reported_model: &str) -> DiagnosticMetadata {
    let run = Run {
        profile: ProviderProfileBuilder::new(profile_id, ProviderProtocol::SelfHosted, model)
            .endpoint(ENDPOINT)
            .credential_ref("cred-ref-1")
            .timeout_seconds(30)
            .authorized_destination(ORIGIN)
            .capability(text_capability())
            .build()
            .expect("valid profile"),
        ..Run::new()
    };
    reported_model_metadata(&run, reported_model)
}

/// Configured values a shape or vocabulary filter could mistake for plain model names, plus
/// endpoint-, credential- and request-identifier-shaped ones. Built at runtime so no
/// scanner-matching literal is committed.
fn configured_identifiers() -> Vec<String> {
    vec![
        "secretvalue".to_string(),
        "apikeyvalue".to_string(),
        "requestid".to_string(),
        "internalhost".to_string(),
        "prodserver".to_string(),
        "hunter".to_string() + "two",
        ["xoxb", "1234", "5678", "abcd"].join("-"),
        "req-raw-provider-request-id".to_string(),
        "https://llm.internal.test/private-model".to_string(),
        "llm-proxy.corp.example.com".to_string(),
        "10.0.0.1".to_string(),
        "123e4567-e89b-12d3-a456-426614174000".to_string(),
        format!("{}-{}", "sk", "fixture0123456789"),
        format!("{}_{}", "sess", "abcdef0123456789"),
        format!("{}{}", "AKIA", "FIXTURE"),
        "claude-sonnet-4-5".to_string(),
        "llama-3.1-70b-instruct".to_string(),
    ]
}

fn assert_only_core_assigned_provenance(metadata: &DiagnosticMetadata, configured: &str) {
    let rendered = serde_json::to_value(metadata).unwrap();
    let provenance = rendered["provenance"].as_object().unwrap();
    let mut fields: Vec<&str> = provenance.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(fields, ["profile_version", "protocol", "reported_model"]);
    Uuid::parse_str(provenance["profile_version"].as_str().unwrap())
        .expect("profile_version is a UUID");
    assert!(
        !rendered.to_string().contains(configured),
        "{configured} leaked"
    );
}

#[test]
fn configured_profile_ids_and_models_never_enter_public_metadata() {
    for configured in configured_identifiers() {
        let metadata = configured_metadata(&configured, "model-a", "model-a");
        assert_only_core_assigned_provenance(&metadata, &configured);

        let metadata = configured_metadata("synthetic-self-hosted", &configured, &configured);
        assert_eq!(metadata.provenance.reported_model, ReportedModel::Pinned);
        assert_only_core_assigned_provenance(&metadata, &configured);

        let revision = format!("{configured}-20250929");
        let metadata = configured_metadata("synthetic-self-hosted", &configured, &revision);
        assert_eq!(
            metadata.provenance.reported_model,
            ReportedModel::PinnedRevision
        );
        assert_only_core_assigned_provenance(&metadata, &configured);
    }
}

#[test]
fn adapters_without_diagnostic_support_are_refused_before_any_transport() {
    let run = Run::new();
    let legacy = FakeProvider::new(run.clock.clone(), vec![FakeStep::respond(PROVIDER_BODY)]);
    let failure = run.call(&legacy, RequestedSettings::new()).unwrap_err();

    assert_eq!(failure.kind, DiagnosticFailureKind::DiagnosticsUnsupported);
    assert_eq!(failure.class(), ErrorClass::Unsupported);
    assert_eq!(failure.usage, UsageAvailability::Unavailable);
    assert!(legacy.calls().is_empty(), "legacy prompt path must not run");
    assert!(legacy.diagnostic_calls().is_empty());
}

#[test]
fn unsupported_settings_fail_explicitly_instead_of_being_dropped() {
    let run = Run::new();
    let fake = FakeProvider::new(run.clock.clone(), vec![FakeStep::respond(PROVIDER_BODY)])
        .with_diagnostic_support(DiagnosticSupport::supporting([
            DiagnosticSetting::Temperature,
        ]));
    let settings = requested().with_seed(42);
    let failure = run.call(&fake, settings).unwrap_err();

    assert_eq!(
        failure.kind,
        DiagnosticFailureKind::UnsupportedSettings {
            settings: vec![DiagnosticSetting::MaxOutputTokens, DiagnosticSetting::Seed]
        }
    );
    assert_eq!(failure.class(), ErrorClass::Unsupported);
    assert!(fake.diagnostic_calls().is_empty());

    let unconstrained = run
        .call(
            &fake,
            RequestedSettings::new().with_temperature_milli(0).unwrap(),
        )
        .expect("a supported setting still works");
    assert_eq!(unconstrained.metadata.settings.len(), 1);
}

#[test]
fn diagnostic_support_does_not_change_production_dispatch() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    let request = request_for(&run.profile, "call mom tomorrow");
    let output = dispatch(
        &fake,
        &run.profile,
        &request,
        run.clock.as_ref(),
        &run.cancel,
        &DispatchLimits::default(),
    )
    .expect("production dispatch unchanged");
    assert_eq!(output.request_version, request.request_version());
    assert_eq!(fake.calls().len(), 1);
    assert!(fake.diagnostic_calls().is_empty());
}

#[test]
fn cancellation_before_dispatch_sends_nothing_and_reports_no_usage() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    run.cancel.cancel();
    let failure = run.call(&fake, requested()).unwrap_err();

    assert_eq!(provider_failure_kind(&failure), FailureKind::Cancelled);
    assert_eq!(failure.class(), ErrorClass::Cancelled);
    assert_eq!(failure.usage, UsageAvailability::Unavailable);
    assert!(fake.diagnostic_calls().is_empty());
}

#[test]
fn cancellation_in_flight_discards_output_without_inventing_usage() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::cancel_then_respond(PROVIDER_BODY)]);
    let failure = run.call(&fake, requested()).unwrap_err();

    assert_eq!(provider_failure_kind(&failure), FailureKind::Cancelled);
    assert_eq!(failure.usage, UsageAvailability::Unavailable);
    let json = serde_json::to_value(&failure).unwrap();
    assert_eq!(
        json["usage"],
        serde_json::json!({"availability": "unavailable"})
    );
}

#[test]
fn usage_the_provider_did_report_survives_a_later_failure() {
    let run = Run::new();
    let usage = TokenUsage {
        input_tokens: Some(50),
        output_tokens: Some(9),
    };
    let observations = CallObservations {
        usage: Some(usage),
        ..CallObservations::default()
    };
    let fake = run.fake(vec![
        FakeStep::respond("not json").with_observations(observations.clone()),
        FakeStep::cancel_then_respond(PROVIDER_BODY).with_observations(observations),
    ]);
    let invalid = run.call(&fake, requested()).unwrap_err();
    assert_eq!(provider_failure_kind(&invalid), FailureKind::InvalidOutput);
    assert_eq!(invalid.usage, UsageAvailability::Reported { usage });

    let cancelled = run.call(&fake, requested()).unwrap_err();
    assert_eq!(provider_failure_kind(&cancelled), FailureKind::Cancelled);
    assert_eq!(cancelled.usage, UsageAvailability::Reported { usage });
}

#[test]
fn transport_failures_and_timeouts_report_no_usage() {
    let run = Run::new();
    let fake = run.fake(vec![
        FakeStep::fail(TransportError::Unavailable),
        FakeStep::respond(PROVIDER_BODY).after_ms(31_000),
    ]);
    let unavailable = run.call(&fake, requested()).unwrap_err();
    assert_eq!(
        provider_failure_kind(&unavailable),
        FailureKind::Unavailable
    );
    assert_eq!(unavailable.class(), ErrorClass::Transient);
    assert_eq!(unavailable.usage, UsageAvailability::Unavailable);

    let timed_out = run.call(&fake, requested()).unwrap_err();
    assert_eq!(provider_failure_kind(&timed_out), FailureKind::Timeout);
    assert_eq!(timed_out.usage, UsageAvailability::Unavailable);
}

#[test]
fn output_larger_than_the_bound_is_rejected() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    let failure = dispatch_diagnostic(
        &fake,
        &run.profile,
        &diagnostic_request(&run.profile, requested()),
        run.clock.as_ref(),
        &run.cancel,
        &DispatchLimits {
            max_response_bytes: 4,
        },
    )
    .unwrap_err();
    assert_eq!(provider_failure_kind(&failure), FailureKind::OutputTooLarge);
}

#[test]
fn profile_pinning_and_input_bounds_still_apply() {
    let run = Run::new();
    let fake = run.fake(vec![FakeStep::respond(PROVIDER_BODY)]);
    let other_profile = valid_builder().build().expect("another profile version");
    let mismatched = diagnostic_request(&other_profile, requested());
    let failure = dispatch_diagnostic(
        &fake,
        &run.profile,
        &mismatched,
        run.clock.as_ref(),
        &run.cancel,
        &DispatchLimits::default(),
    )
    .unwrap_err();
    assert_eq!(
        provider_failure_kind(&failure),
        FailureKind::ProfileMismatch
    );

    let oversized = InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        "x".repeat(201),
        Uuid::new_v4().to_string(),
        content_version(INSTRUCTIONS),
        &run.profile,
        "route",
        time_context(),
    )
    .unwrap();
    let request = DiagnosticRequest::new(oversized, INSTRUCTIONS, CONTEXT, requested()).unwrap();
    let failure = dispatch_diagnostic(
        &fake,
        &run.profile,
        &request,
        run.clock.as_ref(),
        &run.cancel,
        &DispatchLimits::default(),
    )
    .unwrap_err();
    assert_eq!(provider_failure_kind(&failure), FailureKind::InputTooLarge);
    assert!(fake.diagnostic_calls().is_empty());
}

#[test]
fn diagnostic_requests_reject_unbound_or_malformed_inputs() {
    let profile = profile();
    let build = |instructions: &str, context: &str| {
        DiagnosticRequest::new(
            interpretation_request(&profile, INSTRUCTIONS),
            instructions,
            context,
            RequestedSettings::new(),
        )
        .map(|_| ())
    };
    assert_eq!(
        build("  ", CONTEXT),
        Err(DiagnosticRequestError::EmptyInstructions)
    );
    assert_eq!(
        build(INSTRUCTIONS, ""),
        Err(DiagnosticRequestError::EmptyContext)
    );
    assert_eq!(
        build("different instructions", CONTEXT),
        Err(DiagnosticRequestError::InstructionVersionMismatch)
    );
    assert_eq!(
        build(INSTRUCTIONS, &"c".repeat(MAX_DIAGNOSTIC_CONTEXT_BYTES + 1)),
        Err(DiagnosticRequestError::ContextTooLarge)
    );
    assert_eq!(
        RequestedSettings::new().with_temperature_milli(MAX_TEMPERATURE_MILLI + 1),
        Err(DiagnosticRequestError::InvalidSetting(
            DiagnosticSetting::Temperature
        ))
    );
    assert_eq!(
        RequestedSettings::new().with_max_output_tokens(0),
        Err(DiagnosticRequestError::InvalidSetting(
            DiagnosticSetting::MaxOutputTokens
        ))
    );
}
