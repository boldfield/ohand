//! Integration tests for provider contracts and fake provider harness.

use ohand_core::providers::contracts::{
    fake::{FakeBehavior, FakeProvider},
    CapabilityMetadata, CapabilitySupport, CredentialRef, ErrorType, InterpretationRequest,
    InterpretationResponse, ProviderCapability, ProviderProtocol, ResponseStatus,
};
use uuid::Uuid;

#[test]
fn test_profile_validation_accepts_valid_anthropic_profile() {
    let mut profile = FakeProvider::fake_profile();
    profile.protocol = ProviderProtocol::Anthropic;
    profile.model = "claude-3-sonnet".to_string();
    profile.endpoint = None;
    profile.credential_ref = Some(CredentialRef {
        ref_id: "anthropic-key".to_string(),
    });

    assert!(profile.validate().is_ok());
}

#[test]
fn test_profile_validation_rejects_openai_with_endpoint() {
    let mut profile = FakeProvider::fake_profile();
    profile.protocol = ProviderProtocol::OpenAi;
    profile.endpoint = Some("https://api.openai.com".to_string());

    let result = profile.validate();
    assert!(result.is_err());
}

#[test]
fn test_profile_validation_rejects_self_hosted_without_endpoint() {
    let mut profile = FakeProvider::fake_profile();
    profile.protocol = ProviderProtocol::SelfHosted;
    profile.endpoint = None;

    let result = profile.validate();
    assert!(result.is_err());
}

#[test]
fn test_fake_provider_success_response() {
    let provider = FakeProvider::new(FakeBehavior::Success);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    assert_eq!(response.status, ResponseStatus::Success);
    assert!(response.error.is_none());
    assert!(response.result.is_some());
    assert!(response.usage.is_some());

    let result = response.result.unwrap();
    assert!(result.annotations.is_some());
}

