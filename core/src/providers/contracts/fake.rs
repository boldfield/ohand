//! Deterministic fake provider for testing.
//!
//! The fake provider exercises protocol contracts without external dependencies,
//! testing timeout, cancellation, bounded response size, and invalid output handling.
//! It has no provider history as canonical memory; each request is independent.

use super::{
    CapabilityMetadata, CapabilitySupport, ErrorType, InterpretationResponse, InterpretationResult,
    ProviderCapability, ProviderError, ProviderProfile, ProviderProtocol, ResponseStatus,
    UsageInfo,
};
use serde_json::json;
use std::time::Duration;

/// Configuration for fake provider behavior in tests.
#[derive(Debug, Clone, Copy)]
pub enum FakeBehavior {
    /// Respond successfully within the timeout.
    Success,
    /// Delay until timeout expires.
    Timeout,
    /// Return empty/unparseable output.
    InvalidOutput,
    /// Return an unavailable error.
    Unavailable,
    /// Simulate cancellation mid-request.
    Cancelled,
    /// Return output exceeding size bounds.
    OversizedOutput,
}

impl FakeBehavior {
    /// Response delay for this behavior in milliseconds.
    pub fn delay_ms(&self) -> u32 {
        match self {
            FakeBehavior::Success => 50,
            FakeBehavior::Timeout => 35000,
            FakeBehavior::InvalidOutput => 50,
            FakeBehavior::Unavailable => 100,
            FakeBehavior::Cancelled => 100,
            FakeBehavior::OversizedOutput => 50,
        }
    }
}

/// Deterministic fake provider that produces reproducible responses.
pub struct FakeProvider {
    behavior: FakeBehavior,
    max_output_size: usize,
}

impl FakeProvider {
    /// Create a new fake provider with the specified behavior.
    pub fn new(behavior: FakeBehavior) -> Self {
        FakeProvider {
            behavior,
            max_output_size: 100_000,
        }
    }

