//! Profiles and adapters for the two arms.
//!
//! Core mints a random `profile_version` for every built profile, but a resumable run must
//! present the same pin on every invocation, so the version here is a digest of everything the
//! profile pins. Changing the model, credential reference, destination or timeout therefore
//! changes the version and the executor refuses to merge the results.

use std::sync::Arc;

use ohand_core::interpretation::instructions::content_version;
use ohand_core::providers::anthropic::{
    AnthropicAdapter, AnthropicSettings, ANTHROPIC_MESSAGES_ENDPOINT,
};
use ohand_core::providers::contracts::{
    AdapterCall, CancelToken, CapabilityMetadata, Clock, DiagnosticAdapterCall, DiagnosticResponse,
    DiagnosticSetting, DiagnosticSupport, ProviderAdapter, ProviderCapability, ProviderProfile,
    ProviderProtocol, RetryPolicy, StructuredOutputMode, TransportError, PROFILE_SCHEMA_VERSION,
};
use ohand_core::providers::openai::{
    HttpTransport, OpenAiAdapter, OPENAI_CHAT_COMPLETIONS_ENDPOINT,
};
use url::Url;

use super::args::{protocol_label, ProfileSpec};
use crate::transport::HostTransport;

const PROFILE_CREATED_AT: &str = "2026-01-01T00:00:00Z";
/// Largest case text a profile accepts; the synthetic corpus stays far below it.
const INPUT_SIZE_LIMIT: usize = 8_000;

/// A profile together with the endpoint its arm contacts in live mode.
pub struct ArmProfile {
    pub spec: ProfileSpec,
    pub profile: ProviderProfile,
    /// Full request URL this arm contacts in live mode; the transport approves exactly it.
    pub endpoint: String,
    pub origin: String,
}

/// 32 hex digits of the digest of `parts`, shaped like a UUID. Only the shape matters: core
/// requires `profile_version` and the comparison identifiers to parse as UUIDs.
pub fn derived_uuid(parts: &[&str]) -> String {
    let digest = content_version(&parts.join("\u{1f}"));
    let hex = digest.trim_start_matches("sha256:");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn origin_of(endpoint: &str) -> Result<String, String> {
    let url = Url::parse(endpoint).map_err(|_| "the endpoint is not a URL".to_string())?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("the endpoint must be an https URL".to_string());
    }
    Ok(url.origin().ascii_serialization())
}

/// Endpoint replacements for fixture servers; `None` keeps the provider's real endpoint.
#[derive(Clone, Copy, Default)]
pub struct EndpointOverrides<'a> {
    pub anthropic: Option<&'a str>,
    pub openai: Option<&'a str>,
}

pub fn build_profile(
    spec: &ProfileSpec,
    timeout_seconds: u32,
    overrides: EndpointOverrides<'_>,
) -> Result<ArmProfile, String> {
    let endpoint = match spec.protocol {
        ProviderProtocol::Anthropic => overrides
            .anthropic
            .unwrap_or(ANTHROPIC_MESSAGES_ENDPOINT)
            .to_string(),
        ProviderProtocol::OpenAi => overrides
            .openai
            .unwrap_or(OPENAI_CHAT_COMPLETIONS_ENDPOINT)
            .to_string(),
        ProviderProtocol::SelfHosted => {
            return Err("self-hosted profiles are not supported by this command".to_string())
        }
    };
    let origin = origin_of(&endpoint)?;
    let protocol = protocol_label(spec.protocol);
    let profile_id = format!("{protocol}-{}", spec.model);
    let timeout = timeout_seconds.to_string();
    let profile_version = derived_uuid(&[
        "bench-profile-v1",
        &profile_id,
        &spec.model,
        &spec.credential_ref,
        &origin,
        &timeout,
    ]);
    let capability = CapabilityMetadata::supported(
        ProviderCapability::TextInterpretation,
        "bench-cli-synthetic-comparison",
    )
    .with_input_size_limit(INPUT_SIZE_LIMIT)
    .with_structured_output(StructuredOutputMode::JsonSchema);
    // Core exposes profile contents only through serde, whose validation is the same as the
    // builder's; the builder alone would mint a random version.
    let record = serde_json::json!({
        "schema_version": PROFILE_SCHEMA_VERSION,
        "profile_version": profile_version,
        "profile_id": profile_id,
        "protocol": spec.protocol,
        "endpoint": null,
        "model": spec.model,
        "credential_ref": spec.credential_ref,
        "timeout_seconds": timeout_seconds,
        "retry_policy": RetryPolicy::default(),
        "authorized_destinations": [origin],
        "capabilities": { "text_interpretation": capability },
        "created_at": PROFILE_CREATED_AT,
    });
    let profile: ProviderProfile =
        serde_json::from_value(record).map_err(|error| format!("profile is not valid: {error}"))?;
    Ok(ArmProfile {
        spec: spec.clone(),
        profile,
        endpoint,
        origin,
    })
}

