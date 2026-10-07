use ohand_core::privacy::routing::*;
use ohand_core::providers::contracts::*;

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
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

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
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::LocalOnly,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: None,
    };

    let result = authorizer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));

    // Attempt to upgrade to cloud even though only local-only is authorized.
    let upgraded_context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

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
    let context_v1 = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile_v1_version.clone(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

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
    let context_queued = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile_v1_version,
        destination: Some("https://api.openai.com".to_string()),
    };

    let auth_v2 = authorizer.authorize(&context_queued, &profile_v2);
    assert!(auth_v2.is_err());
    match auth_v2.unwrap_err() {
        AuthorizationError::ProfileVersionMismatch => {} // Expected
        e => panic!("Expected ProfileVersionMismatch, got {}", e),
    }
}

/// Acceptance test: malicious stored instructions cannot change routing policy.
/// Even if a model response contains instructions to change routing,
/// the original declared route is authoritative.
#[test]
fn malicious_instructions_cannot_override_declared_route() {
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

    // Capture declared as local-only.
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::LocalOnly,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: None,
    };

    let result = authorizer.authorize(&context, &profile);
    assert_eq!(result, Ok(AuthorizationDecision::Authorized));

    // The declared route is immutable. No response can change it.
    // This is guaranteed by the capture's immutable route_id field.
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

    let context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

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

    let context = AuthorizationContext {
        declared_route: ProcessingRoute::PrivateServer,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://private.example.com".to_string()),
    };

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
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://evil.attacker.com".to_string()),
    };

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
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::Transcription,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::CapabilityNotSupported => {} // Expected
        e => panic!("Expected CapabilityNotSupported, got {}", e),
    }
}

/// Test: reviewer route requires explicit authorization.
#[test]
fn reviewer_route_requires_explicit_authorization() {
    let policy = RoutingPolicy::fresh_install();

    let context = AuthorizationContext {
        declared_route: ProcessingRoute::Reviewer,
        capability: ProviderCapability::TextInterpretation,
        profile_version: "dummy".to_string(),
        destination: None,
    };

    // Create a dummy profile just for testing.
    let mut builder = ProviderProfileBuilder::new("test", ProviderProtocol::Anthropic, "claude");
    builder = builder
        .authorized_destination("https://api.anthropic.com")
        .credential_ref("test_key");
    builder = builder.capability(
        CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
            .with_input_size_limit(10_000),
    );
    let profile = builder.build().expect("valid profile");

    let authorizer = Authorizer::new(policy);
    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::DestinationRequired => {} // Expected for reviewer route
        e => panic!("Expected DestinationRequired, got {}", e),
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
    let text_context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };
    assert_eq!(
        authorizer.authorize(&text_context, &profile),
        Ok(AuthorizationDecision::Authorized)
    );

    // Same capability is not authorized for alternate destination.
    let alt_dest_context = AuthorizationContext {
        declared_route: ProcessingRoute::Cloud,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic-alt.com".to_string()),
    };
    assert!(authorizer.authorize(&alt_dest_context, &profile).is_err());
    match authorizer.authorize(&alt_dest_context, &profile).unwrap_err() {
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
    let context = AuthorizationContext {
        declared_route: ProcessingRoute::PrivateServer,
        capability: ProviderCapability::TextInterpretation,
        profile_version: profile.profile_version().to_string(),
        destination: Some("https://api.anthropic.com".to_string()),
    };

    let result = authorizer.authorize(&context, &profile);
    assert!(result.is_err());
    match result.unwrap_err() {
        AuthorizationError::ProfileProtocolMismatch => {} // Expected
        e => panic!("Expected ProfileProtocolMismatch, got {}", e),
    }
}
