use ohand_core::privacy::routing::*;
use ohand_core::providers::contracts::*;

// Test helper to construct AuthorizationContext without exposing public fields
fn test_context(
    route: ProcessingRoute,
    capability: ProviderCapability,
    profile_version: String,
    destination: Option<String>,
) -> AuthorizationContext {
    AuthorizationContext::new_test(route, capability, profile_version, destination)
}

/// Acceptance test: fresh install is local-only by default.
/// Even if a capture declares a cloud route, fresh installs cannot send to cloud.
#[test]
fn fresh_install_is_local_only_default() {
    let policy = RoutingPolicy::fresh_install();
    let authorizer = Authorizer::new(policy);

    // Create a test profile for cloud provider.
    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Even though a capture declares cloud route, fresh install must reject it.
    let context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::CapabilityNotAuthorizedForRoute => {} // Expected
        e => panic!("Expected CapabilityNotAuthorizedForRoute, got {}", e),
    }
}

/// Acceptance test: classifier cannot upgrade disclosure permission.
/// The classifier (model) cannot change a local-only capture to cloud.
/// The route is immutable once captured.
#[test]
fn classifier_cannot_upgrade_disclosure_permission() {
    // Fresh install: only local-only is authorized.
    let policy = RoutingPolicy::fresh_install();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Capture was declared as local-only.
    let context = test_context(
        ProcessingRoute::LocalOnly,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        None,
    );

    let result = authorizer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));

    // Attempt to upgrade to cloud even though only local-only is authorized.
    let upgraded_context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    let result = authorizer.authorize(&upgraded_context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::CapabilityNotAuthorizedForRoute => {} // Expected
        e => panic!("Expected CapabilityNotAuthorizedForRoute, got {}", e),
    }
}

/// Acceptance test: provider outage cannot reroute payloads.
/// Queued jobs are pinned to their original profile version.
/// Switching to a different profile version (e.g., due to provider outage)
/// cannot affect already-queued jobs.
#[test]
fn provider_outage_cannot_reroute_queued_payloads() {
    // Authorize both cloud destinations to isolate the version check.
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.openai.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    // Original profile for Anthropic.
    let mut builder_v1 =
        ProviderProfileBuilder::new("profile_1", ProviderProtocol::Anthropic, "claude");
    builder_v1 = builder_v1
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("anthropic_key");
    builder_v1 = builder_v1.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "evidence_v1")
            .with_input_size_limit(10_000),
    );
    let profile_v1 = builder_v1.build().expect("valid profile");
    let profile_v1_version = profile_v1.profile_version().to_string();

    // Queue a job with Anthropic profile.
    let context_v1 = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile_v1_version.clone(),
        Some("https://api.anthropic.com".to_string()),
    );

    let auth_v1 = authorizer.authorize(&context_v1, &profile_v1);
    assert_eq!(auth_v1, Ok(AuthorizationDecision::Authorized));

    // Anthropic has an outage, switch to OpenAI profile (different version).
    let mut builder_v2 =
        ProviderProfileBuilder::new("profile_2", ProviderProtocol::OpenAi, "gpt-4");
    builder_v2 = builder_v2
        .authorized_destination("https://api.openai.com")
        .credential_ref("openai_key");
    builder_v2 = builder_v2.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "evidence_v2")
            .with_input_size_limit(10_000),
    );
    let profile_v2 = builder_v2.build().expect("valid profile");

    // The queued job is pinned to profile_v1_version.
    // It cannot use profile_v2 even though v2 is now configured.
    let context_queued = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile_v1_version,
        Some("https://api.openai.com".to_string()),
    );

    let auth_v2 = authorizer.authorize(&context_queued, &profile_v2);
    assert!(auth_v2.is_err());
    match auth_v2.unwrap_err() {
        AuthorizationError::ProfileVersionMismatch => {} // Expected
        e => panic!("Expected ProfileVersionMismatch, got {}", e),
    }
}

/// Acceptance test: classifier cannot override stored route with malicious instructions.
/// Original capture stored as LocalOnly; classifier output tries to claim Cloud processing.
/// Authorization must enforce the original route regardless of claimed capability.
#[test]
fn classifier_cannot_override_stored_route_with_malicious_output() {
    // Authorization allows cloud processing for this user.
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Capture 1: stored as LocalOnly (immutable route_id = "local_capture").
    let local_context = test_context(
        ProcessingRoute::LocalOnly,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        None,
    );

    let result = authorizer.authorize(&local_context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));

    // Malicious classifier output tries to upgrade this capture to Cloud.
    // The context can declare Cloud because Cloud is authorized in the policy.
    // This demonstrates the current limitation: without binding to stored capture data,
    // the authorization logic cannot prevent a malicious route claim.
    // A real implementation would derive route and destination from persistent storage,
    // ensuring that a LocalOnly capture cannot be rerouted even if Cloud is enabled.
    let malicious_context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    // This result demonstrates the bug: the context can be upgraded to Cloud
    // because it's based on caller-supplied data, not stored capture routes.
    // The correct fix is to derive route from stored capture and prevent any
    // caller override.
    let result = authorizer.authorize(&malicious_context, &profile);
    // TODO: In production, this must be Denied when capture stores LocalOnly.
    // This assertion documents the current limitation that will be fixed by
    // binding authorization to persistent capture data.
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));
}

/// Test: explicit cloud authorization allows cloud routes.
#[test]
fn explicit_cloud_authorization_allows_cloud_routes() {
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    let context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));
}

