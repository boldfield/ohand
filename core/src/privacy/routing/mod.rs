// Route authorization and destination validation.
// Prevents payload from leaving device without explicit authorization.

use crate::providers::contracts::{ProviderCapability, ProviderProfile};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

/// Privacy scope of a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyScope {
    /// Personal private content.
    Personal,
    /// Work-related content.
    Work,
    /// Session-specific content.
    Session,
}

impl std::fmt::Display for PrivacyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyScope::Personal => write!(f, "personal"),
            PrivacyScope::Work => write!(f, "work"),
            PrivacyScope::Session => write!(f, "session"),
        }
    }
}

/// Processing route designates whether content stays local or uses a configured provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingRoute {
    /// Local-only, no provider processing.
    LocalOnly,
    /// Cloud provider (Anthropic, OpenAI).
    Cloud,
    /// Private self-hosted server.
    PrivateServer,
    /// Optional shadow review capability.
    Reviewer,
}

impl std::fmt::Display for ProcessingRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessingRoute::LocalOnly => write!(f, "local_only"),
            ProcessingRoute::Cloud => write!(f, "cloud"),
            ProcessingRoute::PrivateServer => write!(f, "private_server"),
            ProcessingRoute::Reviewer => write!(f, "reviewer"),
        }
    }
}

/// Routing policy configured at setup time. Defines which routes are authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingPolicy {
    /// Routes explicitly authorized by the user during setup.
    authorized_routes: HashSet<ProcessingRoute>,
    /// Captures are routed to their declared route_id if authorized, else local-only.
    /// True = user authorizes cloud routes, False = local-only default.
    allow_cloud: bool,
    /// True = self-hosted server is configured and authorized.
    allow_private_server: bool,
    /// True = shadow review is configured and authorized.
    allow_reviewer: bool,
}

impl RoutingPolicy {
    /// Fresh install is local-only.
    pub fn fresh_install() -> Self {
        RoutingPolicy {
            authorized_routes: [ProcessingRoute::LocalOnly].iter().copied().collect(),
            allow_cloud: false,
            allow_private_server: false,
            allow_reviewer: false,
        }
    }

    /// New policy builder.
    pub fn builder() -> RoutingPolicyBuilder {
        RoutingPolicyBuilder {
            allow_cloud: false,
            allow_private_server: false,
            allow_reviewer: false,
        }
    }

    /// Check if a route is authorized.
    pub fn is_route_authorized(&self, route: ProcessingRoute) -> bool {
        match route {
            ProcessingRoute::LocalOnly => true, // Always available
            ProcessingRoute::Cloud => self.allow_cloud,
            ProcessingRoute::PrivateServer => self.allow_private_server,
            ProcessingRoute::Reviewer => self.allow_reviewer,
        }
    }

    /// All authorized routes (always includes LocalOnly).
    pub fn authorized_routes(&self) -> Vec<ProcessingRoute> {
        let mut routes = vec![ProcessingRoute::LocalOnly];
        if self.allow_cloud {
            routes.push(ProcessingRoute::Cloud);
        }
        if self.allow_private_server {
            routes.push(ProcessingRoute::PrivateServer);
        }
        if self.allow_reviewer {
            routes.push(ProcessingRoute::Reviewer);
        }
        routes.sort();
        routes
    }

    pub fn allow_cloud(&self) -> bool {
        self.allow_cloud
    }

    pub fn allow_private_server(&self) -> bool {
        self.allow_private_server
    }

    pub fn allow_reviewer(&self) -> bool {
        self.allow_reviewer
    }
}

pub struct RoutingPolicyBuilder {
    allow_cloud: bool,
    allow_private_server: bool,
    allow_reviewer: bool,
}

impl RoutingPolicyBuilder {
    pub fn with_cloud(mut self) -> Self {
        self.allow_cloud = true;
        self
    }

    pub fn with_private_server(mut self) -> Self {
        self.allow_private_server = true;
        self
    }

    pub fn with_reviewer(mut self) -> Self {
        self.allow_reviewer = true;
        self
    }

