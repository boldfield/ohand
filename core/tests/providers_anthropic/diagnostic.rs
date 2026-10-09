//! Diagnostic calls through the real Anthropic adapter: the wire carries exactly the
//! instructions, context, model and settings the request names, and the metadata comes from
//! the same response.

use super::{
    body_of, message, ok_reply, profile, tool_use, valid_proposal, Harness, CREDENTIAL_REF, MODEL,
    NOTE_TEXT,
};
use chrono::{TimeZone, Utc};
use ohand_core::interpretation::instructions::{content_version, M1_INSTRUCTION_TEXT};
use ohand_core::providers::anthropic::fake::FakeAnthropicStep;
use ohand_core::providers::anthropic::{
    AnthropicSettings, DEFAULT_MAX_OUTPUT_TOKENS, INTERPRETATION_TOOL_NAME,
};
use ohand_core::providers::contracts::{
    dispatch, dispatch_diagnostic, DiagnosticFailure, DiagnosticFailureKind, DiagnosticOutput,
    DiagnosticRequest, DiagnosticSetting, DispatchLimits, EffectiveSetting, ErrorClass,
    FailureKind, InterpretationRequest, ProviderProfile, ReportedModel, RequestedSettings,
    SettingValue, StructuredOutputMode, TextBasis, TokenUsage, UsageAvailability,
};
use ohand_core::time::TimeContext;
use serde_json::{json, Value};
use uuid::Uuid;

const EXPERIMENT_INSTRUCTIONS: &str =
    "Synthetic experiment instructions: reply with the interpretation as one JSON object.";
const EXPERIMENT_CONTEXT: &str = r#"{"source":{"text":"call mom tomorrow at 9"},"note":"frozen"}"#;

fn time_context() -> TimeContext {
    TimeContext {
        timezone: "America/Chicago".to_string(),
        locale: "en-US".to_string(),
        reference_time: Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0).unwrap(),
        utc_offset_at_capture: -21_600,
        calendar: "gregorian".to_string(),
    }
}

fn interpretation_request(profile: &ProviderProfile, instructions: &str) -> InterpretationRequest {
    InterpretationRequest::new(
        Uuid::new_v4().to_string(),
        0,
        TextBasis::Original { item_revision: 0 },
        NOTE_TEXT,
        Uuid::new_v4().to_string(),
        content_version(instructions),
        profile,
        "route-private-name",
        time_context(),
    )
    .expect("valid request")
}

fn diagnostic_request(profile: &ProviderProfile, settings: RequestedSettings) -> DiagnosticRequest {
    DiagnosticRequest::new(
        interpretation_request(profile, EXPERIMENT_INSTRUCTIONS),
        EXPERIMENT_INSTRUCTIONS,
        EXPERIMENT_CONTEXT,
        settings,
    )
    .expect("valid diagnostic request")
}

impl Harness {
    fn diagnose(&self, settings: RequestedSettings) -> Result<DiagnosticOutput, DiagnosticFailure> {
        self.diagnose_with(settings, &DispatchLimits::default())
    }

    fn diagnose_with(
        &self,
        settings: RequestedSettings,
        limits: &DispatchLimits,
    ) -> Result<DiagnosticOutput, DiagnosticFailure> {
        dispatch_diagnostic(
            &self.adapter,
            &self.profile,
            &diagnostic_request(&self.profile, settings),
            self.clock.as_ref(),
            &self.cancel,
            limits,
        )
    }

    fn diagnostic_failure(&self) -> DiagnosticFailure {
        self.diagnose(RequestedSettings::new())
            .expect_err("expected failure")
    }
}

fn provider_failure_kind(failure: &DiagnosticFailure) -> FailureKind {
    match &failure.kind {
        DiagnosticFailureKind::Provider { failure } => failure.kind,
        other => panic!("expected a provider failure, got {other:?}"),
    }
}

