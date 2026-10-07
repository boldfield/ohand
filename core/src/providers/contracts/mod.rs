//! Provider protocol contracts and normalization
//!
//! This module defines the versioned, non-secret provider profile schema and normalized
//! request/response/error contracts for interpretation adapters. Provider configuration
//! is immutable and versioned; jobs pin to specific profile versions so configuration
//! changes cannot silently reroute in-flight work.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

pub mod fake;

/// Protocol type supported by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderProtocol {
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "self_hosted")]
    SelfHosted,
    #[serde(rename = "test_fake")]
    TestFake,
}

impl ProviderProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderProtocol::OpenAi => "openai",
            ProviderProtocol::Anthropic => "anthropic",
            ProviderProtocol::SelfHosted => "self_hosted",
            ProviderProtocol::TestFake => "test_fake",
        }
    }
}

/// Provider capability (feature/endpoint type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum ProviderCapability {
    #[serde(rename = "text_interpretation")]
    TextInterpretation,
    #[serde(rename = "transcription")]
    Transcription,
    #[serde(rename = "embedding")]
    Embedding,
}

impl ProviderCapability {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderCapability::TextInterpretation => "text_interpretation",
            ProviderCapability::Transcription => "transcription",
            ProviderCapability::Embedding => "embedding",
        }
    }
}

/// Capability support state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilitySupport {
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "unsupported")]
    Unsupported,
    #[serde(rename = "unavailable")]
    Unavailable,
}

impl CapabilitySupport {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapabilitySupport::Supported => "supported",
            CapabilitySupport::Unsupported => "unsupported",
            CapabilitySupport::Unavailable => "unavailable",
        }
    }
}

/// Credential reference (opaque to avoid storing secrets). Resolved at dispatch time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef {
    pub ref_id: String,
}

/// Capability metadata: support state and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityMetadata {
    pub capability: ProviderCapability,
    pub support_state: CapabilitySupport,
    pub input_size_limit: Option<usize>,
    pub structured_output_supported: bool,
}

/// Immutable versioned provider profile.
/// Multiple versions can coexist per profile_id; jobs pin to a specific version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub profile_version: String,
    pub profile_id: String,
    pub protocol: ProviderProtocol,
    pub endpoint: Option<String>,
    pub model: String,
    pub credential_ref: Option<CredentialRef>,
    pub timeout_seconds: u32,
    pub retry_policy: RetryPolicy,
    pub authorized_destinations: Vec<String>,
    pub capabilities: BTreeMap<ProviderCapability, CapabilityMetadata>,
    pub created_at: DateTime<Utc>,
}

impl ProviderProfile {
    pub fn new(profile_id: String, protocol: ProviderProtocol, model: String) -> ProviderProfile {
        ProviderProfile {
            profile_version: format!("{}-{}", profile_id, Uuid::new_v4()),
            profile_id,
            protocol,
            endpoint: None,
            model,
            credential_ref: None,
            timeout_seconds: 30,
            retry_policy: RetryPolicy::default(),
            authorized_destinations: vec![],
            capabilities: BTreeMap::new(),
            created_at: Utc::now(),
        }
    }

    /// Validate the profile for completeness and internal consistency.
    pub fn validate(&self) -> Result<(), ProfileValidationError> {
        if self.model.is_empty() {
            return Err(ProfileValidationError::MissingModel);
        }

        if self.timeout_seconds == 0 {
            return Err(ProfileValidationError::InvalidTimeout);
        }

        match self.protocol {
            ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => {
                if self.endpoint.is_some() {
                    return Err(ProfileValidationError::UnexpectedEndpoint);
                }
            }
            ProviderProtocol::SelfHosted => {
                if self.endpoint.is_none() {
                    return Err(ProfileValidationError::MissingEndpoint);
                }
            }
            ProviderProtocol::TestFake => {}
        }

        if self.credential_ref.is_none() && self.protocol != ProviderProtocol::TestFake {
            return Err(ProfileValidationError::MissingCredential);
        }

        if self.authorized_destinations.is_empty() {
            return Err(ProfileValidationError::MissingAuthorizedDestinations);
        }

        if self.capabilities.is_empty() {
            return Err(ProfileValidationError::NoCapabilities);
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProfileValidationError {
    #[error("Profile is missing model")]
    MissingModel,
    #[error("Profile has invalid timeout (must be > 0)")]
    InvalidTimeout,
    #[error("Profile has unexpected endpoint for protocol")]
    UnexpectedEndpoint,
    #[error("Profile is missing endpoint for self-hosted protocol")]
    MissingEndpoint,
    #[error("Profile is missing credential reference")]
    MissingCredential,
    #[error("Profile has no authorized destinations")]
    MissingAuthorizedDestinations,
    #[error("Profile declares no capabilities")]
    NoCapabilities,
}

/// Retry policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_ms: u32,
    pub max_backoff_ms: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 10000,
        }
    }
}

/// Normalized interpretation request contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationRequest {
    pub request_id: String,
    pub capability: ProviderCapability,
    pub source_text: String,
    pub instructions: String,
    pub context: RequestContext,
}