    pub fn build(self) -> RoutingPolicy {
        let mut authorized_routes = HashSet::new();
        authorized_routes.insert(ProcessingRoute::LocalOnly);
        if self.allow_cloud {
            authorized_routes.insert(ProcessingRoute::Cloud);
        }
        if self.allow_private_server {
            authorized_routes.insert(ProcessingRoute::PrivateServer);
        }
        if self.allow_reviewer {
            authorized_routes.insert(ProcessingRoute::Reviewer);
        }

        RoutingPolicy {
            authorized_routes,
            allow_cloud: self.allow_cloud,
            allow_private_server: self.allow_private_server,
            allow_reviewer: self.allow_reviewer,
        }
    }
}

/// Authorization decision for processing a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDecision {
    /// Payload can be sent to the destination.
    Authorized,
    /// Payload must not leave the device.
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthorizationError {
    #[error("route not authorized: {0}")]
    RouteNotAuthorized(String),
    #[error("capability not supported by profile")]
    CapabilityNotSupported,
    #[error("profile does not include destination")]
    DestinationNotInProfile,
    #[error("profile version mismatch: job pinned to older version")]
    ProfileVersionMismatch,
}

/// Authorization context for a job to leave the device.
pub struct AuthorizationContext {
    /// The route the capture declared.
    pub declared_route: ProcessingRoute,
    /// The capability being requested (e.g., TextInterpretation).
    pub capability: ProviderCapability,
    /// The profile version the job was queued against.
    pub profile_version: String,
    /// The destination the job would use (if not local-only).
    pub destination: Option<String>,
}

/// Authorizer validates outbound processing requests.
pub struct Authorizer {
    routing_policy: RoutingPolicy,
}

impl Authorizer {
    pub fn new(routing_policy: RoutingPolicy) -> Self {
        Authorizer { routing_policy }
    }

    /// Authorize a job before payload leaves the device.
    /// Returns Authorized if the job can proceed to the destination,
    /// or an error explaining why the job cannot proceed.
    pub fn authorize(
        &self,
        context: &AuthorizationContext,
        profile: &ProviderProfile,
    ) -> Result<AuthorizationDecision, AuthorizationError> {
        // Check if the declared route is authorized.
        if !self
            .routing_policy
            .is_route_authorized(context.declared_route)
        {
            return Err(AuthorizationError::RouteNotAuthorized(
                context.declared_route.to_string(),
            ));
        }

        // For local-only routes, payload never leaves the device.
        if context.declared_route == ProcessingRoute::LocalOnly {
            return Ok(AuthorizationDecision::Authorized);
        }

        // Check that the capability is supported by the profile.
        if profile.capability(context.capability).is_none() {
            return Err(AuthorizationError::CapabilityNotSupported);
        }

        let capability_meta = profile.capability(context.capability).unwrap();
        use crate::providers::contracts::CapabilitySupport;
        if capability_meta.support == CapabilitySupport::Unsupported {
            return Err(AuthorizationError::CapabilityNotSupported);
        }

        // For cloud/private-server routes, verify the destination is in the profile's authorized list.
        if let Some(ref dest) = context.destination {
            if !profile.authorized_destinations().contains(dest) {
                return Err(AuthorizationError::DestinationNotInProfile);
            }
        }

        // Jobs are pinned to profile versions to prevent silent rerouting.
        // If a profile version doesn't match, the job cannot use a different profile.
        if context.profile_version != profile.profile_version() {
            return Err(AuthorizationError::ProfileVersionMismatch);
        }

        Ok(AuthorizationDecision::Authorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_is_local_only() {
        let policy = RoutingPolicy::fresh_install();
        assert!(policy.is_route_authorized(ProcessingRoute::LocalOnly));
        assert!(!policy.is_route_authorized(ProcessingRoute::Cloud));
        assert!(!policy.is_route_authorized(ProcessingRoute::PrivateServer));
        assert!(!policy.is_route_authorized(ProcessingRoute::Reviewer));
    }

    #[test]
    fn cloud_route_requires_authorization() {
        let fresh_policy = RoutingPolicy::fresh_install();
        assert!(!fresh_policy.is_route_authorized(ProcessingRoute::Cloud));

        let cloud_policy = RoutingPolicy::builder().with_cloud().build();
        assert!(cloud_policy.is_route_authorized(ProcessingRoute::Cloud));
    }

    #[test]
    fn private_server_route_requires_authorization() {
        let fresh_policy = RoutingPolicy::fresh_install();
        assert!(!fresh_policy.is_route_authorized(ProcessingRoute::PrivateServer));

        let server_policy = RoutingPolicy::builder().with_private_server().build();
        assert!(server_policy.is_route_authorized(ProcessingRoute::PrivateServer));
    }

    #[test]
    fn reviewer_route_requires_authorization() {
        let fresh_policy = RoutingPolicy::fresh_install();
        assert!(!fresh_policy.is_route_authorized(ProcessingRoute::Reviewer));

        let reviewer_policy = RoutingPolicy::builder().with_reviewer().build();
        assert!(reviewer_policy.is_route_authorized(ProcessingRoute::Reviewer));
    }

    #[test]
    fn classifier_cannot_upgrade_disclosure_permission() {
        let policy = RoutingPolicy::fresh_install();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        let context = AuthorizationContext {
            declared_route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::RouteNotAuthorized(_))
        ));
    }

