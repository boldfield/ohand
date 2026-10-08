// Route authorization and destination validation.
// Prevents payload from leaving device without explicit authorization.

use crate::providers::contracts::{ProviderCapability, ProviderProfile};
use anyhow::Result;
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

/// Per-capability per-destination authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityGrant {
    /// The capability being authorized.
    pub capability: ProviderCapability,
    /// The route this capability is authorized for.
    pub route: ProcessingRoute,
    /// The destination URL for this grant (required for non-local routes).
    pub destination: Option<String>,
}

/// Routing policy configured at setup time. Defines which routes and capabilities are authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingPolicy {
    /// Per-capability per-destination authorization grants.
    /// Maps (ProcessingRoute, ProviderCapability) -> authorized destinations.
    /// If destination is None for a non-local route, the capability is not authorized for that route.
    grants: HashSet<CapabilityGrant>,
}

impl RoutingPolicy {
    /// Fresh install is local-only.
    pub fn fresh_install() -> Self {
        RoutingPolicy {
            grants: HashSet::new(),
        }
    }

    /// New policy builder.
    pub fn builder() -> RoutingPolicyBuilder {
        RoutingPolicyBuilder {
            grants: HashSet::new(),
        }
    }

    /// Check if a capability can use a route with a specific destination.
    pub fn is_capability_authorized(
        &self,
        route: ProcessingRoute,
        capability: ProviderCapability,
        destination: Option<&str>,
    ) -> bool {
        match route {
            ProcessingRoute::LocalOnly => true, // Always available
            _ => {
                // For non-local routes, check if there's a matching grant.
                self.grants.iter().any(|grant| {
                    grant.route == route && grant.capability == capability && {
                        match destination {
                            Some(dest) => grant.destination.as_deref() == Some(dest),
                            None => false, // Non-local routes require a destination
                        }
                    }
                })
            }
        }
    }

    /// Check if a route is authorized for a capability (returns true if any destination works).
    pub fn is_route_authorized_for_capability(
        &self,
        route: ProcessingRoute,
        capability: ProviderCapability,
    ) -> bool {
        match route {
            ProcessingRoute::LocalOnly => true,
            _ => self
                .grants
                .iter()
                .any(|g| g.route == route && g.capability == capability),
        }
    }

    /// All authorized routes (always includes LocalOnly).
    pub fn authorized_routes(&self) -> Vec<ProcessingRoute> {
        let mut routes = vec![ProcessingRoute::LocalOnly];
        for grant in &self.grants {
            if !routes.contains(&grant.route) {
                routes.push(grant.route);
            }
        }
        routes.sort();
        routes
    }
}

pub struct RoutingPolicyBuilder {
    grants: HashSet<CapabilityGrant>,
}

impl RoutingPolicyBuilder {
    /// Authorize a capability for cloud routes with a specific destination.
    pub fn with_cloud_capability(
        mut self,
        capability: ProviderCapability,
        destination: impl Into<String>,
    ) -> Self {
        self.grants.insert(CapabilityGrant {
            capability,
            route: ProcessingRoute::Cloud,
            destination: Some(destination.into()),
        });
        self
    }

    /// Authorize a capability for private server routes with a specific destination.
    pub fn with_private_server_capability(
        mut self,
        capability: ProviderCapability,
        destination: impl Into<String>,
    ) -> Self {
        self.grants.insert(CapabilityGrant {
            capability,
            route: ProcessingRoute::PrivateServer,
            destination: Some(destination.into()),
        });
        self
    }

    /// Authorize a capability for reviewer routes with a specific destination.
    pub fn with_reviewer_capability(
        mut self,
        capability: ProviderCapability,
        destination: impl Into<String>,
    ) -> Self {
        self.grants.insert(CapabilityGrant {
            capability,
            route: ProcessingRoute::Reviewer,
            destination: Some(destination.into()),
        });
        self
    }

