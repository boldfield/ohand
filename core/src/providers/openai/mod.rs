//! OpenAI API adapter for interpretation requests.

use serde::{Deserialize, Serialize};

use crate::providers::contracts::{AdapterCall, ProviderAdapter, TransportError};

/// Request body to send to OpenAI's chat completions API.
#[derive(Debug, Clone, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    response_format: ResponseFormat,
}

#[derive(Debug, Clone, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ResponseFormat {
    #[serde(rename = "json_object")]
    JsonObject,
}

/// OpenAI chat completions response.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct OpenAiResponse {
    choices: Vec<Choice>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
}

/// OpenAI error response.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct OpenAiError {
    error: ErrorDetail,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: String,
}

/// HTTP transport interface for making requests to OpenAI API.
pub trait HttpTransport {
    fn post(
        &self,
        endpoint: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &crate::providers::contracts::CancelToken,
        clock: &dyn crate::providers::contracts::Clock,
    ) -> Result<Vec<u8>, TransportError>;
}

pub struct OpenAiAdapter<T: HttpTransport> {
    transport: T,
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    pub fn new(transport: T) -> Self {
        OpenAiAdapter { transport }
    }
}

impl<T: HttpTransport> ProviderAdapter for OpenAiAdapter<T> {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        // Get the endpoint. OpenAI hosted uses the default API endpoint.
        let endpoint = call
            .profile
            .endpoint()
            .unwrap_or("https://api.openai.com/v1/chat/completions");

        // Get credential reference. The actual credential is resolved by the transport.
        let credential_ref = call.profile.credential_ref().as_str();