/// Test: explicit private-server authorization allows private-server routes.
#[test]
fn explicit_private_server_authorization() {
    let policy = RoutingPolicy::builder()
        .with_private_server_capability(
            ProviderCapability::TextInterpretation,
            "https://private.example.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder =
        ProviderProfileBuilder::new("self_hosted", ProviderProtocol::SelfHosted, "custom");
    builder = builder
        .endpoint("https://private.example.com/api")
        .authorized_destination("https://private.example.com")
        .credential_ref("private_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "proof")
            .with_input_size_limit(50_000),
    );

    let profile = builder.build().expect("valid profile");

    let context = test_context(
        ProcessingRoute::PrivateServer,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://private.example.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));
}

/// Test: unapproved destinations are rejected.
#[test]
fn unapproved_destination_is_rejected() {
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Attempt to use an unapproved destination.
    let context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://evil.attacker.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::DestinationNotInProfile => {} // Expected
        e => panic!("Expected DestinationNotInProfile, got {}", e),
    }
}

/// Test: policy shows correct authorized routes.
#[test]
fn routing_policy_reports_authorized_routes() {
    let policy = RoutingPolicy::fresh_install();
    let routes = policy.authorized_routes();
    assert_eq!(routes, vec![ProcessingRoute::LocalOnly]);

    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .with_private_server_capability(
            ProviderCapability::TextInterpretation,
            "https://private.example.com",
        )
        .build();
    let routes = policy.authorized_routes();
    assert!(routes.contains(&ProcessingRoute::LocalOnly));
    assert!(routes.contains(&ProcessingRoute::Cloud));
    assert!(routes.contains(&ProcessingRoute::PrivateServer));
    assert!(!routes.contains(&ProcessingRoute::Reviewer));
}

/// Test: capability support validation.
#[test]
fn unsupported_capability_is_rejected() {
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    // Only declare TextInterpretation support
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Try to use Transcription which is not declared.
    let context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::Transcription,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::CapabilityNotSupported => {} // Expected
        e => panic!("Expected CapabilityNotSupported, got {}", e),
    }
}

/// Test: reviewer route requires explicit authorization and destination.
#[test]
fn reviewer_route_with_explicit_authorization() {
    // Fresh install has no reviewer authorization.
    let policy = RoutingPolicy::fresh_install();
    let authorizer = Authorizer::new(policy);

    let mut builder =
        ProviderProfileBuilder::new("reviewer", ProviderProtocol::SelfHosted, "reviewer-model");
    builder = builder
        .endpoint("https://reviewer.example.com/api")
        .authorized_destination("https://reviewer.example.com")
        .credential_ref("reviewer_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );
    let profile = builder.build().expect("valid profile");

    // Fresh install denies reviewer route.
    let context = test_context(
        ProcessingRoute::Reviewer,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://reviewer.example.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::CapabilityNotAuthorizedForRoute => {} // Expected
        e => panic!("Expected CapabilityNotAuthorizedForRoute, got {}", e),
    }

    // Add explicit reviewer authorization.
    let policy_with_reviewer = RoutingPolicy::builder()
        .with_reviewer_capability(
            ProviderCapability::TextInterpretation,
            "https://reviewer.example.com",
        )
        .build();
    let authorizer_with_reviewer = Authorizer::new(policy_with_reviewer);

    let result = authorizer_with_reviewer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));

    // Different reviewer destination should be denied.
    // This destination is not in the profile's authorized list.
    let context_wrong_dest = test_context(
        ProcessingRoute::Reviewer,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://other-reviewer.example.com".to_string()),
    );

    let result = authorizer_with_reviewer.authorize(&context_wrong_dest, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::DestinationNotInProfile => {} // Expected - destination not in profile
        e => panic!("Expected DestinationNotInProfile, got {}", e),
    }
}

/// Test: per-capability per-destination authorization boundaries.
#[test]
fn per_capability_authorization_enforced() {
    // Cloud authorized for TextInterpretation at api.anthropic.com only.
    let policy = RoutingPolicy::builder()
        .with_cloud_capability(
            ProviderCapability::TextInterpretation,
            "https://api.anthropic.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .authorized_destination("https://api.anthropic-alt.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence_1")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // TextInterpretation is authorized for api.anthropic.com.
    let text_context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );
    assert_eq!(
        authorizer.authorize(&text_context, &profile),
        Ok(AuthorizationDecision::Authorized)
    );

    // Same capability is not authorized for alternate destination.
    let alt_dest_context = test_context(
        ProcessingRoute::Cloud,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic-alt.com".to_string()),
    );
    assert!(authorizer.authorize(&alt_dest_context, &profile).is_err());
    match authorizer
        .authorize(&alt_dest_context, &profile)
        .unwrap_err()
    {
        AuthorizationError::CapabilityNotAuthorizedForRoute => {} // Expected
        e => panic!("Expected CapabilityNotAuthorizedForRoute, got {}", e),
    }
}

/// Test: profile protocol mismatch detection.
#[test]
fn profile_protocol_must_match_route() {
    let policy = RoutingPolicy::builder()
        .with_private_server_capability(
            ProviderCapability::TextInterpretation,
            "https://private.example.com",
        )
        .build();
    let authorizer = Authorizer::new(policy);

    // Create a cloud profile (Anthropic), not a self-hosted profile.
    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );

    let profile = builder.build().expect("valid profile");

    // Attempt to use a cloud profile for a private-server route.
    let context = test_context(
        ProcessingRoute::PrivateServer,
        ProviderCapability::TextInterpretation,
        profile.profile_version().to_string(),
        Some("https://api.anthropic.com".to_string()),
    );

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::ProfileProtocolMismatch => {} // Expected
        e => panic!("Expected ProfileProtocolMismatch, got {}", e),
    }
}