/// Settings each hosted adapter can honor for a diagnostic request.
fn support_of(protocol: ProviderProtocol) -> DiagnosticSupport {
    match protocol {
        ProviderProtocol::OpenAi => DiagnosticSupport::supporting([
            DiagnosticSetting::Temperature,
            DiagnosticSetting::MaxOutputTokens,
            DiagnosticSetting::Seed,
        ]),
        _ => DiagnosticSupport::supporting([
            DiagnosticSetting::Temperature,
            DiagnosticSetting::MaxOutputTokens,
        ]),
    }
}

/// Fake mode: answers every diagnostic call with one fixed, synthetic, valid reply and has no
/// transport at all, so it cannot reach a network or a credential. It declares the settings
/// support of the protocol it stands in for, so planning and eligibility match live mode.
pub struct SyntheticAdapter {
    protocol: ProviderProtocol,
    reply: Vec<u8>,
}

impl SyntheticAdapter {
    pub fn new(protocol: ProviderProtocol, arm_label: &str) -> SyntheticAdapter {
        let reply = serde_json::json!({
            "operation": { "kind": "annotate", "note": format!("synthetic fake reply {arm_label}") }
        });
        SyntheticAdapter {
            protocol,
            reply: reply.to_string().into_bytes(),
        }
    }
}

impl ProviderAdapter for SyntheticAdapter {
    fn invoke(&self, _call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        Err(TransportError::Rejected)
    }

    fn diagnostic_support(&self) -> DiagnosticSupport {
        support_of(self.protocol)
    }

    fn invoke_diagnostic(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, TransportError> {
        if call.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        Ok(DiagnosticResponse {
            body: self.reply.clone(),
            observations: None,
        })
    }
}

/// Hosted profiles may not declare an endpoint, so the OpenAI adapter always asks for the real
/// one. This sends the request to the endpoint the arm was approved for instead, which differs
/// from the real one only when a fixture server replaces it.
struct ApprovedEndpoint {
    transport: HostTransport,
    endpoint: String,
}

impl HttpTransport for ApprovedEndpoint {
    fn post(
        &self,
        _requested_endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &CancelToken,
        clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        self.transport.post(
            &self.endpoint,
            credential_ref,
            headers,
            body,
            deadline_ms,
            cancel,
            clock,
            max_response_bytes,
        )
    }
}

/// Live mode: the real protocol adapter over the one host transport.
pub fn live_adapter(arm: &ArmProfile, transport: &HostTransport) -> Box<dyn ProviderAdapter> {
    match arm.spec.protocol {
        ProviderProtocol::OpenAi => Box::new(OpenAiAdapter::new(ApprovedEndpoint {
            transport: transport.clone(),
            endpoint: arm.endpoint.clone(),
        })),
        _ => Box::new(AnthropicAdapter::new(
            Arc::new(transport.clone()),
            AnthropicSettings::default().with_endpoint(arm.endpoint.clone()),
        )),
    }
}