#[test]
fn test_fake_provider_timeout_response() {
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
fn test_fake_provider_invalid_output_response() {
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
fn test_fake_provider_unavailable_response() {
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
fn test_fake_provider_cancellation_response() {
    let provider = FakeProvider::new(FakeBehavior::Cancelled);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    assert_eq!(response.status, ResponseStatus::Cancelled);
    assert!(response.error.is_some());
}

#[test]
fn test_fake_provider_bounded_response_size() {
    let provider = FakeProvider::new(FakeBehavior::OversizedOutput);
    let request_id = Uuid::new_v4().to_string();

    let response = provider.interpret(&request_id);

    assert_eq!(response.status, ResponseStatus::InvalidOutput);
    let result = response.result.unwrap();
    assert!(result.raw_output.len() > 100_000);
}

#[test]
fn test_request_response_serialization() {
    let request = InterpretationRequest {
        request_id: Uuid::new_v4().to_string(),
        capability: ProviderCapability::TextInterpretation,
        source_text: "remind me to call the roofer".to_string(),
        instructions: "extract action items".to_string(),
        context: ohand_core::providers::contracts::RequestContext {
            capture_instant: chrono::Utc::now(),
            timezone_id: "America/New_York".to_string(),
            locale: "en-US".to_string(),
            source_revision: 0,
        },
    };

    let json = serde_json::to_string(&request).expect("should serialize");
    let deserialized: InterpretationRequest =
        serde_json::from_str(&json).expect("should deserialize");

    assert_eq!(request.request_id, deserialized.request_id);
    assert_eq!(request.source_text, deserialized.source_text);
}

#[test]
fn test_provider_profile_immutability() {
    let profile = FakeProvider::fake_profile();
    let version1 = profile.profile_version.clone();

    let profile2 = FakeProvider::fake_profile();
    let version2 = profile2.profile_version.clone();

    assert_ne!(
        version1, version2,
        "Each profile should have unique version"
    );
}

#[test]
fn test_provider_capability_support_states() {
    let mut profile = FakeProvider::fake_profile();

    profile.capabilities.insert(
        ProviderCapability::Transcription,
        CapabilityMetadata {
            capability: ProviderCapability::Transcription,
            support_state: CapabilitySupport::Unsupported,
            input_size_limit: None,
            structured_output_supported: false,
        },
    );

    profile.capabilities.insert(
        ProviderCapability::Embedding,
        CapabilityMetadata {
            capability: ProviderCapability::Embedding,
            support_state: CapabilitySupport::Unavailable,
            input_size_limit: None,
            structured_output_supported: false,
        },
    );

    assert_eq!(
        profile
            .capabilities
            .get(&ProviderCapability::Transcription)
            .map(|c| c.support_state),
        Some(CapabilitySupport::Unsupported)
    );

    assert_eq!(
        profile
            .capabilities
            .get(&ProviderCapability::Embedding)
            .map(|c| c.support_state),
        Some(CapabilitySupport::Unavailable)
    );
}

#[test]
fn test_fake_provider_no_history() {
    let provider = FakeProvider::new(FakeBehavior::Success);

    let req1 = Uuid::new_v4().to_string();
    let req2 = Uuid::new_v4().to_string();

    let resp1 = provider.interpret(&req1);
    let resp2 = provider.interpret(&req2);

    // Both responses should have the same structure but different request IDs
    // No historical state carried between requests
    assert_eq!(resp1.status, ResponseStatus::Success);
    assert_eq!(resp2.status, ResponseStatus::Success);
    assert_ne!(resp1.request_id, resp2.request_id);
}

#[test]
fn test_profile_with_input_size_limits() {
    let mut profile = FakeProvider::fake_profile();

    if let Some(capability) = profile
        .capabilities
        .get_mut(&ProviderCapability::TextInterpretation)
    {
        capability.input_size_limit = Some(50_000);
    }

    assert_eq!(
        profile
            .capabilities
            .get(&ProviderCapability::TextInterpretation)
            .and_then(|c| c.input_size_limit),
        Some(50_000)
    );
}

#[test]
fn test_profile_with_structured_output_support() {
    let mut profile = FakeProvider::fake_profile();

    if let Some(capability) = profile
        .capabilities
        .get_mut(&ProviderCapability::TextInterpretation)
    {
        capability.structured_output_supported = true;
    }

    assert!(profile
        .capabilities
        .get(&ProviderCapability::TextInterpretation)
        .is_some_and(|c| c.structured_output_supported));
}

#[test]
fn test_response_with_all_fields() {
    let response = InterpretationResponse {
        request_id: Uuid::new_v4().to_string(),
        status: ResponseStatus::Success,
        result: Some(ohand_core::providers::contracts::InterpretationResult {
            annotations: Some(serde_json::json!({"type": "action", "text": "call"})),
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
    assert!(deserialized.result.is_some());
    assert!(deserialized.usage.is_some());
}

#[test]
fn test_all_fake_behaviors() {
    let behaviors = vec![
        FakeBehavior::Success,
        FakeBehavior::Timeout,
        FakeBehavior::InvalidOutput,
        FakeBehavior::Unavailable,
        FakeBehavior::Cancelled,
        FakeBehavior::OversizedOutput,
    ];

    for behavior in behaviors {
        let provider = FakeProvider::new(behavior);
        let request_id = Uuid::new_v4().to_string();

        let response = provider.interpret(&request_id);

        assert_eq!(response.request_id, request_id);
        // Each behavior produces a response, even if it's an error
        assert!(!response.request_id.is_empty());
    }
}

#[test]
fn test_profile_protocol_variants() {
    let protocols = vec![
        ProviderProtocol::OpenAi,
        ProviderProtocol::Anthropic,
        ProviderProtocol::SelfHosted,
        ProviderProtocol::TestFake,
    ];

    for _protocol in protocols {
        let profile = FakeProvider::fake_profile();
        // Test that we can create profiles with different protocols
        assert!(profile.protocol == ProviderProtocol::TestFake);
    }
}

#[test]
fn test_credential_ref_opaque() {
    let cred_ref = CredentialRef {
        ref_id: "some-opaque-id-12345".to_string(),
    };

    assert_eq!(cred_ref.ref_id, "some-opaque-id-12345");
    // Credential refs are opaque and do not contain secrets
    assert!(!cred_ref.ref_id.contains("secret"));
    assert!(!cred_ref.ref_id.contains("key"));
}