fn reply_with(mutate: impl FnOnce(&mut Value)) -> FakeAnthropicStep {
    let mut reply = message(
        "tool_use",
        json!([tool_use(INTERPRETATION_TOOL_NAME, valid_proposal())]),
    );
    mutate(&mut reply);
    FakeAnthropicStep::respond_json(200, &reply)
}

// ---- what is sent ----

#[test]
fn wire_carries_the_exact_instructions_context_model_and_requested_settings() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new()
        .with_temperature_milli(700)
        .unwrap()
        .with_max_output_tokens(300)
        .unwrap();
    harness.diagnose(settings).expect("valid reply");

    let calls = harness.transport.calls();
    assert_eq!(calls.len(), 1);
    let body = body_of(&calls[0]);
    let system = body["system"].as_str().expect("system prompt");
    assert!(
        system.starts_with(EXPERIMENT_INSTRUCTIONS),
        "the experiment's instructions are sent byte for byte"
    );
    assert!(
        !system.contains(&M1_INSTRUCTION_TEXT[..40]),
        "no built-in prompt is mixed in"
    );
    let framing = &system[EXPERIMENT_INSTRUCTIONS.len()..];
    assert!(framing.starts_with("\n\n"));
    assert!(framing.contains(INTERPRETATION_TOOL_NAME));
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], EXPERIMENT_CONTEXT);
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["temperature"], json!(0.7));
    assert_eq!(body["max_tokens"], 300);
    assert_eq!(body["tool_choice"], json!({ "type": "auto" }));
    assert_eq!(body["tools"][0]["name"], INTERPRETATION_TOOL_NAME);
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "max_tokens",
            "messages",
            "model",
            "system",
            "temperature",
            "tool_choice",
            "tools"
        ]
    );
}

#[test]
fn zero_temperature_is_sent_not_dropped() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new().with_temperature_milli(0).unwrap();
    harness.diagnose(settings).expect("valid reply");
    assert_eq!(
        body_of(&harness.transport.calls()[0])["temperature"],
        json!(0.0)
    );
}

#[test]
fn unrequested_settings_are_left_to_the_provider_or_the_declared_adapter_settings() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let output = harness
        .diagnose(RequestedSettings::new())
        .expect("valid reply");
    let body = body_of(&harness.transport.calls()[0]);
    assert!(body.get("temperature").is_none(), "no hidden temperature");
    assert_eq!(body["max_tokens"], DEFAULT_MAX_OUTPUT_TOKENS);
    assert!(output.metadata.settings.is_empty());

    let tuned = Harness::with(
        profile(StructuredOutputMode::JsonSchema),
        AnthropicSettings::default()
            .with_max_output_tokens(256)
            .unwrap(),
        vec![ok_reply(valid_proposal())],
    );
    tuned
        .diagnose(RequestedSettings::new())
        .expect("valid reply");
    assert_eq!(body_of(&tuned.transport.calls()[0])["max_tokens"], 256);

    let overriding = Harness::with(
        profile(StructuredOutputMode::JsonSchema),
        AnthropicSettings::default()
            .with_max_output_tokens(256)
            .unwrap(),
        vec![ok_reply(valid_proposal())],
    );
    let settings = RequestedSettings::new().with_max_output_tokens(64).unwrap();
    overriding.diagnose(settings).expect("valid reply");
    assert_eq!(
        body_of(&overriding.transport.calls()[0])["max_tokens"],
        64,
        "a declared experiment setting wins over the adapter default"
    );
}

#[test]
fn diagnostic_tool_schema_follows_the_declared_structured_output_mode() {
    for mode in [
        StructuredOutputMode::JsonSchema,
        StructuredOutputMode::JsonObject,
        StructuredOutputMode::None,
    ] {
        let diagnostic = Harness::with(
            profile(mode),
            AnthropicSettings::default(),
            vec![ok_reply(valid_proposal())],
        );
        diagnostic.diagnose(RequestedSettings::new()).unwrap();
        let production = Harness::with(
            profile(mode),
            AnthropicSettings::default(),
            vec![ok_reply(valid_proposal())],
        );
        production.run().unwrap();
        assert_eq!(
            body_of(&diagnostic.transport.calls()[0])["tools"],
            body_of(&production.transport.calls()[0])["tools"],
            "{mode:?}"
        );
    }
}

