//! OpenAI Chat Completions adapter for interpretation requests.
//!
//! The adapter builds the provider request, hands it to an [`HttpTransport`], and decodes the
//! Chat Completions envelope. It never sees secret bytes: the opaque credential reference is
//! passed to the transport, which resolves it natively and attaches the `Authorization` header.
//! Deadline, cancellation, size and output validation are applied by `dispatch`.

use crate::interpretation::instructions::render_prompt;
use crate::providers::contracts::{
    AdapterCall, CallObservations, CancelToken, Clock, DiagnosticAdapterCall, DiagnosticResponse,
    DiagnosticSetting, DiagnosticSupport, DiagnosticTransportFailure, ProviderAdapter,
    ProviderProfile, SettingValue, TokenUsage, TransportError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Endpoint used for hosted OpenAI profiles, which may not declare their own endpoint.
pub const OPENAI_CHAT_COMPLETIONS_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

#[derive(Debug, Clone, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<Message>,
    // Optional settings are omitted, not defaulted, unless the caller asked for them.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    response_format: ResponseFormat,
}

#[derive(Debug, Clone, Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ResponseFormat {
    #[serde(rename = "json_object")]
    JsonObject,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAiError {
    error: ErrorDetail,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    code: Option<String>,
}

/// Native HTTP seam for one POST to the provider.
///
/// Implementations must:
/// - resolve `credential_ref` natively and attach it as the bearer `Authorization` header;
///   `headers` never carries secrets and no secret ever crosses into core;
/// - stop at `deadline_ms` on `clock` and abort when `cancel` is cancelled;
/// - read at most `max_response_bytes + 1` bytes of the response body, so an over-limit body
///   is detected without being fully materialized;
/// - return the HTTP status together with the body for every completed exchange, including
///   4xx/5xx, and report only transport-level problems as `Err`.
pub trait HttpTransport {
    #[allow(clippy::too_many_arguments)]
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &CancelToken,
        clock: &dyn Clock,
        max_response_bytes: u64,
    ) -> Result<(u16, Vec<u8>), TransportError>;
}

pub struct OpenAiAdapter<T: HttpTransport> {
    transport: T,
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    pub fn new(transport: T) -> Self {
        OpenAiAdapter { transport }
    }
}

/// The parts of an adapter call that are the same for production and diagnostic requests.
struct CallEnvelope<'a> {
    profile: &'a ProviderProfile,
    deadline_ms: u64,
    max_response_bytes: usize,
    cancel: &'a CancelToken,
    clock: &'a dyn Clock,
}

/// A decoded exchange: either a completed answer, or a success body over the response limit
/// that is handed back untouched so that `dispatch` reports `OutputTooLarge`.
enum Exchange {
    Completed(Completion),
    OverLimit(Vec<u8>),
}

struct Completion {
    content: String,
    usage: Option<TokenUsage>,
    model: Option<String>,
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    fn exchange(
        &self,
        call: &CallEnvelope<'_>,
        request: &OpenAiRequest,
    ) -> Result<Exchange, DiagnosticTransportFailure> {
        let endpoint = call
            .profile
            .endpoint()
            .unwrap_or(OPENAI_CHAT_COMPLETIONS_ENDPOINT);
        let credential_ref = call.profile.credential_ref().as_str();
        let body = serde_json::to_vec(request).map_err(|_| TransportError::Rejected)?;

        let max_response_bytes = call.max_response_bytes as u64;
        let (status, response_bytes) = self.transport.post(
            endpoint,
            credential_ref,
            &[("Content-Type", "application/json")],
            body,
            call.deadline_ms,
            call.cancel,
            call.clock,
            max_response_bytes,
        )?;

        let within_bound = response_bytes.len() as u64 <= max_response_bytes;
        if (200..300).contains(&status) && !within_bound {
            return Ok(Exchange::OverLimit(response_bytes));
        }
        decode_response(status, &response_bytes, within_bound).map(Exchange::Completed)
    }
}