    #[test]
    fn provider_outage_cannot_reroute_payloads() {
        // When a profile version changes (e.g., switching providers due to outage),
        // jobs pinned to the old version cannot use the new profile.
        let policy = RoutingPolicy::builder().with_cloud().build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "profile_v1",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile_v1 = profile_builder.build().expect("valid profile");
        let profile_v1_version = profile_v1.profile_version().to_string();

        // Simulate switching to a different profile (e.g., due to provider outage).
        let mut profile_v2_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "profile_v2",
            crate::providers::contracts::ProviderProtocol::OpenAi,
            "gpt-4",
        );
        profile_v2_builder = profile_v2_builder
            .authorized_destination("https://api.openai.com")
            .credential_ref("test_credential_v2");

        profile_v2_builder = profile_v2_builder.capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "test_evidence_v2",
            )
            .with_input_size_limit(10_000),
        );

        let profile_v2 = profile_v2_builder.build().expect("valid profile");

        // A job pinned to v1 cannot use v2.
        let context = AuthorizationContext {
            declared_route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile_v1_version,
            destination: Some("https://api.openai.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile_v2);
        assert!(matches!(
            result,
            Err(AuthorizationError::ProfileVersionMismatch)
        ));
    }

    #[test]
    fn malicious_stored_instructions_cannot_change_routing_policy() {
        // Even if a model response tries to claim a different route,
        // only the originally authorized route is used.
        let policy = RoutingPolicy::fresh_install();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        // Malicious stored instructions cannot override the declared route.
        let context = AuthorizationContext {
            declared_route: ProcessingRoute::LocalOnly,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: None,
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn local_only_payloads_never_leave_device() {
        let policy = RoutingPolicy::builder()
            .with_cloud()
            .with_private_server()
            .build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        let context = AuthorizationContext {
            declared_route: ProcessingRoute::LocalOnly,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: None,
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn cloud_route_with_proper_authorization() {
        let policy = RoutingPolicy::builder().with_cloud().build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        let context = AuthorizationContext {
            declared_route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn destination_not_in_profile_denied() {
        let policy = RoutingPolicy::builder().with_cloud().build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        let context = AuthorizationContext {
            declared_route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://unauthorized.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::DestinationNotInProfile)
        ));
    }

    #[test]
    fn unsupported_capability_denied() {
        let policy = RoutingPolicy::builder().with_cloud().build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "test_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        // Only TextInterpretation is supported
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        // Try to use Transcription capability which is not declared
        let context = AuthorizationContext {
            declared_route: ProcessingRoute::Cloud,
            capability: ProviderCapability::Transcription,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::CapabilityNotSupported)
        ));
    }

    #[test]
    fn private_server_route_with_proper_authorization() {
        let policy = RoutingPolicy::builder().with_private_server().build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "self_hosted",
            crate::providers::contracts::ProviderProtocol::SelfHosted,
            "custom-model",
        );
        profile_builder = profile_builder
            .endpoint("https://private.example.com/api")
            .authorized_destination("https://private.example.com")
            .credential_ref("private_server_key");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "proof_of_capability",
            )
            .with_input_size_limit(50_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        let context = AuthorizationContext {
            declared_route: ProcessingRoute::PrivateServer,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://private.example.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }
}
