//! Anthropic Messages API request construction and response decoding. Everything
//! protocol-specific stays in this module; callers only see the extracted proposal bytes or a
//! shared [`TransportError`].

use serde_json::{json, Value};

use super::schema::conforms;
use super::{AnthropicSettings, HttpResponse, INTERPRETATION_TOOL_NAME};
use crate::interpretation::instructions::render_prompt;
use crate::providers::contracts::{
    CallObservations, DiagnosticRequest, DiagnosticResponse, DiagnosticSetting,
    InterpretationRequest, ProviderProfile, SettingValue, StructuredOutputMode, TokenUsage,
    TransportError,
};

/// Schema the `interpret` tool advertises and the extracted input must satisfy. Only a profile
/// declaring `JsonSchema` gets the full proposal schema; other modes ask for any JSON object.
pub(super) fn effective_schema(mode: StructuredOutputMode, settings: &AnthropicSettings) -> Value {
    match mode {
        StructuredOutputMode::JsonSchema => settings.proposal_schema.clone(),
        StructuredOutputMode::JsonObject | StructuredOutputMode::None => {
            json!({ "type": "object" })
        }
    }
}

/// Protocol framing appended to the pinned instructions: the reply object is delivered as the
/// input of the single tool call.
pub(super) fn delivery_framing() -> String {
    format!(
        "DELIVERY\nDeliver the JSON object described above as the input of exactly one call to \
         the `{INTERPRETATION_TOOL_NAME}` tool, and reply with nothing else."
    )
}

/// Fields that vary between the production and the diagnostic call; everything else (tool,
/// `tool_choice`, model) is shared so both calls speak the same protocol.
struct MessageParts<'a> {
    system: String,
    user: &'a str,
    max_tokens: u32,
    /// Sent only when an experiment asked for it; otherwise the field is absent and the
    /// provider's own default applies.
    temperature_milli: Option<u32>,
}

/// `tool_choice` is `auto`: current Anthropic models reject forced tool use (`type: tool` or
/// `any`) with HTTP 400, so the system prompt instructs the call and [`decode_response`]
/// accepts only a reply that actually made exactly one `interpret` call.
fn build_body(
    profile: &ProviderProfile,
    parts: &MessageParts<'_>,
    settings: &AnthropicSettings,
    mode: StructuredOutputMode,
) -> Result<Vec<u8>, TransportError> {
    let mut body = json!({
        "model": profile.model(),
        "max_tokens": parts.max_tokens,
        "system": parts.system,
        "messages": [{ "role": "user", "content": parts.user }],
        "tools": [{
            "name": INTERPRETATION_TOOL_NAME,
            "description": "Report the interpretation of the captured note.",
            "input_schema": effective_schema(mode, settings),
        }],
        "tool_choice": { "type": "auto" },
    });
    if let Some(milli) = parts.temperature_milli {
        body["temperature"] = json!(f64::from(milli) / 1000.0);
    }
    serde_json::to_vec(&body).map_err(|_| TransportError::Rejected)
}

pub(super) fn build_messages_body(
    profile: &ProviderProfile,
    request: &InterpretationRequest,
    settings: &AnthropicSettings,
    mode: StructuredOutputMode,
) -> Result<Vec<u8>, TransportError> {
    // The pinned instructions and context document are rendered by the interpretation module; a
    // request for any other instruction version is rejected, not re-framed. Only the delivery
    // mechanism, which is specific to this protocol, is added after the pinned text.
    let prompt = render_prompt(request).map_err(|_| TransportError::Rejected)?;
    let parts = MessageParts {
        system: with_delivery(&prompt.system),
        user: &prompt.user,
        max_tokens: settings.max_output_tokens,
        temperature_milli: None,
    };
    build_body(profile, &parts, settings, mode)
}

/// The diagnostic request already carries the exact instructions and context to send, so
/// nothing is rendered here. A requested setting is sent as given; an unrequested one is not
/// invented, except `max_tokens`, which the protocol requires and which then comes from the
/// adapter's declared settings.
pub(super) fn build_diagnostic_body(
    profile: &ProviderProfile,
    request: &DiagnosticRequest,
    settings: &AnthropicSettings,
    mode: StructuredOutputMode,
) -> Result<Vec<u8>, TransportError> {
    let requested = request.settings();
    let temperature_milli = match requested.value_for(DiagnosticSetting::Temperature) {
        Some(SettingValue::TemperatureMilli(milli)) => {
            if milli > MAX_ANTHROPIC_TEMPERATURE_MILLI {
                return Err(TransportError::Rejected);
            }
            Some(milli)
        }
        _ => None,
    };
    let max_tokens = match requested.value_for(DiagnosticSetting::MaxOutputTokens) {
        Some(SettingValue::MaxOutputTokens(tokens)) => tokens,
        _ => settings.max_output_tokens,
    };
    let parts = MessageParts {
        system: with_delivery(request.instructions()),
        user: request.context(),
        max_tokens,
        temperature_milli,
    };
    build_body(profile, &parts, settings, mode)
}

/// The Messages API accepts temperatures from 0.0 to 1.0.
const MAX_ANTHROPIC_TEMPERATURE_MILLI: u32 = 1000;

fn with_delivery(instructions: &str) -> String {
    format!(
        "{instructions}\n\n{delivery}",
        delivery = delivery_framing()
    )
}