    /// Create a fake profile for testing.
    pub fn fake_profile() -> ProviderProfile {
        let mut profile = ProviderProfile::new(
            "fake-test".to_string(),
            ProviderProtocol::TestFake,
            "fake-model".to_string(),
        );
        profile.timeout_seconds = 30;
        profile.authorized_destinations = vec!["test".to_string()];

        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: Some(50_000),
                structured_output_supported: true,
            },
        );

        profile
    }

    /// Create a fake profile for testing with specific timeout.
    pub fn fake_profile_with_timeout(timeout_seconds: u32) -> ProviderProfile {
        let mut profile = Self::fake_profile();
        profile.timeout_seconds = timeout_seconds;
        profile
    }

    /// Simulate a provider response based on the configured behavior.
    /// In a real implementation, this would make an HTTP request to the provider.
    pub fn interpret(&self, request_id: &str) -> InterpretationResponse {
        let status;
        let result;
        let error;

        match self.behavior {
            FakeBehavior::Success => {
                status = ResponseStatus::Success;
                result = Some(InterpretationResult {
                    annotations: Some(json!({
                        "intent": "action",
                        "text": "call the roofer",
                        "extracted_date": "2026-10-10"
                    })),
                    raw_output: r#"{"intent":"action","text":"call the roofer"}"#.to_string(),
                });
                error = None;
            }
            FakeBehavior::Timeout => {
                status = ResponseStatus::Timeout;
                result = None;
                error = Some(ProviderError {
                    error_type: ErrorType::Timeout,
                    message: "Request timeout after 30 seconds".to_string(),
                    retriable: true,
                    code: Some("TIMEOUT".to_string()),
                });
            }
            FakeBehavior::InvalidOutput => {
                status = ResponseStatus::InvalidOutput;
                result = Some(InterpretationResult {
                    annotations: None,
                    raw_output: "not valid json {]".to_string(),
                });
                error = Some(ProviderError {
                    error_type: ErrorType::InvalidOutput,
                    message: "Response is not valid JSON".to_string(),
                    retriable: false,
                    code: Some("INVALID_JSON".to_string()),
                });
            }
            FakeBehavior::Unavailable => {
                status = ResponseStatus::Error;
                result = None;
                error = Some(ProviderError {
                    error_type: ErrorType::Unavailable,
                    message: "Service temporarily unavailable".to_string(),
                    retriable: true,
                    code: Some("SERVICE_UNAVAILABLE".to_string()),
                });
            }
            FakeBehavior::Cancelled => {
                status = ResponseStatus::Cancelled;
                result = None;
                error = Some(ProviderError {
                    error_type: ErrorType::Unknown,
                    message: "Request was cancelled".to_string(),
                    retriable: true,
                    code: Some("CANCELLED".to_string()),
                });
            }
            FakeBehavior::OversizedOutput => {
                status = ResponseStatus::InvalidOutput;
                let oversized = "x".repeat(self.max_output_size + 1000);
                result = Some(InterpretationResult {
                    annotations: None,
                    raw_output: oversized,
                });
                error = Some(ProviderError {
                    error_type: ErrorType::InvalidOutput,
                    message: "Response exceeds maximum size".to_string(),
                    retriable: false,
                    code: Some("OVERSIZED".to_string()),
                });
            }
        }

        InterpretationResponse {
            request_id: request_id.to_string(),
            status,
            result,
            error,
            usage: if matches!(self.behavior, FakeBehavior::Success) {
                Some(UsageInfo {
                    input_tokens: Some(50),
                    output_tokens: Some(25),
                })
            } else {
                None
            },
        }
    }

    /// Get the expected delay for this behavior.
    pub fn expected_delay(&self) -> Duration {
        Duration::from_millis(self.behavior.delay_ms() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_fake_provider_success() {
        let provider = FakeProvider::new(FakeBehavior::Success);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.request_id, request_id);
        assert_eq!(response.status, ResponseStatus::Success);
        assert!(response.result.is_some());
        assert!(response.error.is_none());
        assert!(response.usage.is_some());

        let result = response.result.unwrap();
        assert!(result.annotations.is_some());
        assert!(!result.raw_output.is_empty());
    }

    #[test]
    fn test_fake_provider_timeout() {
        let provider = FakeProvider::new(FakeBehavior::Timeout);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::Timeout);
        assert!(response.result.is_none());
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.error_type, ErrorType::Timeout);
        assert!(error.retriable);
    }

    #[test]
    fn test_fake_provider_invalid_output() {
        let provider = FakeProvider::new(FakeBehavior::InvalidOutput);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::InvalidOutput);
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.error_type, ErrorType::InvalidOutput);
        assert!(!error.retriable);
    }

    #[test]
    fn test_fake_provider_unavailable() {
        let provider = FakeProvider::new(FakeBehavior::Unavailable);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::Error);
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.error_type, ErrorType::Unavailable);
        assert!(error.retriable);
    }

    #[test]
    fn test_fake_provider_cancelled() {
        let provider = FakeProvider::new(FakeBehavior::Cancelled);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::Cancelled);
        assert!(response.error.is_some());
    }

    #[test]
    fn test_fake_provider_oversized_output() {
        let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::InvalidOutput);
        assert!(response.error.is_some());

        let result = response.result.unwrap();
        assert!(result.raw_output.len() > provider.max_output_size);
    }

    #[test]
    fn test_fake_profile() {
        let profile = FakeProvider::fake_profile();

        assert_eq!(profile.protocol, ProviderProtocol::TestFake);
        assert_eq!(profile.model, "fake-model");
        assert!(!profile.authorized_destinations.is_empty());
        assert!(profile
            .capabilities
            .contains_key(&ProviderCapability::TextInterpretation));
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn test_fake_profile_with_timeout() {
        let profile = FakeProvider::fake_profile_with_timeout(60);

        assert_eq!(profile.timeout_seconds, 60);
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn test_behavior_delays() {
        let success = FakeBehavior::Success;
        let timeout = FakeBehavior::Timeout;

        assert!(success.delay_ms() < timeout.delay_ms());
        assert!(timeout.delay_ms() > 30000);
    }

    #[test]
    fn test_multiple_requests_independent() {
        let provider = FakeProvider::new(FakeBehavior::Success);

        let req1 = Uuid::new_v4().to_string();
        let req2 = Uuid::new_v4().to_string();

        let resp1 = provider.interpret(&req1);
        let resp2 = provider.interpret(&req2);

        assert_eq!(resp1.request_id, req1);
        assert_eq!(resp2.request_id, req2);
        assert_ne!(resp1.request_id, resp2.request_id);
    }

    #[test]
    fn test_response_serialization() {
        let provider = FakeProvider::new(FakeBehavior::Success);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);
        let json = serde_json::to_string(&response).expect("should serialize");
        let deserialized: InterpretationResponse =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(response.request_id, deserialized.request_id);
        assert_eq!(response.status, deserialized.status);
    }
}
