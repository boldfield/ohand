//! Integration tests for provider contracts and fake provider harness.
//! Tests focus on contract enforcement, validation, and harness behavior.

use ohand_core::providers::contracts::{
    fake::{FakeBehavior, FakeProvider},
    CapabilityMetadata, CapabilitySupport, CredentialRef, ErrorType, InterpretationRequest,
    InterpretationResponse, ProfileValidationError, ProviderCapability, ProviderProtocol,
    RequestContext, ResponseStatus,
};
use uuid::Uuid;

#[test]
fn test_profile_validation_rejects_all_unsupported_capabilities() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.credential_ref = Some(CredentialRef {
        ref_id: "test-cred".to_string(),
    });

    profile.capabilities.clear();
    profile.capabilities.insert(
        ProviderCapability::TextInterpretation,
        CapabilityMetadata {
            capability: ProviderCapability::TextInterpretation,
            support_state: CapabilitySupport::Unsupported,
            input_size_limit: None,
            structured_output_supported: false,
        },
    );
    profile.capabilities.insert(
        ProviderCapability::Transcription,
        CapabilityMetadata {
            capability: ProviderCapability::Transcription,
            support_state: CapabilitySupport::Unverified,
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
fn test_profile_validation_rejects_inconsistent_capability_keys() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.credential_ref = Some(CredentialRef {
        ref_id: "test-cred".to_string(),
    });

    profile.capabilities.clear();
    profile.capabilities.insert(
        ProviderCapability::TextInterpretation,
        CapabilityMetadata {
            capability: ProviderCapability::Transcription,
            support_state: CapabilitySupport::Supported,
            input_size_limit: None,
            structured_output_supported: false,
        },
    );

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::InconsistentCapabilityMetadata)
    ));
}

#[test]
fn test_profile_validation_rejects_empty_profile_id() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.profile_id = String::new();
    profile.credential_ref = Some(CredentialRef {
        ref_id: "test-cred".to_string(),
    });

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::EmptyProfileId)
    ));
}

#[test]
fn test_profile_validation_rejects_empty_credential_ref_id() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.credential_ref = Some(CredentialRef {
        ref_id: String::new(),
    });

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::EmptyCredentialRef)
    ));
}

#[test]
fn test_profile_validation_rejects_test_fake_in_normal_flow() {
    let profile = FakeProvider::fake_test_profile();

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::TestFakeNotAllowed)
    ));
}

#[test]
fn test_profile_validation_rejects_empty_profile_version() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.profile_version = String::new();
    profile.credential_ref = Some(CredentialRef {
        ref_id: "test-cred".to_string(),
    });

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::InvalidProfileVersion)
    ));
}

#[test]
fn test_profile_validation_rejects_unsupported_schema_version() {
    let mut profile = FakeProvider::fake_test_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.schema_version = 99;
    profile.credential_ref = Some(CredentialRef {
        ref_id: "test-cred".to_string(),
    });

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::UnsupportedSchemaVersion)
    ));
}

#[test]
fn test_valid_anthropic_profile() {
    let profile = FakeProvider::anthropic_profile();
    assert!(profile.validate().is_ok());
}

#[test]
fn test_fake_provider_bounds_at_exact_limit() {
    let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    let result = response.result.expect("oversized behavior returns result");
    assert_eq!(result.raw_output.len(), provider.max_output_size());
}

#[test]
fn test_fake_provider_bounds_below_limit() {
    let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    let result = response.result.expect("oversized behavior returns result");
    assert!(result.raw_output.len() <= provider.max_output_size());
}

#[test]
fn test_interpretation_request_with_full_contract() {
    let profile_version = Uuid::new_v4().to_string();
    let request = InterpretationRequest {
        request_id: Uuid::new_v4().to_string(),
        request_version: 1,
        capture_id: Uuid::new_v4().to_string(),
        capability: ProviderCapability::TextInterpretation,
        source_text: "remind me to call the roofer".to_string(),
        text_basis: "original source capture".to_string(),
        instructions: "extract action items and dates".to_string(),
        profile_id: "anthropic-test".to_string(),
        profile_version: profile_version.clone(),
        authorization_route: "interpreter".to_string(),
        context: RequestContext {
            capture_instant: chrono::Utc::now(),
            timezone_id: "America/New_York".to_string(),
            locale: "en-US".to_string(),
            source_revision: 0,
            utc_offset_seconds: -18000,
        },
    };

    assert_eq!(request.profile_version, profile_version);
    assert_eq!(request.request_version, 1);
    assert!(!request.capture_id.is_empty());
    assert!(!request.text_basis.is_empty());
    assert!(!request.authorization_route.is_empty());
    assert!(request.context.utc_offset_seconds != 0);
}

