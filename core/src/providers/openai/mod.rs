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
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ChoiceMessage {
    content: String,
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

pub struct OpenAiAdapter;

impl ProviderAdapter for OpenAiAdapter {
    fn invoke(&self, call: &AdapterCall<'_>) -> Result<Vec<u8>, TransportError> {
        // Get the endpoint from the profile. OpenAI hosted uses the default API endpoint.
        let endpoint = call
            .profile
            .endpoint()
            .unwrap_or("https://api.openai.com/v1/chat/completions");

        // Get credentials from the profile. In production, these would come from secure storage.
        let credential_ref = call.profile.credential_ref().as_str();

        // Build the request according to OpenAI's API.
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
            temperature: 0.7,
            response_format: ResponseFormat::JsonObject,
        };

        // Convert to JSON.
        let body = match serde_json::to_vec(&request) {
            Ok(b) => b,
            Err(_) => return Err(TransportError::Rejected),
        };

        // In a real implementation, this would use an HTTP client like reqwest.
        // For now, we'll return a properly formatted error to show the pattern.
        // The actual HTTP transport layer would need to:
        // 1. Check deadline against the provided deadline_ms
        // 2. Monitor the cancel token during the request
        // 3. Return the raw response bytes or a TransportError
        // 4. Respect max_response_bytes when reading the response

        // Simulate the HTTP call (in reality this would be a real HTTP request)
        self.make_http_request(endpoint, credential_ref, body, call)
    }
}

impl OpenAiAdapter {
    fn make_http_request(
        &self,
        _endpoint: &str,
        _credential_ref: &str,
        _body: Vec<u8>,
        _call: &AdapterCall<'_>,
    ) -> Result<Vec<u8>, TransportError> {
        // This is where the actual HTTP transport would happen.
        // For testing, we rely on the FakeProvider in tests.
        // In production, this would use something like:
        //
        // let client = reqwest::Client::new();
        // let timeout = Duration::from_millis(call.deadline_ms - call.clock.now_ms());
        // let request = client
        //     .post(endpoint)
        //     .header("Authorization", format!("Bearer {}", api_key))
        //     .timeout(timeout)
        //     .body(body);
        // let response = request.send().await
        //     .map_err(|e| self.map_http_error(e))?;
        // let bytes = response.bytes().await
        //     .map_err(|_| TransportError::Unavailable)?;
        // Ok(bytes.to_vec())

        // For now, return a placeholder to be implemented with real HTTP layer
        Err(TransportError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::contracts::fake::{FakeProvider, FakeStep};
    use crate::providers::contracts::{
        dispatch, CancelToken, CapabilityMetadata, DispatchLimits, FailureKind,
        InterpretationRequest, ManualClock, ProviderCapability, ProviderProfile,
        ProviderProfileBuilder, ProviderProtocol, StructuredOutputMode, TextBasis,
    };
    use crate::time::TimeContext;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use uuid::Uuid;

    #[allow(dead_code)]
    const ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
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
            steps: Vec<FakeStep>,
            limits: DispatchLimits,
        ) -> Result<
            crate::providers::contracts::InterpretationOutput,
            crate::providers::contracts::ProviderFailure,
        > {
            let fake = FakeProvider::new(self.clock.clone(), steps);
            let request = request_for(&self.profile, "call mom tomorrow");
            dispatch(
                &fake,
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
            vec![FakeStep::respond(r#"{"kind":"note","text":"reminder"}"#).after_ms(1200)],
            DispatchLimits::default(),
        );
        let output = result.expect("should succeed");
        assert_eq!(output.proposal["kind"], "note");
        assert_eq!(output.proposal["text"], "reminder");
        assert_eq!(output.elapsed_ms, 1200);
    }

    #[test]
    fn timeout_is_enforced() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::respond("{}").after_ms(30_001)],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should timeout");
        assert_eq!(failure.kind, FailureKind::Timeout);
    }

    #[test]
    fn invalid_json_output_fails() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::respond("not json")],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn non_object_output_fails() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::respond("[1,2,3]")],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn transport_error_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::fail(TransportError::Unauthorized)],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::Unauthorized);
    }

    #[test]
    fn rate_limit_error_is_mapped() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::fail(TransportError::RateLimited)],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::RateLimited);
    }

    #[test]
    fn cancellation_is_respected() {
        let harness = Harness::new();
        harness.cancel.cancel();
        let result = harness.run(vec![FakeStep::respond("{}")], DispatchLimits::default());
        let failure = result.expect_err("should be cancelled");
        assert_eq!(failure.kind, FailureKind::Cancelled);
    }

    #[test]
    fn malformed_output_fails() {
        let harness = Harness::new();
        let result = harness.run(
            vec![FakeStep::respond(b"not json {]")],
            DispatchLimits::default(),
        );
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::InvalidOutput);
    }

    #[test]
    fn oversized_output_fails() {
        let harness = Harness::new();
        let limits = DispatchLimits {
            max_response_bytes: 64,
        };
        let large_response = r#"{"kind":"note","text":"this is a very long response that exceeds the size limit we set for this test"}"#;
        let result = harness.run(vec![FakeStep::respond(large_response)], limits);
        let failure = result.expect_err("should fail");
        assert_eq!(failure.kind, FailureKind::OutputTooLarge);
    }
}