    pub fn build(self) -> RoutingPolicy {
        RoutingPolicy {
            grants: self.grants,
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
    #[error("route not authorized for this capability")]
    CapabilityNotAuthorizedForRoute,
    #[error("capability not supported by profile")]
    CapabilityNotSupported,
    #[error("destination required for non-local routes")]
    DestinationRequired,
    #[error("destination not in profile authorized list")]
    DestinationNotInProfile,
    #[error("profile protocol does not match the route")]
    ProfileProtocolMismatch,
    #[error("profile version mismatch: job pinned to older version")]
    ProfileVersionMismatch,
}

/// Authorization context for a job to leave the device.
/// The route, capability, and destination come from the stored capture and route authorizations,
/// not from untrusted caller input. This ensures the original user intent is preserved.
pub struct AuthorizationContext {
    /// The immutable route from the stored capture (route_id in schema).
    /// Never supplied by caller; always read from persistent storage.
    route: ProcessingRoute,
    /// The capability being requested (e.g., TextInterpretation).
    capability: ProviderCapability,
    /// The profile version the job was queued against.
    profile_version: String,
    /// The destination from the stored route authorization.
    /// Required for non-local routes; never supplied by caller.
    destination: Option<String>,
}

impl AuthorizationContext {
    /// For testing only: construct a context with specific values.
    /// In production, contexts are always derived from stored capture/route data.
    /// This method should not be used in production code; it exists to allow unit tests
    /// to exercise the authorization logic without database dependencies.
    pub fn new_test(
        route: ProcessingRoute,
        capability: ProviderCapability,
        profile_version: String,
        destination: Option<String>,
    ) -> Self {
        AuthorizationContext {
            route,
            capability,
            profile_version,
            destination,
        }
    }

    /// Production API: derive authorization context from persisted capture data.
    /// This method ensures that route, destination, and profile version are bound to
    /// the stored capture record, preventing any caller from supplying these values.
    ///
    /// Note: This is a placeholder showing the intended production interface.
    /// Implementation requires database access to read:
    /// 1. Capture by capture_id to get route_id
    /// 2. Route by route_id to determine ProcessingRoute type
    /// 3. RouteAuthorization by (route_id, capability) to get authorized destinations
    /// 4. Provider profile version to pin the job
    #[doc(hidden)]
    pub fn from_stored_capture(
        _capture_id: &str,
        _capability: ProviderCapability,
        _profile_version: String,
    ) -> Result<Self, String> {
        // This is a placeholder. Actual implementation requires:
        // 1. Database connection and access to read captures/routes/route_authorizations
        // 2. Logic to determine ProcessingRoute type from stored route record
        // 3. Verification that destination is in authorized_destinations for the capability
        // The key invariant: all values come from persisted storage, never from caller
        Err(
            "Not yet implemented: database-backed authorization requires D01/D02 schema support"
                .to_string(),
        )
    }
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
        // For local-only routes, payload never leaves the device.
        if context.route == ProcessingRoute::LocalOnly {
            return Ok(AuthorizationDecision::Authorized);
        }

        // For non-local routes, destination must be present.
        let destination = context
            .destination
            .as_deref()
            .ok_or(AuthorizationError::DestinationRequired)?;

        // Validate profile protocol matches the route type.
        Self::validate_profile_for_route(context.route, profile)?;

        // Check that the capability is supported by the profile.
        use crate::providers::contracts::CapabilitySupport;
        let capability_meta = profile
            .capability(context.capability)
            .ok_or(AuthorizationError::CapabilityNotSupported)?;

        if capability_meta.support == CapabilitySupport::Unsupported {
            return Err(AuthorizationError::CapabilityNotSupported);
        }

        // Verify the destination is in the profile's authorized list.
        if !profile
            .authorized_destinations()
            .contains(&destination.to_string())
        {
            return Err(AuthorizationError::DestinationNotInProfile);
        }

        // Check if this capability is authorized for this route and destination.
        if !self.routing_policy.is_capability_authorized(
            context.route,
            context.capability,
            Some(destination),
        ) {
            return Err(AuthorizationError::CapabilityNotAuthorizedForRoute);
        }

        // Jobs are pinned to profile versions to prevent silent rerouting.
        // If a profile version doesn't match, the job cannot use a different profile.
        if context.profile_version != profile.profile_version() {
            return Err(AuthorizationError::ProfileVersionMismatch);
        }

        Ok(AuthorizationDecision::Authorized)
    }

    /// Validate that the profile's protocol matches the route type.
    fn validate_profile_for_route(
        route: ProcessingRoute,
        profile: &ProviderProfile,
    ) -> Result<(), AuthorizationError> {
        use crate::providers::contracts::ProviderProtocol;

        match route {
            ProcessingRoute::Cloud => match profile.protocol() {
                ProviderProtocol::Anthropic | ProviderProtocol::OpenAi => Ok(()),
                _ => Err(AuthorizationError::ProfileProtocolMismatch),
            },
            ProcessingRoute::PrivateServer => match profile.protocol() {
                ProviderProtocol::SelfHosted => Ok(()),
                _ => Err(AuthorizationError::ProfileProtocolMismatch),
            },
            ProcessingRoute::Reviewer => {
                // Reviewer can use cloud or self-hosted services with explicit authorization
                match profile.protocol() {
                    ProviderProtocol::Anthropic
                    | ProviderProtocol::OpenAi
                    | ProviderProtocol::SelfHosted => Ok(()),
                }
            }
            ProcessingRoute::LocalOnly => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_is_local_only() {
        let policy = RoutingPolicy::fresh_install();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "cloud_profile",
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

        // Fresh install denies cloud routes even with a cloud profile.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::CapabilityNotAuthorizedForRoute)
        ));
    }

    #[test]
    fn private_server_auth_cannot_use_cloud_profile() {
        // A private-server authorization must not accept a cloud provider profile.
        let policy = RoutingPolicy::builder()
            .with_private_server_capability(
                ProviderCapability::TextInterpretation,
                "https://private.example.com",
            )
            .build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "cloud_profile",
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

        // Attempting to send to cloud provider with private-server-only authorization fails.
        let context = AuthorizationContext {
            route: ProcessingRoute::PrivateServer,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::ProfileProtocolMismatch)
        ));
    }

    #[test]
    fn reviewer_auth_requires_explicit_destination() {
        // A reviewer authorization requires a specific destination.
        let policy = RoutingPolicy::builder()
            .with_reviewer_capability(
                ProviderCapability::TextInterpretation,
                "https://reviewer.example.com",
            )
            .build();
        let authorizer = Authorizer::new(policy);

        // Reviewer is a separate service with its own profile and destination.
        let mut reviewer_profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "reviewer_service",
            crate::providers::contracts::ProviderProtocol::SelfHosted,
            "reviewer-model",
        );
        reviewer_profile_builder = reviewer_profile_builder
            .endpoint("https://reviewer.example.com/api")
            .authorized_destination("https://reviewer.example.com")
            .credential_ref("reviewer_credential");

        use crate::providers::contracts::CapabilityMetadata;
        reviewer_profile_builder = reviewer_profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let reviewer_profile = reviewer_profile_builder.build().expect("valid profile");

        // Reviewer authorization with matching destination succeeds.
        let context = AuthorizationContext {
            route: ProcessingRoute::Reviewer,
            capability: ProviderCapability::TextInterpretation,
            profile_version: reviewer_profile.profile_version().to_string(),
            destination: Some("https://reviewer.example.com".to_string()),
        };

        let result = authorizer.authorize(&context, &reviewer_profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn missing_destination_denied_for_cloud() {
        // A cloud route with no destination is denied.
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
            .build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "cloud_profile",
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
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: None,
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::DestinationRequired)
        ));
    }

    #[test]
    fn classifier_cannot_upgrade_to_unauthorized_destination() {
        // Cloud authorized for TextInterpretation at api.anthropic.com only.
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
            .build();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "cloud_profile",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_builder = profile_builder
            .authorized_destination("https://api.anthropic.com")
            .authorized_destination("https://api.anthropic-alt.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_builder = profile_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile = profile_builder.build().expect("valid profile");

        // Attempt to use alternate destination even though only api.anthropic.com is authorized.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic-alt.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::CapabilityNotAuthorizedForRoute)
        ));
    }

    #[test]
    fn provider_outage_cannot_reroute_to_different_profile_version() {
        // Jobs pinned to a specific profile version cannot use a different version.
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
            .build();
        let authorizer = Authorizer::new(policy);

        let mut profile_v1_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "profile_v1",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet",
        );
        profile_v1_builder = profile_v1_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        use crate::providers::contracts::CapabilityMetadata;
        profile_v1_builder = profile_v1_builder.capability(
            CapabilityMetadata::supported(ProviderCapability::TextInterpretation, "test_evidence")
                .with_input_size_limit(10_000),
        );

        let profile_v1 = profile_v1_builder.build().expect("valid profile");
        let profile_v1_version = profile_v1.profile_version().to_string();

        // Create a different profile version (still Anthropic, but different model/config).
        let mut profile_v2_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "profile_v2",
            crate::providers::contracts::ProviderProtocol::Anthropic,
            "claude-3-5-sonnet", // same model but will get different version ID
        );
        profile_v2_builder = profile_v2_builder
            .authorized_destination("https://api.anthropic.com")
            .credential_ref("test_credential");

        profile_v2_builder = profile_v2_builder.capability(
            CapabilityMetadata::supported(
                ProviderCapability::TextInterpretation,
                "test_evidence_v2",
            )
            .with_input_size_limit(10_000),
        );

        let profile_v2 = profile_v2_builder.build().expect("valid profile");

        // Job pinned to v1 cannot use v2, even though both are authorized for the same destination.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile_v1_version,
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile_v2);
        assert!(matches!(
            result,
            Err(AuthorizationError::ProfileVersionMismatch)
        ));
    }

    #[test]
    fn malicious_stored_instructions_cannot_override_route() {
        // Even if a caller tries to declare a cloud route when only local is stored, local is used.
        let policy = RoutingPolicy::fresh_install();
        let authorizer = Authorizer::new(policy);

        let mut profile_builder = crate::providers::contracts::ProviderProfileBuilder::new(
            "cloud_profile",
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

        // Attempt to declare cloud route when no cloud authorization exists.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert!(matches!(
            result,
            Err(AuthorizationError::CapabilityNotAuthorizedForRoute)
        ));
    }

    #[test]
    fn local_only_payloads_never_leave_device() {
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
            route: ProcessingRoute::LocalOnly,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: None,
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn cloud_route_with_proper_authorization() {
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
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
            route: ProcessingRoute::Cloud,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://api.anthropic.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }

    #[test]
    fn destination_not_in_profile_denied() {
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
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

        // Even though cloud is authorized, this destination isn't.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
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
        let policy = RoutingPolicy::builder()
            .with_cloud_capability(
                ProviderCapability::TextInterpretation,
                "https://api.anthropic.com",
            )
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

        // Transcription is not declared in the profile.
        let context = AuthorizationContext {
            route: ProcessingRoute::Cloud,
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
        let policy = RoutingPolicy::builder()
            .with_private_server_capability(
                ProviderCapability::TextInterpretation,
                "https://private.example.com",
            )
            .build();
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
            route: ProcessingRoute::PrivateServer,
            capability: ProviderCapability::TextInterpretation,
            profile_version: profile.profile_version().to_string(),
            destination: Some("https://private.example.com".to_string()),
        };

        let result = authorizer.authorize(&context, &profile);
        assert_eq!(result, Ok(AuthorizationDecision::Authorized));
    }
}
