//! Anthropic Messages API request construction and response decoding. Everything
//! protocol-specific stays in this module; callers only see the extracted proposal bytes or a
//! shared [`TransportError`].

use serde_json::{json, Value};

use super::schema::conforms;
use super::{AnthropicSettings, HttpResponse, INTERPRETATION_TOOL_NAME};
use crate::interpretation::instructions::render_prompt;
use crate::providers::contracts::{
    InterpretationRequest, ProviderProfile, StructuredOutputMode, TransportError,
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

/// `tool_choice` is `auto`: current Anthropic models reject forced tool use (`type: tool` or
/// `any`) with HTTP 400, so the system prompt instructs the call and [`decode_response`]
/// accepts only a reply that actually made exactly one `interpret` call.
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
    let system = format!(
        "{instructions}\n\n{delivery}",
        instructions = prompt.system,
        delivery = delivery_framing(),
    );
    let body = json!({
        "model": profile.model(),
        "max_tokens": settings.max_output_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": prompt.user }],
        "tools": [{
            "name": INTERPRETATION_TOOL_NAME,
            "description": "Report the interpretation of the captured note.",
            "input_schema": effective_schema(mode, settings),
        }],
        "tool_choice": { "type": "auto" },
    });
    serde_json::to_vec(&body).map_err(|_| TransportError::Rejected)
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

/// Output a decoded-but-unusable reply as an empty body: the shared harness classifies a body
/// that is not a JSON object as `InvalidOutput`.
fn invalid_output() -> Result<Vec<u8>, TransportError> {
    Ok(Vec::new())
}

/// Decode one HTTP response into the bare proposal bytes `dispatch` expects.
pub(super) fn decode_response(
    response: &HttpResponse,
    max_response_bytes: usize,
    schema: &Value,
) -> Result<Vec<u8>, TransportError> {
    if response.body.len() > max_response_bytes {
        // A non-2xx status is classified from the status alone, without reading the body, so a
        // large gateway error page keeps its retry semantics. Any other oversized body is handed
        // back undecoded so the shared harness applies its size bound before any parsing.
        if !(200..300).contains(&response.status) {
            return Err(map_status(response.status));
        }
        return Ok(response.body.clone());
    }
    if let Some(error_type) = error_envelope_type(&response.body) {
        return Err(map_error_type(&error_type).unwrap_or_else(|| map_status(response.status)));
    }
    if !(200..300).contains(&response.status) {
        return Err(map_status(response.status));
    }
    let Ok(envelope) = serde_json::from_slice::<Value>(&response.body) else {
        return invalid_output();
    };
    if envelope.get("type").and_then(Value::as_str) != Some("message") {
        return invalid_output();
    }
    match envelope.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => {}
        Some("refusal") => return Err(TransportError::Rejected),
        _ => return invalid_output(),
    }
    let Some(blocks) = envelope.get("content").and_then(Value::as_array) else {
        return invalid_output();
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
                    return invalid_output();
                }
            }
            _ => return invalid_output(),
        }
    }
    let Some(Some(input)) = interpret_input else {
        return invalid_output();
    };
    if !input.is_object() || !conforms(input, schema) {
        return invalid_output();
    }
    serde_json::to_vec(input).map_err(|_| TransportError::Rejected)
}
