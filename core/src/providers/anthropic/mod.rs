//! Anthropic Messages API adapter for the structured interpretation capability.
//!
//! The adapter specifies the full HTTP exchange (URL, headers, body, credential attachment
//! rule, time and size bounds) and maps the HTTP response (status and error envelopes) onto the
//! shared [`TransportError`] contract. The native layer only performs the TLS request and
//! attaches the secret behind the credential reference; no key, protocol envelope or
//! protocol-specific type reaches domain state. The adapter makes no speech or transcription
//! claim: it serves only the text interpretation capability.

use std::fmt;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::interpretation::instructions::output_schema;

use super::contracts::{
    AdapterCall, CancelToken, Clock, DiagnosticAdapterCall, DiagnosticResponse, DiagnosticSetting,
    DiagnosticSupport, ProviderAdapter, ProviderCapability, ProviderProfile, ProviderProtocol,
    StructuredOutputMode, TransportError,
};

pub mod fake;
mod schema;
mod wire;

/// Public Messages endpoint. Hosted profiles cannot declare an endpoint (profile validation),
/// so this protocol default is used unless [`AnthropicSettings::endpoint`] overrides it; either
/// way its origin must be among the profile's authorized destinations on every call.
pub const ANTHROPIC_MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
/// Value of the `anthropic-version` header the documented protocol requires.
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;
/// Name of the single tool the model must call to return structured output.
pub const INTERPRETATION_TOOL_NAME: &str = "interpret";
/// Header the native layer fills with the secret behind the credential reference.
pub const CREDENTIAL_HEADER: &str = "x-api-key";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("proposal schema must be a JSON-schema object with type \"object\"")]
    InvalidProposalSchema,
    #[error("max_output_tokens must be positive")]
    InvalidMaxOutputTokens,
}

/// Non-secret protocol settings. Model, credential and timeout come from the profile.
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicSettings {
    pub endpoint: String,
    pub api_version: String,
    pub max_output_tokens: u32,
    /// Input schema of the `interpret` tool for profiles declaring `JsonSchema` output. The default
    /// is the pinned M1 output contract published with the interpretation instructions.
    pub proposal_schema: Value,
}

impl Default for AnthropicSettings {
    fn default() -> Self {
        AnthropicSettings {
            endpoint: ANTHROPIC_MESSAGES_ENDPOINT.to_string(),
            api_version: ANTHROPIC_API_VERSION.to_string(),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            proposal_schema: output_schema(),
        }
    }
}

impl AnthropicSettings {
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_max_output_tokens(mut self, tokens: u32) -> Result<Self, SettingsError> {
        if tokens == 0 {
            return Err(SettingsError::InvalidMaxOutputTokens);
        }
        self.max_output_tokens = tokens;
        Ok(self)
    }

    pub fn with_proposal_schema(mut self, schema: Value) -> Result<Self, SettingsError> {
        if schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(SettingsError::InvalidProposalSchema);
        }
        self.proposal_schema = schema;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Post,
}

/// Names a secret held by the native credential service and the header it must be placed in.
/// The secret itself never enters core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialAttachment {
    pub reference: String,
    pub header: String,
}

