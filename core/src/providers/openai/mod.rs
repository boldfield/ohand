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
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<RequestMetadata>,
}

#[derive(Debug, Clone, Serialize)]
struct RequestMetadata {
    instruction_version: String,
    time_context: TimeContextData,
}

#[derive(Debug, Clone, Serialize)]
struct TimeContextData {
    timezone: String,
    locale: String,
    reference_time: String,
    utc_offset_at_capture: i32,
    calendar: String,
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
#[derive(Debug, Clone, Deserialize)]
struct OpenAiResponse {
    choices: Vec<Choice>,
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

/// OpenAI error response.
#[derive(Debug, Clone, Deserialize)]
struct OpenAiError {
    error: ErrorDetail,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct ErrorDetail {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: String,
}

/// HTTP transport interface for making requests to OpenAI API.
pub trait HttpTransport {
    #[allow(clippy::too_many_arguments)]
    fn post(
        &self,
        endpoint: &str,
        credential_ref: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        deadline_ms: u64,
        cancel: &crate::providers::contracts::CancelToken,
        clock: &dyn crate::providers::contracts::Clock,
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

const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

const SYSTEM_INSTRUCTION: &str = "You are an assistant that interprets user input and produces \
structured JSON output. Respond only with a JSON object containing the interpretation of the user's input. \
The output must be valid JSON with a 'kind' field indicating the type of interpretation. \
Examples: {\"kind\":\"note\",\"text\":\"...\",\"priority\":\"high\"}, {\"kind\":\"refusal\"}, {\"kind\":\"command\",\"action\":\"...\"}. \
Provide a single JSON object. The 'kind' field indicates the interpretation type, and additional fields depend on the kind.";

impl<T: HttpTransport> ProviderAdapter for OpenAiAdapter<T> {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        // Get the endpoint. OpenAI hosted uses the default API endpoint.
        let endpoint = call.profile.endpoint().unwrap_or(DEFAULT_ENDPOINT);
        let credential_ref = call.profile.credential_ref().as_str();

        // Extract time context from request
        let time_ctx = call.request.time_context();
        let time_context_data = TimeContextData {
            timezone: time_ctx.timezone.clone(),
            locale: time_ctx.locale.clone(),
            reference_time: time_ctx.reference_time.to_rfc3339(),
            utc_offset_at_capture: time_ctx.utc_offset_at_capture,
            calendar: time_ctx.calendar.clone(),
        };

        // Build the request according to OpenAI's API spec.
        let request = OpenAiRequest {
            model: call.profile.model().to_string(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: SYSTEM_INSTRUCTION.to_string(),
                },
                Message {
                    role: "user".to_string(),
                    content: call.request.text().to_string(),
                },
            ],
            temperature: 0.0,
            response_format: ResponseFormat::JsonObject,
            metadata: Some(RequestMetadata {
                instruction_version: call.request.instruction_version().to_string(),
                time_context: time_context_data,
            }),
        };

        let body = serde_json::to_vec(&request).map_err(|_| TransportError::Rejected)?;

        let headers = [("Content-Type", "application/json")];

        let (status, response_bytes) = self.transport.post(
            endpoint,
            credential_ref,
            &headers,
            body,
            call.deadline_ms,
            call.cancel,
            call.clock,
            call.max_response_bytes as u64,
        )?;

        // Decode the OpenAI response and extract the proposal.
        self.decode_response(status, &response_bytes)
    }
}

impl<T: HttpTransport> OpenAiAdapter<T> {
    fn decode_response(
        &self,
        status: u16,
        response_bytes: &[u8],
    ) -> Result<Vec<u8>, TransportError> {
        // Handle HTTP error statuses first
        if status >= 400 {
            // Try to parse as OpenAI error response
            if let Ok(error_response) = serde_json::from_slice::<OpenAiError>(response_bytes) {
                return Err(self.map_error_code(status, error_response.error.code.as_deref()));
            }
            // If we can't parse the error, map based on status code
            return Err(self.map_status_code(status));
        }

        // Handle successful responses (200-399)
        if let Ok(response) = serde_json::from_slice::<OpenAiResponse>(response_bytes) {
            // Extract the first choice's message content
            if let Some(choice) = response.choices.first() {
                // Check for content filter rejection or truncation - these are protocol violations
                if let Some(reason) = &choice.finish_reason {
                    if reason == "content_filter" {
                        return Err(TransportError::Rejected);
                    }
                    // Truncation (finish_reason == "length") is incomplete output
                    if reason == "length" {
                        return Err(TransportError::InvalidOutput);
                    }
                }

                // Handle successful content
                if let Some(content) = &choice.message.content {
                    return Ok(content.as_bytes().to_vec());
                }

                // Handle refusal - map to TransportError
                if let Some(_refusal) = &choice.message.refusal {
                    return Err(TransportError::Rejected);
                }
            }
            // No valid content, refusal, or choices - this is a protocol violation
            Err(TransportError::InvalidOutput)
        } else {
            // Response is not valid OpenAI JSON - this is a protocol violation
            Err(TransportError::InvalidOutput)
        }
    }