        // Build the request according to OpenAI's API spec.
        let request = OpenAiRequest {
            model: call.profile.model().to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: call.request.instruction_version().to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: call.request.text().to_string(),
                },
            ],
            temperature: 0.0,
            response_format: ResponseFormat::JsonObject,
        };

        let body = serde_json::to_vec(&request).map_err(|_| TransportError::Rejected)?;

        let auth_header = format!("Bearer {{{}}}", credential_ref);
        let headers = [
            ("Authorization", auth_header.as_str()),
            ("Content-Type", "application/json"),
        ];

        let response_bytes = self.transport.post(
            endpoint,
            &headers,
            body,
            call.deadline_ms,
            call.cancel,
            call.clock,
        )?;

        // Decode the OpenAI response and extract the proposal.
        self.decode_response(&response_bytes)
    }
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    fn decode_response(&self, response_bytes: &[u8]) -> Result<Vec<u8>, TransportError> {
        // Try to parse as OpenAI response first
        if let Ok(response) = serde_json::from_slice::<OpenAiResponse>(response_bytes) {
            // Extract the first choice's message content
            if let Some(choice) = response.choices.first() {
                if let Some(content) = &choice.message.content {
                    // Return the content as raw bytes for dispatch to validate
                    return Ok(content.as_bytes().to_vec());
                }
                // Handle refusal
                if let Some(refusal) = &choice.message.refusal {
                    let proposal = serde_json::json!({
                        "kind": "refusal",
                        "reason": refusal
                    });
                    return serde_json::to_vec(&proposal).map_err(|_| TransportError::Rejected);
                }
            }
            // No valid content or refusal
            return Err(TransportError::Rejected);
        }

        // Try to parse as OpenAI error response
        if let Ok(error_response) = serde_json::from_slice::<OpenAiError>(response_bytes) {
            if let Some(code) = error_response.error.code.as_ref() {
                match code.as_str() {
                    "invalid_api_key" | "invalid_request_error" => {
                        return Err(TransportError::Unauthorized)
                    }
                    "rate_limit_exceeded" => return Err(TransportError::RateLimited),
                    "server_error" | "internal_error" => return Err(TransportError::Unavailable),
                    _ => return Err(TransportError::Unavailable),
                }
            }
            return Err(TransportError::Unavailable);
        }

        // If we can't parse it, return the raw bytes and let dispatch handle it
        Ok(response_bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::contracts::{
        dispatch, CancelToken, CapabilityMetadata, Clock, DispatchLimits, FailureKind,
        InterpretationRequest, ManualClock, ProviderCapability, ProviderProfile,
        ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode, TextBasis,
    };
    use crate::time::TimeContext;
    use chrono::{TimeZone, Utc};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    const ORIGIN: &str = "https://api.openai.com";

    fn text_capability() -> CapabilityMetadata {
        CapabilityMetadata::supported(
            ProviderCapability::TextInterpretation,
            "adapter-test:openai",
        )
        .with_input_size_limit(200)
        .with_structured_output(StructuredOutputMode::JsonObject)
    }

    fn valid_builder() -> ProviderProfileBuilder {
        ProviderProfileBuilder::new("openai-gpt", ProviderProtocol::OpenAi, "gpt-4o")
            .credential_ref("openai-api-key")
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

    /// Mock HTTP transport for testing.
    #[allow(clippy::type_complexity)]
    struct MockTransport {
        response: Arc<Mutex<Option<Result<Vec<u8>, TransportError>>>>,
    }

    impl MockTransport {
        fn new(response: Result<Vec<u8>, TransportError>) -> Self {
            MockTransport {
                response: Arc::new(Mutex::new(Some(response))),
            }
        }
    }

    impl HttpTransport for MockTransport {
        fn post(
            &self,
            _endpoint: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _deadline_ms: u64,
            _cancel: &CancelToken,
            _clock: &dyn Clock,
        ) -> Result<Vec<u8>, TransportError> {
            self.response
                .lock()
                .expect("response lock")
                .take()
                .expect("no response available")
        }
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
            response: Result<Vec<u8>, TransportError>,
            limits: DispatchLimits,
        ) -> Result<
            crate::providers::contracts::InterpretationOutput,
            crate::providers::contracts::ProviderFailure,
        > {
            let transport = MockTransport::new(response);
            let adapter = OpenAiAdapter::new(transport);
            let request = request_for(&self.profile, "call mom tomorrow");
            dispatch(
                &adapter,
                &self.profile,
                &request,
                self.clock.as_ref(),
                &self.cancel,
                &limits,
            )
        }
    }

    #[test]
    fn openai_profile_is_valid() {
        let profile = profile();
        assert_eq!(profile.protocol(), ProviderProtocol::OpenAi);
        assert_eq!(profile.model(), "gpt-4o");
        assert_eq!(profile.timeout_seconds(), 30);
        assert_eq!(profile.credential_ref().as_str(), "openai-api-key");
        assert_eq!(profile.authorized_destinations(), [ORIGIN]);
    }

    #[test]
    fn success_returns_parsed_proposal() {
        let harness = Harness::new();
        let result = harness.run(
            Ok(br#"{"kind":"note","text":"reminder"}"#.to_vec()),
            DispatchLimits::default(),
        );
        let output = result.expect("should succeed");
        assert_eq!(output.proposal["kind"], "note");
        assert_eq!(output.proposal["text"], "reminder");
    }

    #[test]
    fn transport_error_unauthorized_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::Unauthorized), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unauthorized);
    }

    #[test]
    fn transport_error_rate_limited_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::RateLimited), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::RateLimited);
    }

    #[test]
    fn transport_error_timeout_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::Timeout), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Timeout);
    }

    #[test]
    fn transport_error_cancelled_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::Cancelled), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Cancelled);
    }

    #[test]
    fn transport_error_unavailable_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::Unavailable), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unavailable);
    }

    #[test]
    fn transport_error_rejected_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(Err(TransportError::Rejected), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Rejected);
    }

    #[test]
    fn invalid_json_output_fails() {
        let harness = Harness::new();
        let result = harness.run(Ok(b"not json {]".to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn non_object_json_fails() {
        let harness = Harness::new();
        let result = harness.run(Ok(b"[1,2,3]".to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn empty_response_fails() {
        let harness = Harness::new();
        let result = harness.run(Ok(b"".to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn oversized_output_fails() {
        let harness = Harness::new();
        let limits = DispatchLimits {
            max_response_bytes: 64,
        };
        let large_response = br#"{"kind":"note","text":"this is a very long response that exceeds the size limit we set for this test"}"#;
        let result = harness.run(Ok(large_response.to_vec()), limits);
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::OutputTooLarge);
    }

    #[test]
    fn cancellation_before_invoke_is_respected() {
        let harness = Harness::new();
        harness.cancel.cancel();
        let result = harness.run(Ok(b"{}".to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should be cancelled");
        assert_eq!(failure.kind, FailureKind::Cancelled);
    }

    #[test]
    fn openai_response_format_extracts_content() {
        let harness = Harness::new();
        // Simulate an OpenAI API response with the nested format
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "{\"kind\":\"note\",\"text\":\"call mom\"}"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 20}
        }"#;
        let result = harness.run(Ok(openai_response.to_vec()), DispatchLimits::default());
        let output = result.expect("should succeed");
        assert_eq!(output.proposal["kind"], "note");
        assert_eq!(output.proposal["text"], "call mom");
    }

    #[test]
    fn openai_refusal_creates_refusal_proposal() {
        let harness = Harness::new();
        // OpenAI response with refusal
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "I cannot help with that"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let result = harness.run(Ok(openai_response.to_vec()), DispatchLimits::default());
        let output = result.expect("should succeed");
        assert_eq!(output.proposal["kind"], "refusal");
        assert_eq!(output.proposal["reason"], "I cannot help with that");
    }

    #[test]
    fn openai_auth_error_maps_to_unauthorized() {
        let harness = Harness::new();
        // OpenAI error response for invalid API key
        let error_response = br#"{
            "error": {
                "message": "Invalid API Key",
                "type": "invalid_request_error",
                "param": null,
                "code": "invalid_api_key"
            }
        }"#;
        let result = harness.run(Ok(error_response.to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unauthorized);
    }

    #[test]
    fn openai_rate_limit_error_maps() {
        let harness = Harness::new();
        // OpenAI rate limit error response
        let error_response = br#"{
            "error": {
                "message": "Rate limit exceeded",
                "type": "server_error",
                "param": null,
                "code": "rate_limit_exceeded"
            }
        }"#;
        let result = harness.run(Ok(error_response.to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::RateLimited);
    }

    #[test]
    fn openai_server_error_maps_to_unavailable() {
        let harness = Harness::new();
        // OpenAI server error response
        let error_response = br#"{
            "error": {
                "message": "The server had an error",
                "type": "server_error",
                "param": null,
                "code": "server_error"
            }
        }"#;
        let result = harness.run(Ok(error_response.to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unavailable);
    }

    #[test]
    fn empty_choices_in_openai_response_fails() {
        let harness = Harness::new();
        // OpenAI response with empty choices array
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 0}
        }"#;
        let result = harness.run(Ok(openai_response.to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Rejected);
    }

    #[test]
    fn openai_response_with_null_content_and_no_refusal_fails() {
        let harness = Harness::new();
        // Invalid OpenAI response: both content and refusal are null
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": null
                }
            }]
        }"#;
        let result = harness.run(Ok(openai_response.to_vec()), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Rejected);
    }
}