#[test]
fn production_calls_still_use_the_pinned_prompt_and_send_no_temperature() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let request = super::request_for(&harness.profile);
    dispatch(
        &harness.adapter,
        &harness.profile,
        &request,
        harness.clock.as_ref(),
        &harness.cancel,
        &DispatchLimits::default(),
    )
    .expect("valid reply");
    let body = body_of(&harness.transport.calls()[0]);
    assert!(body["system"]
        .as_str()
        .unwrap()
        .starts_with(M1_INSTRUCTION_TEXT));
    assert!(body.get("temperature").is_none());
}

// ---- explicit unsupported settings ----

#[test]
fn a_requested_seed_is_refused_before_any_transport_call() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new()
        .with_temperature_milli(300)
        .unwrap()
        .with_seed(7);
    let failure = harness.diagnose(settings).expect_err("seed is unsupported");
    assert_eq!(
        failure.kind,
        DiagnosticFailureKind::UnsupportedSettings {
            settings: vec![DiagnosticSetting::Seed]
        }
    );
    assert_eq!(failure.usage, UsageAvailability::Unavailable);
    assert!(harness.transport.calls().is_empty());
}

#[test]
fn a_temperature_the_protocol_cannot_express_is_rejected_before_any_transport_call() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new()
        .with_temperature_milli(1001)
        .unwrap();
    let failure = harness.diagnose(settings).expect_err("out of range");
    assert_eq!(provider_failure_kind(&failure), FailureKind::Rejected);
    assert!(harness.transport.calls().is_empty());

    let at_limit = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new()
        .with_temperature_milli(1000)
        .unwrap();
    at_limit.diagnose(settings).expect("1.0 is accepted");
    assert_eq!(
        body_of(&at_limit.transport.calls()[0])["temperature"],
        json!(1.0)
    );
}

// ---- metadata from the same response ----

#[test]
fn usage_and_model_come_from_the_same_response() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let settings = RequestedSettings::new()
        .with_temperature_milli(700)
        .unwrap()
        .with_max_output_tokens(300)
        .unwrap();
    let output = harness.diagnose(settings).expect("valid reply");

    assert_eq!(
        output.metadata.usage,
        UsageAvailability::Reported {
            usage: TokenUsage {
                input_tokens: Some(42),
                output_tokens: Some(17),
            }
        }
    );
    assert_eq!(
        output.metadata.provenance.reported_model,
        ReportedModel::Pinned
    );
    assert_eq!(Value::Object(output.output.proposal), valid_proposal());
    // The response does not echo the temperature or token limit, so they stay unknown.
    assert_eq!(output.metadata.settings.len(), 2);
    for outcome in &output.metadata.settings {
        assert!(outcome.requested.is_some());
        assert_eq!(outcome.effective, EffectiveSetting::Unknown);
    }
    assert_eq!(
        output.metadata.settings[0].requested,
        Some(SettingValue::TemperatureMilli(700))
    );
}

#[test]
fn absent_or_malformed_usage_stays_unknown_never_zero() {
    let cases: Vec<(&str, FakeAnthropicStep, UsageAvailability)> = vec![
        (
            "no usage object",
            reply_with(|reply| {
                reply.as_object_mut().unwrap().remove("usage");
            }),
            UsageAvailability::Unavailable,
        ),
        (
            "empty usage object",
            reply_with(|reply| reply["usage"] = json!({})),
            UsageAvailability::Unavailable,
        ),
        (
            "non-numeric counts",
            reply_with(|reply| {
                reply["usage"] = json!({ "input_tokens": "42", "output_tokens": -3 });
            }),
            UsageAvailability::Unavailable,
        ),
        (
            "only output tokens",
            reply_with(|reply| reply["usage"] = json!({ "output_tokens": 9 })),
            UsageAvailability::Reported {
                usage: TokenUsage {
                    input_tokens: None,
                    output_tokens: Some(9),
                },
            },
        ),
    ];
    for (label, reply, expected) in cases {
        let harness = Harness::new(vec![reply]);
        let output = harness
            .diagnose(RequestedSettings::new())
            .unwrap_or_else(|failure| panic!("{label}: {failure}"));
        assert_eq!(output.metadata.usage, expected, "{label}");
    }
}