/// Request context: capture time, timezone, locale, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub capture_instant: DateTime<Utc>,
    pub timezone_id: String,
    pub locale: String,
    pub source_revision: u32,
}

/// Normalized interpretation response contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationResponse {
    pub request_id: String,
    pub status: ResponseStatus,
    pub result: Option<InterpretationResult>,
    pub error: Option<ProviderError>,
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseStatus {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "invalid_output")]
    InvalidOutput,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationResult {
    pub annotations: Option<serde_json::Value>,
    pub raw_output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// Normalized error contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderError {
    pub error_type: ErrorType,
    pub message: String,
    pub retriable: bool,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorType {
    #[serde(rename = "authentication")]
    Authentication,
    #[serde(rename = "authorization")]
    Authorization,
    #[serde(rename = "rate_limit")]
    RateLimit,
    #[serde(rename = "invalid_input")]
    InvalidInput,
    #[serde(rename = "invalid_output")]
    InvalidOutput,
    #[serde(rename = "timeout")]
    Timeout,
    #[serde(rename = "unavailable")]
    Unavailable,
    #[serde(rename = "unknown")]
    Unknown,
}

impl ErrorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorType::Authentication => "authentication",
            ErrorType::Authorization => "authorization",
            ErrorType::RateLimit => "rate_limit",
            ErrorType::InvalidInput => "invalid_input",
            ErrorType::InvalidOutput => "invalid_output",
            ErrorType::Timeout => "timeout",
            ErrorType::Unavailable => "unavailable",
            ErrorType::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_new() {
        let profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );

        assert_eq!(profile.profile_id, "test-profile");
        assert_eq!(profile.protocol, ProviderProtocol::Anthropic);
        assert_eq!(profile.model, "claude-3-sonnet");
        assert_eq!(profile.timeout_seconds, 30);
    }

    #[test]
    fn test_profile_validation_missing_model() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "".to_string(),
        );
        profile.authorized_destinations = vec!["local".to_string()];

        let result = profile.validate();
        assert!(matches!(result, Err(ProfileValidationError::MissingModel)));
    }

    #[test]
    fn test_profile_validation_invalid_timeout() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.timeout_seconds = 0;
        profile.authorized_destinations = vec!["local".to_string()];

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::InvalidTimeout)
        ));
    }

    #[test]
    fn test_profile_validation_missing_destinations() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: None,
                structured_output_supported: true,
            },
        );

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::MissingAuthorizedDestinations)
        ));
    }

    #[test]
    fn test_profile_validation_missing_capabilities() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.authorized_destinations = vec!["local".to_string()];
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::NoCapabilities)
        ));
    }

    #[test]
    fn test_profile_validation_self_hosted_missing_endpoint() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::SelfHosted,
            "llama-3".to_string(),
        );
        profile.authorized_destinations = vec!["local".to_string()];
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: None,
                structured_output_supported: true,
            },
        );

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::MissingEndpoint)
        ));
    }

    #[test]
    fn test_profile_validation_self_hosted_with_endpoint() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::SelfHosted,
            "llama-3".to_string(),
        );
        profile.endpoint = Some("http://localhost:8000".to_string());
        profile.authorized_destinations = vec!["local".to_string()];
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: None,
                structured_output_supported: true,
            },
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "local-cred".to_string(),
        });

        assert!(profile.validate().is_ok());
    }

    #[test]
    fn test_provider_capability_ordering() {
        let mut caps = [
            ProviderCapability::Embedding,
            ProviderCapability::TextInterpretation,
            ProviderCapability::Transcription,
        ];
        caps.sort();

        assert_eq!(caps[0], ProviderCapability::TextInterpretation);
        assert_eq!(caps[1], ProviderCapability::Transcription);
        assert_eq!(caps[2], ProviderCapability::Embedding);
    }

    #[test]
    fn test_interpretation_request_creation() {
        let request = InterpretationRequest {
            request_id: Uuid::new_v4().to_string(),
            capability: ProviderCapability::TextInterpretation,
            source_text: "remind me to call the roofer".to_string(),
            instructions: "extract action items".to_string(),
            context: RequestContext {
                capture_instant: Utc::now(),
                timezone_id: "America/New_York".to_string(),
                locale: "en-US".to_string(),
                source_revision: 0,
            },
        };

        assert_eq!(request.capability, ProviderCapability::TextInterpretation);
        assert_eq!(request.source_text, "remind me to call the roofer");
    }

    #[test]
    fn test_response_serialization() {
        let response = InterpretationResponse {
            request_id: Uuid::new_v4().to_string(),
            status: ResponseStatus::Success,
            result: Some(InterpretationResult {
                annotations: Some(serde_json::json!({"type": "action"})),
                raw_output: "{}".to_string(),
            }),
            error: None,
            usage: Some(UsageInfo {
                input_tokens: Some(100),
                output_tokens: Some(50),
            }),
        };

        let json = serde_json::to_string(&response).expect("should serialize");
        let deserialized: InterpretationResponse =
            serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(response.request_id, deserialized.request_id);
        assert_eq!(response.status, deserialized.status);
    }
}