#[test]
fn test_request_response_serialization() {
    let profile_version = Uuid::new_v4().to_string();
    let request = InterpretationRequest {
        request_id: Uuid::new_v4().to_string(),
        request_version: 1,
        capture_id: Uuid::new_v4().to_string(),
        capability: ProviderCapability::TextInterpretation,
        source_text: "test input".to_string(),
        text_basis: "original".to_string(),
        instructions: "test".to_string(),
        profile_id: "test".to_string(),
        profile_version,
        authorization_route: "test".to_string(),
        context: RequestContext {
            capture_instant: chrono::Utc::now(),
            timezone_id: "UTC".to_string(),
            locale: "en-US".to_string(),
            source_revision: 0,
            utc_offset_seconds: 0,
        },
    };

    let json = serde_json::to_string(&request).expect("should serialize");
    let deserialized: InterpretationRequest =
        serde_json::from_str(&json).expect("should deserialize");

    assert_eq!(request.request_id, deserialized.request_id);
    assert_eq!(request.capture_id, deserialized.capture_id);
    assert_eq!(request.profile_version, deserialized.profile_version);
}

#[test]
fn test_response_with_all_fields() {
    let response = InterpretationResponse {
        request_id: Uuid::new_v4().to_string(),
        status: ResponseStatus::Success,
        result: Some(ohand_core::providers::contracts::InterpretationResult {
            annotations: Some(serde_json::json!({"type": "action"})),
            raw_output: r#"{"type":"action"}"#.to_string(),
        }),
        error: None,
        usage: Some(ohand_core::providers::contracts::UsageInfo {
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

#[test]
fn test_profile_versioning_is_immutable_and_unique() {
    let profile1 = FakeProvider::anthropic_profile();
    let profile2 = FakeProvider::anthropic_profile();

    assert_ne!(profile1.profile_version, profile2.profile_version);
    assert_eq!(profile1.schema_version, profile2.schema_version);
    assert!(!profile1.profile_version.is_empty());
    assert!(!profile2.profile_version.is_empty());
}

#[test]
fn test_profile_version_incompatibility_detection() {
    let mut profile = FakeProvider::anthropic_profile();
    profile.schema_version = 2;

    let result = profile.validate();
    assert!(matches!(
        result,
        Err(ProfileValidationError::UnsupportedSchemaVersion)
    ));
}

#[test]
fn test_fake_provider_has_no_history() {
    let provider = FakeProvider::new(FakeBehavior::Success);

    let req1 = Uuid::new_v4().to_string();
    let req2 = Uuid::new_v4().to_string();

    let resp1 = provider.interpret(&req1);
    let resp2 = provider.interpret(&req2);

    assert_eq!(resp1.status, resp2.status);
    assert_ne!(resp1.request_id, resp2.request_id);
}

#[test]
fn test_capability_support_state_validation() {
    let mut profile = FakeProvider::anthropic_profile();

    profile.capabilities.insert(
        ProviderCapability::Transcription,
        CapabilityMetadata {
            capability: ProviderCapability::Transcription,
            support_state: CapabilitySupport::Unsupported,
            input_size_limit: None,
            structured_output_supported: false,
        },
    );

    assert!(profile.validate().is_ok());
}

#[test]
fn test_input_size_limit_metadata() {
    let mut profile = FakeProvider::anthropic_profile();

    if let Some(cap) = profile
        .capabilities
        .get_mut(&ProviderCapability::TextInterpretation)
    {
        cap.input_size_limit = Some(50_000);
    }

    assert_eq!(
        profile
            .capabilities
            .get(&ProviderCapability::TextInterpretation)
            .and_then(|c| c.input_size_limit),
        Some(50_000)
    );
    assert!(profile.validate().is_ok());
}

#[test]
fn test_structured_output_support_metadata() {
    let mut profile = FakeProvider::anthropic_profile();

    if let Some(cap) = profile
        .capabilities
        .get_mut(&ProviderCapability::TextInterpretation)
    {
        cap.structured_output_supported = true;
    }

    assert!(profile
        .capabilities
        .get(&ProviderCapability::TextInterpretation)
        .is_some_and(|c| c.structured_output_supported));
    assert!(profile.validate().is_ok());
}

#[test]
fn test_fake_provider_invalid_output_detection() {
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
fn test_fake_provider_unavailable_detection() {
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
fn test_fake_provider_cancellation_detection() {
    let provider = FakeProvider::new(FakeBehavior::Cancelled);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    assert_eq!(response.status, ResponseStatus::Cancelled);
    assert!(response.error.is_some());
}

#[test]
fn test_credential_ref_is_opaque() {
    let cred_ref = CredentialRef {
        ref_id: "vault-key-abc-123".to_string(),
    };

    assert!(!cred_ref.ref_id.is_empty());
    assert!(!cred_ref.ref_id.contains("secret"));
    assert!(!cred_ref.ref_id.contains("password"));
}