#[test]
fn model_provenance_distinguishes_absent_revision_and_different_models() {
    let cases: Vec<(&str, Option<&str>, ReportedModel)> = vec![
        ("absent", None, ReportedModel::NotReported),
        ("exact", Some(MODEL), ReportedModel::Pinned),
        (
            "dated revision",
            Some("synthetic-model-one-20260101"),
            ReportedModel::PinnedRevision,
        ),
        (
            "other model",
            Some("synthetic-model-two"),
            ReportedModel::Different,
        ),
    ];
    for (label, reported, expected) in cases {
        let harness = Harness::new(vec![reply_with(|reply| match reported {
            Some(model) => reply["model"] = json!(model),
            None => {
                reply.as_object_mut().unwrap().remove("model");
            }
        })]);
        let output = harness.diagnose(RequestedSettings::new()).unwrap();
        assert_eq!(
            output.metadata.provenance.reported_model, expected,
            "{label}"
        );
    }
}

#[test]
fn published_metadata_carries_no_provider_ids_names_or_credentials() {
    let harness = Harness::new(vec![reply_with(|reply| {
        reply["model"] = json!("https://secret.internal.example/model-x");
    })]);
    let output = harness
        .diagnose(RequestedSettings::new())
        .expect("valid reply");
    let published = serde_json::to_string(&output.metadata).unwrap();
    for forbidden in [
        "msg_synthetic_01",
        "toolu_synthetic_01",
        "secret.internal",
        CREDENTIAL_REF,
        MODEL,
        NOTE_TEXT,
    ] {
        assert!(!published.contains(forbidden), "leaked {forbidden}");
    }
    assert_eq!(
        output.metadata.provenance.reported_model,
        ReportedModel::Different
    );
}

// ---- failure paths ----

#[test]
fn unusable_content_keeps_the_usage_the_same_response_reported() {
    let cases: Vec<(&str, FakeAnthropicStep)> = vec![
        (
            "ended without a tool call",
            reply_with(|reply| {
                reply["stop_reason"] = json!("end_turn");
                reply["content"] = json!([{ "type": "text", "text": "done" }]);
            }),
        ),
        (
            "tool input violates the schema",
            reply_with(|reply| {
                reply["content"] = json!([tool_use(
                    INTERPRETATION_TOOL_NAME,
                    json!({ "operation": { "kind": "create" } })
                )]);
            }),
        ),
        (
            "two interpret calls",
            reply_with(|reply| {
                reply["content"] = json!([
                    tool_use(INTERPRETATION_TOOL_NAME, valid_proposal()),
                    tool_use(INTERPRETATION_TOOL_NAME, valid_proposal())
                ]);
            }),
        ),
    ];
    for (label, reply) in cases {
        let harness = Harness::new(vec![reply]);
        let failure = harness.diagnostic_failure();
        assert_eq!(
            provider_failure_kind(&failure),
            FailureKind::InvalidOutput,
            "{label}"
        );
        assert_eq!(
            failure.usage,
            UsageAvailability::Reported {
                usage: TokenUsage {
                    input_tokens: Some(42),
                    output_tokens: Some(17),
                }
            },
            "{label}"
        );
    }
}

#[test]
fn refusal_is_rejected_and_keeps_the_usage_the_same_response_reported() {
    let harness = Harness::new(vec![FakeAnthropicStep::respond_json(
        200,
        &message("refusal", json!([{ "type": "text", "text": "no" }])),
    )]);
    let failure = harness.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Rejected);
    assert_eq!(failure.class(), ErrorClass::Permanent);
    assert_eq!(
        failure.usage,
        UsageAvailability::Reported {
            usage: TokenUsage {
                input_tokens: Some(42),
                output_tokens: Some(17),
            }
        }
    );
}

