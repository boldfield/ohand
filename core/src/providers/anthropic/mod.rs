use super::contracts::{AdapterCall, ProviderAdapter, TransportError};
use crate::providers::contracts::StructuredOutputMode;
use serde::{Deserialize, Serialize};

pub use self::fake::{FakeAnthropicStep, FakeAnthropicTransport};

pub mod fake;

/// Request body for Anthropic Messages API.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Response envelope from Anthropic Messages API.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    stop_reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum AnthropicContent {
    Text {
        text: String,
    },
    ToolUse {
        #[serde(default)]
        input: serde_json::Value,
    },
}

/// Anthropic provider adapter. The actual HTTP transport is delegated to the platform
/// (native on iOS, or injected for testing). This adapter builds the request and
/// interprets the response.
pub struct AnthropicAdapter {
    transport: Box<dyn AnthropicTransport>,
}

impl AnthropicAdapter {
    pub fn new(transport: Box<dyn AnthropicTransport>) -> Self {
        AnthropicAdapter { transport }
    }
}

/// Platform-specific HTTP transport for Anthropic API calls.
pub trait AnthropicTransport: Send + Sync {
    fn post(
        &self,
        endpoint: &str,
        api_key_ref: &str,
        body: &[u8],
        timeout_ms: u64,
        max_response_bytes: usize,
    ) -> Result<Vec<u8>, TransportError>;
}

impl ProviderAdapter for AnthropicAdapter {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        let profile = call.profile;
        let request = call.request;

        if call.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }

        let structured_mode = profile
            .capability(crate::providers::contracts::ProviderCapability::TextInterpretation)
            .map(|m| m.structured_output)
            .unwrap_or(StructuredOutputMode::None);

        let (tools, tool_choice) = match structured_mode {
            StructuredOutputMode::JsonObject => (
                Some(vec![AnthropicTool {
                    name: "interpret".to_string(),
                    description: "Return the interpretation as a JSON object".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                }]),
                Some(serde_json::json!({
                    "type": "tool",
                    "name": "interpret"
                })),
            ),
            StructuredOutputMode::JsonSchema => (
                Some(vec![AnthropicTool {
                    name: "interpret".to_string(),
                    description: "Return the interpretation as a JSON object".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                }]),
                Some(serde_json::json!({
                    "type": "tool",
                    "name": "interpret"
                })),
            ),
            StructuredOutputMode::None => (None, None),
        };

        let anthropic_request = AnthropicRequest {
            model: profile.model().to_string(),
            max_tokens: 1024,
            system: format!(
                "You are an interpreter of captured notes. Analyze the following text and return a JSON object with the interpretation.\n\
                 Instructions version: {}\n\
                 Timezone: {}\n\
                 Locale: {}\n\
                 Reference time: {}",
                request.instruction_version(),
                request.time_context().timezone,
                request.time_context().locale,
                request.time_context().reference_time.to_rfc3339()
            ),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: request.text().to_string(),
            }],
            tools,
            tool_choice,
        };

        let body = serde_json::to_vec(&anthropic_request).map_err(|_| TransportError::Rejected)?;

        let endpoint = "https://api.anthropic.com/v1/messages";
        validate_endpoint_authorized(endpoint, profile.authorized_destinations())
            .map_err(|_| TransportError::Rejected)?;

        let remaining_ms = call.deadline_ms.saturating_sub(call.clock.now_ms());
        if remaining_ms == 0 {
            return Err(TransportError::Timeout);
        }

        let response_bytes = self.transport.post(
            endpoint,
            profile.credential_ref().as_str(),
            &body,
            remaining_ms,
            call.max_response_bytes,
        )?;

        decode_anthropic_response(&response_bytes)
    }
}

fn validate_endpoint_authorized(endpoint: &str, authorized: &[String]) -> Result<(), ()> {
    authorized
        .iter()
        .any(|dest| endpoint.starts_with(dest))
        .then_some(())
        .ok_or(())
}

/// Decode the Anthropic Messages API response and extract the structured output.
fn decode_anthropic_response(response_bytes: &[u8]) -> Result<Vec<u8>, TransportError> {
    let response: AnthropicResponse =
        serde_json::from_slice(response_bytes).map_err(|_| TransportError::Rejected)?;

    match response.stop_reason.as_str() {
        "max_tokens" | "refusal" => return Err(TransportError::Rejected),
        "tool_use" | "end_turn" => {}
        _ => return Err(TransportError::Rejected),
    }

    if response.content.is_empty() {
        return Err(TransportError::Rejected);
    }

    for content in response.content {
        match content {
            AnthropicContent::ToolUse { input } => {
                return serde_json::to_vec(&input).map_err(|_| TransportError::Rejected);
            }
            AnthropicContent::Text { text } => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    return serde_json::to_vec(&json).map_err(|_| TransportError::Rejected);
                }
            }
        }
    }

    Err(TransportError::Rejected)
}