/// Map an Anthropic error envelope type to a shared error; `None` for unknown types.
fn map_error_type(error_type: &str) -> Option<TransportError> {
    match error_type {
        "authentication_error" | "permission_error" => Some(TransportError::Unauthorized),
        "rate_limit_error" => Some(TransportError::RateLimited),
        "api_error" | "overloaded_error" => Some(TransportError::Unavailable),
        "timeout_error" => Some(TransportError::Timeout),
        "invalid_request_error" | "not_found_error" | "request_too_large" | "billing_error" => {
            Some(TransportError::Rejected)
        }
        _ => None,
    }
}

fn map_status(status: u16) -> TransportError {
    match status {
        401 | 403 => TransportError::Unauthorized,
        429 => TransportError::RateLimited,
        408 | 504 => TransportError::Timeout,
        500..=599 => TransportError::Unavailable,
        _ => TransportError::Rejected,
    }
}

/// The `error.type` of an Anthropic error envelope, if the body is one.
fn error_envelope_type(body: &[u8]) -> Option<String> {
    let envelope: Value = serde_json::from_slice(body).ok()?;
    if envelope.get("type")?.as_str()? != "error" {
        return None;
    }
    Some(envelope.get("error")?.get("type")?.as_str()?.to_string())
}

/// A decoded reply: the bare proposal bytes plus whatever the same response disclosed.
pub(super) struct DecodedReply {
    pub body: Vec<u8>,
    pub observations: Option<CallObservations>,
}

impl DecodedReply {
    fn plain(body: Vec<u8>) -> DecodedReply {
        DecodedReply {
            body,
            observations: None,
        }
    }
}

impl From<DecodedReply> for DiagnosticResponse {
    fn from(reply: DecodedReply) -> DiagnosticResponse {
        DiagnosticResponse {
            body: reply.body,
            observations: reply.observations,
        }
    }
}

/// Output a decoded-but-unusable reply as an empty body: the shared harness classifies a body
/// that is not a JSON object as `InvalidOutput`.
fn invalid_output(observations: Option<CallObservations>) -> Result<DecodedReply, TransportError> {
    Ok(DecodedReply {
        body: Vec::new(),
        observations,
    })
}

/// Decode one HTTP response into the bare proposal bytes `dispatch` expects.
pub(super) fn decode_response(
    response: &HttpResponse,
    max_response_bytes: usize,
    schema: &Value,
) -> Result<Vec<u8>, TransportError> {
    decode_reply(response, max_response_bytes, schema).map(|reply| reply.body)
}

/// Token counts are reported only when the response carries them as non-negative integers; a
/// missing or malformed count stays unknown rather than becoming zero. Anthropic does not echo
/// the temperature or token limit a request used, so no effective setting is ever reported.
fn observe(envelope: &Value) -> Option<CallObservations> {
    let count = |key: &str| {
        envelope
            .get("usage")
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
    };
    let usage = TokenUsage {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
    };
    let reported_model = envelope
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let has_usage = usage.input_tokens.is_some() || usage.output_tokens.is_some();
    if !has_usage && reported_model.is_none() {
        return None;
    }
    Some(CallObservations {
        effective_settings: Vec::new(),
        usage: has_usage.then_some(usage),
        reported_model,
    })
}

/// Decode one HTTP response, keeping the usage and model of the same response whenever it was a
/// well-formed message, even if its content turns out unusable.
pub(super) fn decode_reply(
    response: &HttpResponse,
    max_response_bytes: usize,
    schema: &Value,
) -> Result<DecodedReply, TransportError> {
    if response.body.len() > max_response_bytes {
        // A non-2xx status is classified from the status alone, without reading the body, so a
        // large gateway error page keeps its retry semantics. Any other oversized body is handed
        // back undecoded so the shared harness applies its size bound before any parsing.
        if !(200..300).contains(&response.status) {
            return Err(map_status(response.status));
        }
        return Ok(DecodedReply::plain(response.body.clone()));
    }
    if let Some(error_type) = error_envelope_type(&response.body) {
        return Err(map_error_type(&error_type).unwrap_or_else(|| map_status(response.status)));
    }
    if !(200..300).contains(&response.status) {
        return Err(map_status(response.status));
    }
    let Ok(envelope) = serde_json::from_slice::<Value>(&response.body) else {
        return invalid_output(None);
    };
    if envelope.get("type").and_then(Value::as_str) != Some("message") {
        return invalid_output(None);
    }
    let observations = observe(&envelope);
    match envelope.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => {}
        Some("refusal") => return Err(TransportError::Rejected),
        _ => return invalid_output(observations),
    }
    let Some(blocks) = envelope.get("content").and_then(Value::as_array) else {
        return invalid_output(observations);
    };
    // `tool_choice` is `auto`, so a valid reply may carry text or thinking blocks around the
    // call. Those are never parsed as the proposal; only exactly one `interpret` call counts.
    let mut interpret_input = None;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text" | "thinking" | "redacted_thinking") => {}
            Some("tool_use")
                if block.get("name").and_then(Value::as_str) == Some(INTERPRETATION_TOOL_NAME) =>
            {
                if interpret_input.replace(block.get("input")).is_some() {
                    return invalid_output(observations);
                }
            }
            _ => return invalid_output(observations),
        }
    }
    let Some(Some(input)) = interpret_input else {
        return invalid_output(observations);
    };
    if !input.is_object() || !conforms(input, schema) {
        return invalid_output(observations);
    }
    let body = serde_json::to_vec(input).map_err(|_| TransportError::Rejected)?;
    Ok(DecodedReply { body, observations })
}