#[test]
fn refusal_with_absent_or_malformed_usage_stays_unknown() {
    for usage in [
        None,
        Some(json!({})),
        Some(json!({ "input_tokens": "42", "output_tokens": -3 })),
    ] {
        let mut reply = message("refusal", json!([{ "type": "text", "text": "no" }]));
        match &usage {
            Some(usage) => reply["usage"] = usage.clone(),
            None => {
                reply.as_object_mut().unwrap().remove("usage");
            }
        }
        let harness = Harness::new(vec![FakeAnthropicStep::respond_json(200, &reply)]);
        let failure = harness.diagnostic_failure();
        assert_eq!(provider_failure_kind(&failure), FailureKind::Rejected);
        assert_eq!(failure.usage, UsageAvailability::Unavailable, "{usage:?}");
    }
}

#[test]
fn unparseable_bodies_and_errors_report_no_usage() {
    let cases: Vec<(&str, FakeAnthropicStep, FailureKind)> = vec![
        (
            "html",
            FakeAnthropicStep::respond(200, "<html>gateway</html>"),
            FailureKind::InvalidOutput,
        ),
        (
            "error envelope",
            FakeAnthropicStep::respond_json(
                529,
                &json!({ "type": "error", "error": { "type": "overloaded_error", "message": "x" } }),
            ),
            FailureKind::Unavailable,
        ),
        (
            "rate limit",
            FakeAnthropicStep::respond(429, "slow down"),
            FailureKind::RateLimited,
        ),
    ];
    for (label, reply, expected) in cases {
        let harness = Harness::new(vec![reply]);
        let failure = harness.diagnostic_failure();
        assert_eq!(provider_failure_kind(&failure), expected, "{label}");
        assert_eq!(failure.usage, UsageAvailability::Unavailable, "{label}");
    }
}

#[test]
fn oversized_response_is_bounded_and_reports_no_usage() {
    let harness = Harness::new(vec![ok_reply(valid_proposal())]);
    let failure = harness
        .diagnose_with(
            RequestedSettings::new(),
            &DispatchLimits {
                max_response_bytes: 64,
            },
        )
        .expect_err("body exceeds the bound");
    assert_eq!(provider_failure_kind(&failure), FailureKind::OutputTooLarge);
    assert_eq!(failure.usage, UsageAvailability::Unavailable);
}

#[test]
fn timeout_and_cancellation_follow_the_shared_contract() {
    let pre_cancelled = Harness::new(vec![ok_reply(valid_proposal())]);
    pre_cancelled.cancel.cancel();
    let failure = pre_cancelled.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Cancelled);
    assert!(pre_cancelled.transport.calls().is_empty());

    let in_flight = Harness::new(vec![ok_reply(valid_proposal()).cancelling_in_flight()]);
    let failure = in_flight.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Cancelled);
    assert_eq!(failure.usage, UsageAvailability::Unavailable);

    let ignoring = Harness::new(vec![ok_reply(valid_proposal())
        .cancelling_in_flight()
        .ignoring_cancellation()]);
    let failure = ignoring.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Cancelled);
    assert_eq!(
        failure.usage,
        UsageAvailability::Unavailable,
        "a discarded late reply discloses no usage"
    );

    let late = Harness::new(vec![ok_reply(valid_proposal())
        .after_ms(31_000)
        .ignoring_deadline()]);
    let failure = late.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Timeout);
}

#[test]
fn destination_checks_apply_to_diagnostic_calls() {
    let unauthorized = Harness::with(
        super::hosted_profile_authorizing("https://other.example"),
        AnthropicSettings::default(),
        vec![ok_reply(valid_proposal())],
    );
    let failure = unauthorized.diagnostic_failure();
    assert_eq!(provider_failure_kind(&failure), FailureKind::Rejected);
    assert!(unauthorized.transport.calls().is_empty());
}