/// One HTTP exchange as core specifies it. Native code must not follow cross-origin redirects
/// with the credential, retry, or exceed the time and size bounds.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub method: HttpMethod,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Time remaining for this call, and the absolute deadline on the dispatch clock.
    pub timeout_ms: u64,
    pub deadline_ms: u64,
    pub max_response_bytes: usize,
    pub credential: Option<CredentialAttachment>,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("method", &self.method)
            .field("body_len", &self.body.len())
            .field("timeout_ms", &self.timeout_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// Native HTTP effect. It reports completed exchanges (any status) as `Ok` and only its own
/// failures (timeout, cancellation, no connectivity, TLS/redirect refusal) as `Err`. It must
/// observe `cancel` while the request is in flight.
pub trait AnthropicTransport: Send + Sync {
    fn send(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError>;
}

pub struct AnthropicAdapter {
    transport: Arc<dyn AnthropicTransport>,
    settings: AnthropicSettings,
}

impl AnthropicAdapter {
    pub fn new(transport: Arc<dyn AnthropicTransport>, settings: AnthropicSettings) -> Self {
        AnthropicAdapter {
            transport,
            settings,
        }
    }
}

impl AnthropicAdapter {
    /// Cancellation, protocol and destination checks shared by every call kind; returns the
    /// structured output mode the profile declares for text interpretation.
    fn check_call(
        &self,
        profile: &ProviderProfile,
        cancel: &CancelToken,
    ) -> Result<StructuredOutputMode, TransportError> {
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if profile.protocol() != ProviderProtocol::Anthropic || profile.endpoint().is_some() {
            return Err(TransportError::Rejected);
        }
        if !is_authorized(&self.settings.endpoint, profile.authorized_destinations()) {
            return Err(TransportError::Rejected);
        }
        Ok(profile
            .capability(ProviderCapability::TextInterpretation)
            .map_or(StructuredOutputMode::None, |metadata| {
                metadata.structured_output
            }))
    }

    /// Send one already-built body under the call's deadline, size bound and cancellation.
    fn exchange(
        &self,
        profile: &ProviderProfile,
        body: Vec<u8>,
        limits: ExchangeLimits<'_>,
    ) -> Result<HttpResponse, TransportError> {
        let timeout_ms = limits.deadline_ms.saturating_sub(limits.clock.now_ms());
        if timeout_ms == 0 {
            return Err(TransportError::Timeout);
        }
        let request = HttpRequest {
            url: self.settings.endpoint.clone(),
            method: HttpMethod::Post,
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "application/json".to_string()),
                (
                    "anthropic-version".to_string(),
                    self.settings.api_version.clone(),
                ),
            ],
            body,
            timeout_ms,
            deadline_ms: limits.deadline_ms,
            max_response_bytes: limits.max_response_bytes,
            credential: Some(CredentialAttachment {
                reference: profile.credential_ref().as_str().to_string(),
                header: CREDENTIAL_HEADER.to_string(),
            }),
        };

        if limits.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        let response = self.transport.send(&request, limits.cancel)?;
        if limits.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        Ok(response)
    }
}

struct ExchangeLimits<'a> {
    deadline_ms: u64,
    max_response_bytes: usize,
    cancel: &'a CancelToken,
    clock: &'a dyn Clock,
}

impl ProviderAdapter for AnthropicAdapter {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        let mode = self.check_call(call.profile, call.cancel)?;
        let body = wire::build_messages_body(call.profile, call.request, &self.settings, mode)?;
        let response = self.exchange(
            call.profile,
            body,
            ExchangeLimits {
                deadline_ms: call.deadline_ms,
                max_response_bytes: call.max_response_bytes,
                cancel: call.cancel,
                clock: call.clock,
            },
        )?;
        let schema = wire::effective_schema(mode, &self.settings);
        wire::decode_response(&response, call.max_response_bytes, &schema)
    }

    /// Temperature and output-token limit are honored as requested. Anthropic has no seed
    /// parameter, so a requested seed is refused by `dispatch_diagnostic` before any transport.
    fn diagnostic_support(&self) -> DiagnosticSupport {
        DiagnosticSupport::supporting([
            DiagnosticSetting::Temperature,
            DiagnosticSetting::MaxOutputTokens,
        ])
    }

    fn invoke_diagnostic(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, TransportError> {
        let mode = self.check_call(call.profile, call.cancel)?;
        let body = wire::build_diagnostic_body(call.profile, call.request, &self.settings, mode)?;
        let response = self.exchange(
            call.profile,
            body,
            ExchangeLimits {
                deadline_ms: call.deadline_ms,
                max_response_bytes: call.max_response_bytes,
                cancel: call.cancel,
                clock: call.clock,
            },
        )?;
        let schema = wire::effective_schema(mode, &self.settings);
        wire::decode_reply(&response, call.max_response_bytes, &schema).map(Into::into)
    }
}

/// True when the URL's origin equals one of the authorized origins (parsed, never a prefix).
fn is_authorized(url: &str, authorized_destinations: &[String]) -> bool {
    let Some(origin) = normalized_origin(url) else {
        return false;
    };
    authorized_destinations
        .iter()
        .filter_map(|destination| normalized_origin(destination))
        .any(|authorized| authorized == origin)
}

/// `https://host[:port]` in lowercase with the default port removed; `None` unless https
/// without userinfo.
fn normalized_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    if rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let authority = authority.strip_suffix(":443").unwrap_or(&authority);
    Some(format!("https://{authority}"))
}
