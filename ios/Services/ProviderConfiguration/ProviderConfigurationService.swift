import Foundation
import OhAndCoreBridge

/// Why a provider or route configuration call did not take effect.
public enum ProviderConfigurationError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty identifier).
    case invalidRequest
    /// The core decided against the request, or could not run it. The failure's `code` is one of the core's fixed codes
    /// (for example `unknown_protocol`, `credential_required`, `profile_revoked`, `route_unknown`); none echoes the
    /// request.
    case core(CoreFailure)
    case unreadable
}

/// Provider profiles, connection checks and route policy, as the settings flows use them. Every rule belongs to the core:
/// validation and versioning of profiles, which version may be a default, which destinations a grant covers and what a
/// connection check reports. This service adds the authentication requirement and typed results.
///
/// * **Authenticated.** Every call, including the listings, needs an authenticated read session when it is made. A
///   capture-only session gets `notAuthenticated` and the core is not asked, so nothing that decides what leaves the
///   device (a profile, a default, a grant, a revocation, a connection check) can change or run from a capture-only
///   caller. A listing whose session relocked while it ran is dropped.
/// * **Explicit.** Creating a profile, setting a default or granting a route moves no queued or waiting work. A
///   connection check runs only when called and sends one fixed synthetic request with no user content; it needs no
///   grant, because the user checks before granting.
/// * **No secrets.** The only credential this service handles is the opaque reference a draft names. Entering the secret
///   itself is `ProviderCredentialEntry`'s job, and no call here returns a secret, a reference or a response body.
public final class ProviderConfigurationService: @unchecked Sendable {
    private let profiles: ProviderProfileCore
    private let connectionChecks: ProviderConnectionCheckCore
    private let routePolicy: RoutePolicyCore
    private let session: ReadAuthenticationSession

    init(
        profiles: ProviderProfileCore, connectionChecks: ProviderConnectionCheckCore, routePolicy: RoutePolicyCore,
        session: ReadAuthenticationSession
    ) {
        self.profiles = profiles
        self.connectionChecks = connectionChecks
        self.routePolicy = routePolicy
        self.session = session
    }

    // MARK: profiles

    /// Stores a new immutable version of the profile and returns its identifier and version.
    public func createProfile(_ draft: ProviderProfileDraft) async throws -> ProviderProfileCreatedAnswer {
        guard Self.isIdentifier(draft.profileID) else { throw ProviderConfigurationError.invalidRequest }
        let answer = try await perform(isRead: false) { completion in
            self.profiles.submit(.createProfile(draft), completion: completion)
        }
        guard case .created(let created) = answer else { throw ProviderConfigurationError.unreadable }
        return created
    }

    /// Points the capability's default (`text_interpretation` or `review`) at one unrevoked stored version. Existing
    /// jobs keep the version they were pinned to.
    public func setDefault(capability: String, profileVersion: String) async throws {
        guard Self.isIdentifier(capability), Self.isIdentifier(profileVersion) else {
            throw ProviderConfigurationError.invalidRequest
        }
        let answer = try await perform(isRead: false) { completion in
            self.profiles.submit(.setDefault(capability: capability, profileVersion: profileVersion), completion: completion)
        }
        guard case .defaultSet = answer else { throw ProviderConfigurationError.unreadable }
    }

    /// Removes the capability's default. Allowed when none is set.
    public func clearDefault(capability: String) async throws {
        guard Self.isIdentifier(capability) else { throw ProviderConfigurationError.invalidRequest }
        let answer = try await perform(isRead: false) { completion in
            self.profiles.submit(.clearDefault(capability: capability), completion: completion)
        }
        guard case .defaultCleared = answer else { throw ProviderConfigurationError.unreadable }
    }

    /// Every profile with its versions, and the default per capability. A credential is reported only as bound or not.
    public func listProfiles() async throws -> ProviderProfileListAnswer {
        let answer = try await perform(isRead: true) { completion in
            self.profiles.submit(.list, completion: completion)
        }
        guard case .listing(let listing) = answer else { throw ProviderConfigurationError.unreadable }
        return listing
    }

    // MARK: connection check

    /// Sends one fixed synthetic request to the stored profile version's own destination and reports a redacted
    /// classification. A failure to run the check at all (a missing or revoked version) throws `core`.
    public func checkConnection(profileVersion: String) async throws -> ProviderConnectionOutcome {
        guard Self.isIdentifier(profileVersion) else { throw ProviderConfigurationError.invalidRequest }
        return try await perform(isRead: false) { completion in
            self.connectionChecks.check(profileVersion: profileVersion, completion: completion)
        }
    }

    // MARK: route policy

    /// Grants `capability` (`text_interpretation` or `review`) on `routeID` for the destinations of the stored profile
    /// version. The caller never names destinations. Returns `granted`, or `unchanged` when the policy already matched.
    @discardableResult
    public func grant(routeID: String, capability: String, profileVersion: String) async throws
        -> RoutePolicyChangeOutcome.Disposition
    {
        guard Self.isIdentifier(routeID), Self.isIdentifier(capability), Self.isIdentifier(profileVersion) else {
            throw ProviderConfigurationError.invalidRequest
        }
        return try await change(.grant(routeID: routeID, capability: capability, profileVersion: profileVersion))
    }

    /// Revokes the capability's grant on `routeID`. Jobs already queued for it then fail authorization at dispatch.
    @discardableResult
    public func revoke(routeID: String, capability: String) async throws -> RoutePolicyChangeOutcome.Disposition {
        guard Self.isIdentifier(routeID), Self.isIdentifier(capability) else {
            throw ProviderConfigurationError.invalidRequest
        }
        return try await change(.revoke(routeID: routeID, capability: capability))
    }

    /// Every route's name, scope, granted capabilities and destination classes.
    public func listRoutePolicy() async throws -> [RoutePolicyRoute] {
        let answer = try await perform(isRead: true) { completion in
            self.routePolicy.submit(.list, completion: completion)
        }
        guard case .listing(let listing) = answer else { throw ProviderConfigurationError.unreadable }
        return listing.routes
    }

    // MARK: private

    private func change(_ command: RoutePolicyCommand) async throws -> RoutePolicyChangeOutcome.Disposition {
        let answer = try await perform(isRead: false) { completion in
            self.routePolicy.submit(command, completion: completion)
        }
        guard case .changed(let outcome) = answer else { throw ProviderConfigurationError.unreadable }
        return outcome.disposition
    }

    private func perform<Answer: Sendable>(
        isRead: Bool,
        start: (@escaping @Sendable (Result<Answer, CoreFailure>) -> Void) -> Void
    ) async throws -> Answer {
        guard let authorization = session.currentAuthorization() else {
            throw ProviderConfigurationError.notAuthenticated
        }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<Answer, CoreFailure>, Never>) in
            start { continuation.resume(returning: $0) }
        }
        if isRead, !session.isCurrent(authorization) { throw ProviderConfigurationError.notAuthenticated }
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "invalid_request": throw ProviderConfigurationError.invalidRequest
            case "read_not_authenticated", "read_relocked": throw ProviderConfigurationError.notAuthenticated
            default: throw ProviderConfigurationError.core(failure)
            }
        }
    }

    private static func isIdentifier(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 512
    }
}
