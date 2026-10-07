use super::contracts::{AdapterCall, ProviderAdapter, TransportError};
use serde::Serialize;

#[cfg(test)]
mod tests;

/// Request body for Anthropic Messages API.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
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

        let anthropic_request = AnthropicRequest {
            model: profile.model().to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: format!(
                    "You are an interpreter of captured notes. Analyze the following text and return a JSON object with the interpretation.\n\
                     Text: {}",
                    request.text()
                ),
            }],
            temperature: Some(0.7),
        };

        let body = serde_json::to_vec(&anthropic_request).map_err(|_| TransportError::Rejected)?;

        let endpoint = profile
            .endpoint()
            .unwrap_or("https://api.anthropic.com/v1/messages");
        let remaining_ms = call.deadline_ms.saturating_sub(call.clock.now_ms());

        if call.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }

        if remaining_ms == 0 {
            return Err(TransportError::Timeout);
        }

        self.transport.post(
            endpoint,
            profile.credential_ref().as_str(),
            &body,
            remaining_ms,
            call.max_response_bytes,
        )
    }
}
