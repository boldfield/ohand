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
pub mod normalizer;

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
    #[serde(rename = "unverified")]
    Unverified,
}

impl CapabilitySupport {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapabilitySupport::Supported => "supported",
            CapabilitySupport::Unsupported => "unsupported",
            CapabilitySupport::Unverified => "unverified",
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
/// profile_version is an immutable UUID; any change creates a new version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub schema_version: u32,
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
            schema_version: 1,
            profile_version: Uuid::new_v4().to_string(),
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
        if self.profile_id.is_empty() {
            return Err(ProfileValidationError::EmptyProfileId);
        }

        if self.profile_version.is_empty() {
            return Err(ProfileValidationError::InvalidProfileVersion);
        }

        if self.schema_version == 0 {
            return Err(ProfileValidationError::InvalidSchemaVersion);
        }

        if self.schema_version > 1 {
            return Err(ProfileValidationError::UnsupportedSchemaVersion);
        }

        if self.model.is_empty() {
            return Err(ProfileValidationError::MissingModel);
        }

        if self.timeout_seconds == 0 {
            return Err(ProfileValidationError::InvalidTimeout);
        }

        if self.protocol == ProviderProtocol::TestFake {
            return Err(ProfileValidationError::TestFakeNotAllowed);
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
                if let Some(ref endpoint) = self.endpoint {
                    if !self.authorized_destinations.contains(endpoint) {
                        return Err(ProfileValidationError::EndpointNotInAuthorizedDestinations);
                    }
                }
            }
            ProviderProtocol::TestFake => unreachable!(),
        }

        if let Some(ref cred) = self.credential_ref {
            if cred.ref_id.is_empty() {
                return Err(ProfileValidationError::EmptyCredentialRef);
            }
        } else {
            return Err(ProfileValidationError::MissingCredential);
        }

        if self.authorized_destinations.is_empty() {
            return Err(ProfileValidationError::MissingAuthorizedDestinations);
        }

        for dest in &self.authorized_destinations {
            if dest.is_empty() {
                return Err(ProfileValidationError::EmptyAuthorizedDestination);
            }
        }

        if self.capabilities.is_empty() {
            return Err(ProfileValidationError::NoCapabilities);
        }

        let mut has_supported = false;
        for (key, metadata) in &self.capabilities {
            if key != &metadata.capability {
                return Err(ProfileValidationError::InconsistentCapabilityMetadata);
            }
            if metadata.support_state == CapabilitySupport::Supported {
                has_supported = true;
            }
        }

        if !has_supported {
            return Err(ProfileValidationError::NoSupportedCapabilities);
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProfileValidationError {
    #[error("Profile ID cannot be empty")]
    EmptyProfileId,
    #[error("Profile version cannot be empty")]
    InvalidProfileVersion,
    #[error("Schema version cannot be zero")]
    InvalidSchemaVersion,
    #[error("Unsupported schema version")]
    UnsupportedSchemaVersion,
    #[error("Profile is missing model")]
    MissingModel,
    #[error("Profile has invalid timeout (must be > 0)")]
    InvalidTimeout,
    #[error("TestFake protocol is only for testing")]
    TestFakeNotAllowed,
    #[error("Profile has unexpected endpoint for protocol")]
    UnexpectedEndpoint,
    #[error("Profile is missing endpoint for self-hosted protocol")]
    MissingEndpoint,
    #[error("Self-hosted endpoint is not in authorized destinations")]
    EndpointNotInAuthorizedDestinations,
    #[error("Profile is missing credential reference")]
    MissingCredential,
    #[error("Credential reference ID cannot be empty")]
    EmptyCredentialRef,
    #[error("Profile has no authorized destinations")]
    MissingAuthorizedDestinations,
    #[error("Authorized destination cannot be empty")]
    EmptyAuthorizedDestination,
    #[error("Profile declares no capabilities")]
    NoCapabilities,
    #[error("All capabilities are unsupported or unverified")]
    NoSupportedCapabilities,
    #[error("Capability map key does not match metadata")]
    InconsistentCapabilityMetadata,
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

/// Normalized interpretation request contract pinned to an immutable profile version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationRequest {
    pub request_id: String,
    pub request_version: u32,
    pub capture_id: String,
    pub capability: ProviderCapability,
    pub source_text: String,
    pub text_basis: String,
    pub instructions: String,
    pub profile_id: String,
    pub profile_version: String,
    pub authorization_route: String,
    pub context: RequestContext,
}

/// Request context: capture time, timezone, locale, and complete time context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub capture_instant: DateTime<Utc>,
    pub timezone_id: String,
    pub locale: String,
    pub source_revision: u32,
    pub utc_offset_seconds: i32,
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
    use uuid::Uuid;

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
    fn test_profile_validation_empty_profile_id() {
        let mut profile = ProviderProfile::new(
            "".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
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
            Err(ProfileValidationError::EmptyProfileId)
        ));
    }

    #[test]
    fn test_profile_validation_missing_model() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
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
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
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
            Err(ProfileValidationError::InvalidTimeout)
        ));
    }

    #[test]
    fn test_profile_validation_empty_credential_ref() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "".to_string(),
        });
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
            Err(ProfileValidationError::EmptyCredentialRef)
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
    fn test_profile_validation_no_supported_capabilities() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
        profile.authorized_destinations = vec!["local".to_string()];
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Unsupported,
                input_size_limit: None,
                structured_output_supported: false,
            },
        );

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::NoSupportedCapabilities)
        ));
    }

    #[test]
    fn test_profile_validation_inconsistent_capability_metadata() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::Anthropic,
            "claude-3-sonnet".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
        profile.authorized_destinations = vec!["local".to_string()];
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::Transcription,
                support_state: CapabilitySupport::Supported,
                input_size_limit: None,
                structured_output_supported: true,
            },
        );

        let result = profile.validate();
        assert!(matches!(
            result,
            Err(ProfileValidationError::InconsistentCapabilityMetadata)
        ));
    }

    #[test]
    fn test_profile_validation_test_fake_rejected() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::TestFake,
            "fake-model".to_string(),
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
            Err(ProfileValidationError::TestFakeNotAllowed)
        ));
    }

    #[test]
    fn test_profile_validation_self_hosted_missing_endpoint() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::SelfHosted,
            "llama-3".to_string(),
        );
        profile.credential_ref = Some(CredentialRef {
            ref_id: "test-cred".to_string(),
        });
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
        let endpoint = "http://localhost:8000".to_string();
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::SelfHosted,
            "llama-3".to_string(),
        );
        profile.endpoint = Some(endpoint.clone());
        profile.credential_ref = Some(CredentialRef {
            ref_id: "local-cred".to_string(),
        });
        profile.authorized_destinations = vec![endpoint];
        profile.capabilities.insert(
            ProviderCapability::TextInterpretation,
            CapabilityMetadata {
                capability: ProviderCapability::TextInterpretation,
                support_state: CapabilitySupport::Supported,
                input_size_limit: None,
                structured_output_supported: true,
            },
        );

        assert!(profile.validate().is_ok());
    }

    #[test]
    fn test_profile_validation_endpoint_not_in_authorized_destinations() {
        let mut profile = ProviderProfile::new(
            "test-profile".to_string(),
            ProviderProtocol::SelfHosted,
            "llama-3".to_string(),
        );
        profile.endpoint = Some("http://localhost:8000".to_string());
        profile.credential_ref = Some(CredentialRef {
            ref_id: "local-cred".to_string(),
        });
        profile.authorized_destinations = vec!["other-destination".to_string()];
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
            Err(ProfileValidationError::EndpointNotInAuthorizedDestinations)
        ));
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
        let profile_version = Uuid::new_v4().to_string();
        let request = InterpretationRequest {
            request_id: Uuid::new_v4().to_string(),
            request_version: 1,
            capture_id: Uuid::new_v4().to_string(),
            capability: ProviderCapability::TextInterpretation,
            source_text: "remind me to call the roofer".to_string(),
            text_basis: "original capture text".to_string(),
            instructions: "extract action items".to_string(),
            profile_id: "test-profile".to_string(),
            profile_version: profile_version.clone(),
            authorization_route: "default".to_string(),
            context: RequestContext {
                capture_instant: Utc::now(),
                timezone_id: "America/New_York".to_string(),
                locale: "en-US".to_string(),
                source_revision: 0,
                utc_offset_seconds: -18000,
            },
        };

        assert_eq!(request.capability, ProviderCapability::TextInterpretation);
        assert_eq!(request.source_text, "remind me to call the roofer");
        assert_eq!(request.profile_version, profile_version);
        assert_eq!(request.request_version, 1);
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
