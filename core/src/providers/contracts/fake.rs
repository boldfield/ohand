//! Deterministic fake provider for testing contract harness.
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

/// Provider adapter trait for contract normalization.
/// Real adapters (Anthropic, OpenAI, self-hosted) implement this.
pub trait ProviderAdapter: Send + Sync {
    /// Interpret with contract enforcement: timeout, cancellation, bounded output.
    fn interpret(&self, request_id: &str, profile: &ProviderProfile) -> InterpretationResponse;

    /// Get expected delay for this behavior (testing only).
    fn expected_delay(&self) -> Duration;
}

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

/// Deterministic fake provider that produces reproducible responses and enforces bounds.
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

    /// Create a fake profile for testing (TestFake protocol excluded from validation).
    /// Test profiles use TestFake protocol which bypasses validation in real code.
    pub fn fake_test_profile() -> ProviderProfile {
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
                input_size_limit: Some(100_000),
                structured_output_supported: true,
            },
        );

        profile
    }

    /// Create a valid Anthropic profile for integration tests.
    pub fn anthropic_profile() -> ProviderProfile {
        let mut profile = ProviderProfile::new(
            "anthropic-test".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(super::CredentialRef {
            ref_id: "test-cred".to_string(),
        });
        profile.timeout_seconds = 30;
        profile.authorized_destinations = vec!["interpreter".to_string()];

        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: Some(100_000),
                structured_output_supported: true,
            },
        );

        profile
    }

    /// Simulate a provider response based on the configured behavior.
    /// Enforces bounds: OversizedOutput returns truncated output, not full.
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
                let oversized_raw = "x".repeat(self.max_output_size + 1000);
                let bounded = if oversized_raw.len() > self.max_output_size {
                    oversized_raw[..self.max_output_size].to_string()
                } else {
                    oversized_raw
                };
                result = Some(InterpretationResult {
                    annotations: None,
                    raw_output: bounded,
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

    /// Get the maximum output size for bounds testing.
    pub fn max_output_size(&self) -> usize {
        self.max_output_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_fake_provider_success_response_structure() {
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
    fn test_fake_provider_timeout_response_structure() {
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
    fn test_fake_provider_invalid_output_response_structure() {
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
    fn test_fake_provider_unavailable_response_structure() {
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
    fn test_fake_provider_cancelled_response_structure() {
        let provider = FakeProvider::new(FakeBehavior::Cancelled);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::Cancelled);
        assert!(response.error.is_some());
    }

    #[test]
    fn test_fake_provider_bounds_enforcement_at_limit() {
        let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.status, ResponseStatus::InvalidOutput);
        assert!(response.error.is_some());

        let result = response.result.unwrap();
        assert!(result.raw_output.len() <= provider.max_output_size);
    }

    #[test]
    fn test_fake_provider_bounds_enforcement_exceeds_limit() {
        let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        let result = response.result.unwrap();
        assert_eq!(result.raw_output.len(), provider.max_output_size);
    }

    #[test]
    fn test_fake_test_profile_is_valid_for_testing() {
        let profile = FakeProvider::fake_test_profile();

        assert_eq!(profile.protocol, ProviderProtocol::TestFake);
        assert_eq!(profile.model, "fake-model");
        assert!(!profile.authorized_destinations.is_empty());
        assert!(profile
            .capabilities
            .contains_key(&ProviderCapability::TextInterpretation));
    }

    #[test]
    fn test_anthropic_profile_validates() {
        let profile = FakeProvider::anthropic_profile();

        assert_eq!(profile.protocol, ProviderProtocol::Anthropic);
        assert_eq!(profile.model, "claude-3-sonnet");
        assert!(profile.credential_ref.is_some());
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn test_behavior_delay_ordering() {
        let success = FakeBehavior::Success;
        let timeout = FakeBehavior::Timeout;

        assert!(success.delay_ms() < timeout.delay_ms());
        assert!(timeout.delay_ms() > 30000);
    }

    #[test]
    fn test_fake_provider_no_state_between_requests() {
        let provider = FakeProvider::new(FakeBehavior::Success);

        let req1 = Uuid::new_v4().to_string();
        let req2 = Uuid::new_v4().to_string();

        let resp1 = provider.interpret(&req1);
        let resp2 = provider.interpret(&req2);

        assert_eq!(resp1.request_id, req1);
        assert_eq!(resp2.request_id, req2);
        assert_ne!(resp1.request_id, resp2.request_id);
        assert_eq!(resp1.status, resp2.status);
    }

    #[test]
    fn test_response_serialization_roundtrip() {
        let provider = FakeProvider::new(FakeBehavior::Success);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);
        let json = serde_json::to_string(&response).expect("should serialize");
        let deserialized: InterpretationResponse =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(response.request_id, deserialized.request_id);
        assert_eq!(response.status, deserialized.status);
    }

    #[test]
    fn test_all_behaviors_produce_responses() {
        let behaviors = [
            FakeBehavior::Success,
            FakeBehavior::Timeout,
            FakeBehavior::InvalidOutput,
            FakeBehavior::Unavailable,
            FakeBehavior::Cancelled,
            FakeBehavior::OversizedOutput,
        ];

        for behavior in &behaviors {
            let provider = FakeProvider::new(*behavior);
            let request_id = Uuid::new_v4().to_string();
            let response = provider.interpret(&request_id);

            assert_eq!(response.request_id, request_id);
            assert!(!response.request_id.is_empty());
        }
    }
}