    fn map_status_code(&self, status: u16) -> TransportError {
        match status {
            401 | 403 => TransportError::Unauthorized,
            429 => TransportError::RateLimited,
            _ => TransportError::Unavailable,
        }
    }

    fn map_error_code(&self, status: u16, code: Option<&str>) -> TransportError {
        match code {
            Some("invalid_api_key") => TransportError::Unauthorized,
            Some("rate_limit_exceeded") => TransportError::RateLimited,
            Some("insufficient_quota") => TransportError::Unavailable,
            _ => self.map_status_code(status),
        }
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
    pub struct MockTransport {
        response: Arc<Mutex<Option<Result<(u16, Vec<u8>), TransportError>>>>,
        pub capture_request: Arc<Mutex<Option<CapturedRequest>>>,
    }

    #[derive(Debug, Clone)]
    pub struct CapturedRequest {
        pub endpoint: String,
        pub credential_ref: String,
        pub body: Vec<u8>,
        pub max_response_bytes: u64,
    }

    impl MockTransport {
        fn new(response: Result<(u16, Vec<u8>), TransportError>) -> Self {
            MockTransport {
                response: Arc::new(Mutex::new(Some(response))),
                capture_request: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl HttpTransport for MockTransport {
        fn post(
            &self,
            endpoint: &str,
            credential_ref: &str,
            _headers: &[(&str, &str)],
            body: Vec<u8>,
            _deadline_ms: u64,
            _cancel: &CancelToken,
            _clock: &dyn Clock,
            max_response_bytes: u64,
        ) -> Result<(u16, Vec<u8>), TransportError> {
            *self.capture_request.lock().expect("lock") = Some(CapturedRequest {
                endpoint: endpoint.to_string(),
                credential_ref: credential_ref.to_string(),
                body,
                max_response_bytes,
            });

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
            response: Result<(u16, Vec<u8>), TransportError>,
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
        // Proper OpenAI response envelope with content
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "{\"kind\":\"note\",\"text\":\"reminder\"}"
                },
                "finish_reason": "stop"
            }]
        }"#;
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
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
        let result = harness.run(
            Ok((200, b"not json {]".to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn non_object_json_fails() {
        let harness = Harness::new();
        let result = harness.run(Ok((200, b"[1,2,3]".to_vec())), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn empty_response_fails() {
        let harness = Harness::new();
        let result = harness.run(Ok((200, b"".to_vec())), DispatchLimits::default());
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn oversized_output_fails() {
        let harness = Harness::new();
        let limits = DispatchLimits {
            max_response_bytes: 64,
        };
        // The large content within the OpenAI response will be extracted and checked for size
        let large_response = br#"{"id":"chatcmpl-123","choices":[{"message":{"role":"assistant","content":"{\"kind\":\"note\",\"text\":\"this is a very long response that exceeds the size limit we set for this test\"}"},"finish_reason":"stop"}]}"#;
        let result = harness.run(Ok((200, large_response.to_vec())), limits);
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::OutputTooLarge);
    }

    #[test]
    fn cancellation_before_invoke_is_respected() {
        let harness = Harness::new();
        harness.cancel.cancel();
        let result = harness.run(Ok((200, b"{}".to_vec())), DispatchLimits::default());
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
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let output = result.expect("should succeed");
        assert_eq!(output.proposal["kind"], "note");
        assert_eq!(output.proposal["text"], "call mom");
    }

    #[test]
    fn openai_refusal_creates_refusal_failure() {
        let harness = Harness::new();
        // OpenAI response with refusal - should fail, not create a proposal
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
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail due to refusal");
        assert_eq!(failure.kind, FailureKind::Rejected);
    }

    #[test]
    fn openai_auth_error_maps_to_unauthorized() {
        let harness = Harness::new();
        // OpenAI error response for invalid API key with 401 status
        let error_response = br#"{
            "error": {
                "message": "Invalid API Key",
                "type": "invalid_request_error",
                "param": null,
                "code": "invalid_api_key"
            }
        }"#;
        let result = harness.run(
            Ok((401, error_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unauthorized);
    }

    #[test]
    fn openai_rate_limit_error_maps() {
        let harness = Harness::new();
        // OpenAI rate limit error response with 429 status
        let error_response = br#"{
            "error": {
                "message": "Rate limit exceeded",
                "type": "server_error",
                "param": null,
                "code": "rate_limit_exceeded"
            }
        }"#;
        let result = harness.run(
            Ok((429, error_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::RateLimited);
    }

    #[test]
    fn openai_server_error_maps_to_unavailable() {
        let harness = Harness::new();
        // OpenAI server error response with 500 status
        let error_response = br#"{
            "error": {
                "message": "The server had an error",
                "type": "server_error",
                "param": null,
                "code": "server_error"
            }
        }"#;
        let result = harness.run(
            Ok((500, error_response.to_vec())),
            DispatchLimits::default(),
        );
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
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
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
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn openai_content_filter_rejection_fails() {
        let harness = Harness::new();
        // OpenAI response with content_filter finish_reason
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "some content"
                },
                "finish_reason": "content_filter"
            }]
        }"#;
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Rejected);
    }

    #[test]
    fn openai_length_truncation_fails() {
        let harness = Harness::new();
        // OpenAI response with length finish_reason (truncated)
        let openai_response = br#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "incomplete..."
                },
                "finish_reason": "length"
            }]
        }"#;
        let result = harness.run(
            Ok((200, openai_response.to_vec())),
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn credential_ref_is_passed_to_transport() {
        let harness = Harness::new();
        let transport = MockTransport::new(Ok((200, br#"{"kind":"note"}"#.to_vec())));
        let capture_ref = transport.capture_request.clone();
        let adapter = OpenAiAdapter::new(transport);
        let request = request_for(&harness.profile, "test");

        let _ = dispatch(
            &adapter,
            &harness.profile,
            &request,
            harness.clock.as_ref(),
            &harness.cancel,
            &DispatchLimits::default(),
        );

        // Assert that the opaque credential reference was passed to the transport
        let captured = capture_ref.lock().expect("lock").clone();
        let captured = captured.expect("transport should capture request");
        assert_eq!(captured.credential_ref, "openai-api-key");
        // Verify that no secret bytes appear in the body
        let body_str = String::from_utf8_lossy(&captured.body);
        assert!(!body_str.contains("openai-api-key"));
        // Verify the endpoint
        assert_eq!(captured.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn max_response_bytes_is_passed_to_transport() {
        let harness = Harness::new();
        let transport = MockTransport::new(Ok((200, br#"{"kind":"note"}"#.to_vec())));
        let capture_ref = transport.capture_request.clone();
        let adapter = OpenAiAdapter::new(transport);
        let request = request_for(&harness.profile, "test");

        let limits = DispatchLimits {
            max_response_bytes: 256,
        };

        let _ = dispatch(
            &adapter,
            &harness.profile,
            &request,
            harness.clock.as_ref(),
            &harness.cancel,
            &limits,
        );

        // Assert that the max_response_bytes parameter was passed to the transport
        let captured = capture_ref.lock().expect("lock").clone();
        let captured = captured.expect("transport should capture request");
        assert_eq!(captured.max_response_bytes, 256);
        // Verify the request body contains expected fields
        let body_str = String::from_utf8_lossy(&captured.body);
        assert!(body_str.contains("\"model\""));
        assert!(body_str.contains("\"messages\""));
        assert!(body_str.contains("\"response_format\""));
        assert!(body_str.contains("\"json_object\""));
    }
}
