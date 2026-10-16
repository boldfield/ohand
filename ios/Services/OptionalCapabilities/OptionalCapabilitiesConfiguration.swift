import Foundation

/// Why an optional capability is off. Fixed labels only: never a route, endpoint, credential reference or content.
public enum OptionalCapabilityDisabledReason: String, Equatable, Sendable {
    case notConfigured = "not_configured"
    case missingProfileVersion = "missing_profile_version"
    case evidenceIncomplete = "evidence_incomplete"
    case modelNotVerified = "model_not_verified"
    case reviewProfileMissing = "review_profile_missing"
    case reviewProfileNotIndependent = "review_profile_not_independent"
    case policyDisabled = "policy_disabled"
}

public enum OptionalCapabilityState: Equatable, Sendable {
    case enabled
    case disabled(OptionalCapabilityDisabledReason)

    public var isEnabled: Bool { self == .enabled }
}

/// Whether the core's store holds a usable profile for a verified registration. Fixed labels only.
public enum SelfHostedProfileAvailability: Equatable, Sendable {
    case unchecked
    case available
    case unavailable(Reason)

    public enum Reason: String, Equatable, Sendable {
        case missing
        case notSelfHosted = "not_self_hosted"
        case revoked
        case modelNotServed = "model_not_served"
        case checkFailed = "check_failed"
    }
}

/// Registers the verified self-hosted provider. It names the installed profile and the model the profile serves, and
/// carries the evidence the claim rests on. It holds no endpoint, credential or route: the profile in the core's store
/// is the only place those live, so there is no default endpoint to fall back to.
public struct SelfHostedRegistration: Equatable, Sendable {
    public var profileVersion: String
    public var model: String
    public var evidence: [SelfHostedEvidenceReference]

    public init(
        profileVersion: String,
        model: String,
        evidence: [SelfHostedEvidenceReference] = SelfHostedVerifiedBoundary.current.evidence
    ) {
        self.profileVersion = profileVersion
        self.model = model
        self.evidence = evidence
    }
}

/// The optional capabilities a build offers. The default offers none.
public struct OptionalCapabilitiesConfiguration {
    public var selfHosted: SelfHostedRegistration?
    public var shadowReview: ShadowReviewConfiguration?

    public init(selfHosted: SelfHostedRegistration? = nil, shadowReview: ShadowReviewConfiguration? = nil) {
        self.selfHosted = selfHosted
        self.shadowReview = shadowReview
    }

    public static let none = OptionalCapabilitiesConfiguration()
}

/// Decides whether a registration is complete enough to be offered. These are local preconditions only: the core stays
/// the authority on routes, grants, revocation and credential references for every request.
public enum OptionalCapabilityVerification {
    public static func selfHostedState(
        _ registration: SelfHostedRegistration?,
        boundary: SelfHostedVerifiedBoundary = .current
    ) -> OptionalCapabilityState {
        guard let registration else { return .disabled(.notConfigured) }
        guard !registration.profileVersion.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return .disabled(.missingProfileVersion)
        }
        for expected in boundary.evidence where !registration.evidence.contains(expected) {
            return .disabled(.evidenceIncomplete)
        }
        guard registration.model == boundary.model else { return .disabled(.modelNotVerified) }
        return .enabled
    }

    /// Shadow review needs a named review profile and an enabled budget. When the self-hosted provider is also offered,
    /// the review profile must be a different profile, so the reviewer is independent of the primary.
    public static func shadowReviewState(
        _ configuration: ShadowReviewConfiguration?,
        selfHosted: SelfHostedRegistration?
    ) -> OptionalCapabilityState {
        guard let configuration else { return .disabled(.notConfigured) }
        guard
            let reviewProfileVersion = configuration.reviewProfileVersion,
            !reviewProfileVersion.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            return .disabled(.reviewProfileMissing)
        }
        guard configuration.policy.isEnabled else { return .disabled(.policyDisabled) }
        if let selfHosted, selfHosted.profileVersion == reviewProfileVersion {
            return .disabled(.reviewProfileNotIndependent)
        }
        return .enabled
    }
}