impl<T: HttpTransport> ProviderAdapter for OpenAiAdapter<T> {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        // The pinned instructions and context document are rendered by the interpretation
        // module; a request for any other instruction version is rejected, not re-framed.
        let prompt = render_prompt(call.request).map_err(|_| TransportError::Rejected)?;
        let openai_request = OpenAiRequest {
            model: call.profile.model().to_string(),
            messages: vec![
                Message {
                    role: "system",
                    content: prompt.system,
                },
                Message {
                    role: "user",
                    content: prompt.user,
                },
            ],
            temperature: Some(0.0),
            max_completion_tokens: None,
            seed: None,
            response_format: ResponseFormat::JsonObject,
        };
        let envelope = CallEnvelope {
            profile: call.profile,
            deadline_ms: call.deadline_ms,
            max_response_bytes: call.max_response_bytes,
            cancel: call.cancel,
            clock: call.clock,
        };
        match self
            .exchange(&envelope, &openai_request)
            .map_err(|failure| failure.error)?
        {
            Exchange::Completed(completion) => Ok(completion.content.into_bytes()),
            Exchange::OverLimit(body) => Ok(body),
        }
    }

    fn diagnostic_support(&self) -> DiagnosticSupport {
        DiagnosticSupport::supporting([
            DiagnosticSetting::Temperature,
            DiagnosticSetting::MaxOutputTokens,
            DiagnosticSetting::Seed,
        ])
    }

    fn invoke_diagnostic(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, TransportError> {
        self.invoke_diagnostic_observed(call)
            .map_err(|failure| failure.error)
    }

    fn invoke_diagnostic_observed(
        &self,
        call: &DiagnosticAdapterCall<'_>,
    ) -> Result<DiagnosticResponse, DiagnosticTransportFailure> {
        // The caller's instructions and context are sent verbatim; nothing is re-rendered and
        // no setting is added that the caller did not request.
        let mut openai_request = OpenAiRequest {
            model: call.profile.model().to_string(),
            messages: vec![
                Message {
                    role: "system",
                    content: call.request.instructions().to_string(),
                },
                Message {
                    role: "user",
                    content: call.request.context().to_string(),
                },
            ],
            temperature: None,
            max_completion_tokens: None,
            seed: None,
            response_format: ResponseFormat::JsonObject,
        };
        for value in call.request.settings().values() {
            match value {
                SettingValue::TemperatureMilli(milli) => {
                    openai_request.temperature = Some(f64::from(milli) / 1000.0)
                }
                SettingValue::MaxOutputTokens(tokens) => {
                    openai_request.max_completion_tokens = Some(tokens)
                }
                SettingValue::Seed(seed) => openai_request.seed = Some(seed),
            }
        }
        let envelope = CallEnvelope {
            profile: call.profile,
            deadline_ms: call.deadline_ms,
            max_response_bytes: call.max_response_bytes,
            cancel: call.cancel,
            clock: call.clock,
        };
        match self.exchange(&envelope, &openai_request)? {
            Exchange::Completed(completion) => Ok(DiagnosticResponse {
                body: completion.content.into_bytes(),
                // OpenAI does not echo temperature, seed or token limits, so no effective
                // setting is claimed: they stay unknown.
                observations: Some(CallObservations {
                    effective_settings: Vec::new(),
                    usage: completion.usage,
                    reported_model: completion.model,
                }),
            }),
            Exchange::OverLimit(body) => Ok(DiagnosticResponse {
                body,
                observations: None,
            }),
        }
    }
}

fn decode_response(
    status: u16,
    response_bytes: &[u8],
    within_bound: bool,
) -> Result<Completion, DiagnosticTransportFailure> {
    if !(200..300).contains(&status) {
        let error_code = if within_bound {
            serde_json::from_slice::<OpenAiError>(response_bytes)
                .ok()
                .and_then(|parsed| parsed.error.code)
        } else {
            None
        };
        return Err(map_error(status, error_code.as_deref()).into());
    }

    let envelope: Value =
        serde_json::from_slice(response_bytes).map_err(|_| TransportError::InvalidOutput)?;
    if !envelope.is_object() {
        return Err(TransportError::InvalidOutput.into());
    }
    // Usage and model the provider reported in this response stay attached to every failure
    // below, so a refused or unusable reply still accounts for the tokens it consumed. They
    // are read from the raw envelope so a malformed choice or observation cannot drop them.
    let usage = envelope.get("usage").map(|usage| TokenUsage {
        input_tokens: usage.get("prompt_tokens").and_then(Value::as_u64),
        output_tokens: usage.get("completion_tokens").and_then(Value::as_u64),
    });
    let model = envelope
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let failure_with_observations = |error: TransportError| {
        DiagnosticTransportFailure::with_observations(
            error,
            Some(CallObservations {
                effective_settings: Vec::new(),
                usage,
                reported_model: model.clone(),
            }),
        )
    };
    let choice: Choice = envelope
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| serde_json::from_value(choice.clone()).ok())
        .ok_or_else(|| failure_with_observations(TransportError::InvalidOutput))?;

    if choice.message.refusal.is_some() || choice.finish_reason.as_deref() == Some("content_filter")
    {
        return Err(failure_with_observations(TransportError::Rejected));
    }
    if choice.finish_reason.as_deref() != Some("stop") {
        // Only a completed answer may become a proposal: "length" is truncated output and
        // a missing, "tool_calls", "function_call" or unknown reason is not a final message.
        return Err(failure_with_observations(TransportError::InvalidOutput));
    }
    let content = choice
        .message
        .content
        .ok_or_else(|| failure_with_observations(TransportError::InvalidOutput))?;
    Ok(Completion {
        content,
        usage,
        model,
    })
}

/// Status decides the class; the OpenAI error code only refines it. Unexpected 4xx responses
/// (bad request, unknown model, quota exhaustion) are permanent so they are never retried.
fn map_error(status: u16, code: Option<&str>) -> TransportError {
    if code == Some("insufficient_quota") {
        return TransportError::Rejected;
    }
    match status {
        401 | 403 => TransportError::Unauthorized,
        429 => TransportError::RateLimited,
        408 | 500..=599 => TransportError::Unavailable,
        _ => TransportError::Rejected,
    }
}
