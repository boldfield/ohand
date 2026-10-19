import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The route-policy and profile operations this screen uses. `ProviderConfigurationService` is the production
/// implementation; the screen reuses its operations and adds none.
protocol PrivacySettingsControlling: AnyObject {
    func listRoutePolicy() async throws -> [RoutePolicyRoute]
    func listProfiles() async throws -> ProviderProfileListAnswer
    func grant(routeID: String, capability: String, profileVersion: String) async throws
        -> RoutePolicyChangeOutcome.Disposition
}

extension ProviderConfigurationService: PrivacySettingsControlling {}

/// The one permission the user is being asked about: a single capability on a single route for the destinations of a
/// single stored profile version. Confirming it grants exactly this and nothing else.
struct PrivacyGrantProposal: Equatable {
    let routeID: String
    let routeLabel: String
    let capability: String
    let profileVersion: String
    let versionLabel: String
}

/// A stored profile version the user could allow a capability for.
struct PrivacyOfferedVersion: Equatable {
    let profileID: String
    let version: ProviderProfileListAnswer.Version
}

enum PrivacySettingsPhase: Equatable {
    case idle
    case loading
    case loaded
    /// The read session is not authenticated, so no stored state is shown.
    case locked
    case failed(String)
}

enum PrivacySettingsActionState: Equatable {
    case working
    case allowed(String)
    case cancelled
    case failed(String)

    var message: String {
        switch self {
        case .working: return "Working…"
        case .allowed(let message): return message
        case .cancelled: return PrivacySettingsText.cancelledNotice
        case .failed(let message): return message
        }
    }
}

/// The state of the processing-policy screen: each route with what it may send and where, and the deliberate steps that
/// allow one more capability on one route for one destination.
///
/// Rules this type keeps:
/// * Local use needs nothing from here. A fresh install has routes with no permissions; the screen shows them as local
///   only and offers permissions without requiring any. A failed read leaves the screen explanatory, never blocking.
/// * A permission is granted only by `confirm`, and only for the proposal the user was shown. Opening a proposal,
///   cancelling it, a refresh, the listing of profiles or anything a classifier or provider produced never calls the
///   grant. A grant for one capability, route or profile version authorizes no other.
/// * Route labels, scopes and permissions are read from the core and shown as stored. This type never edits them
///   locally; they change only through a confirmed grant that the core accepted, then are read again.
/// * Stored state appears only while the read session is authenticated; a relock empties the screen and drops any open
///   proposal.
@MainActor
final class PrivacySettingsModel: ObservableObject {
    @Published private(set) var phase: PrivacySettingsPhase = .idle
    @Published private(set) var routes: [RoutePolicyRoute] = []
    @Published private(set) var listing: ProviderProfileListAnswer?
    @Published private(set) var proposal: PrivacyGrantProposal?
    @Published private(set) var actionState: PrivacySettingsActionState?

    /// The capabilities the core lets a user authorize for sending content.
    static let grantableCapabilities = ["text_interpretation", "review"]

    private let controls: PrivacySettingsControlling
    private let readScope: () -> CompositionReadScope
    private var generation = 0

    nonisolated init(controls: PrivacySettingsControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isWorking: Bool { actionState == .working }

    /// Whether any route may send anything off the device.
    var isLocalOnly: Bool { routes.allSatisfy { $0.grantedCapabilities.isEmpty } }

    /// Whether the route already holds a grant for `capability`.
    func isGranted(routeID: String, capability: String) -> Bool {
        routes.first { $0.routeID == routeID }?.grantedCapabilities.contains { $0.capability == capability } ?? false
    }

    /// Unrevoked stored versions that declare `capability` as supported or unverified, with their profile names, newest first.
    func offeredVersions(for capability: String) -> [PrivacyOfferedVersion] {
        guard let listing else { return [] }
        return listing.profiles.flatMap { profile in
            profile.versions.reversed().compactMap { version in
                guard !version.revoked,
                    version.capabilities.contains(where: { $0.capability == capability && $0.support != "unsupported" })
                else {
                    return nil
                }
                return PrivacyOfferedVersion(profileID: profile.profileID, version: version)
            }
        }
    }

    /// Reads routes and profiles again.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        generation += 1
        let thisGeneration = generation
        if phase != .loaded { phase = .loading }
        do {
            let freshRoutes = try await controls.listRoutePolicy()
            let freshListing = try await controls.listProfiles()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            routes = freshRoutes
            listing = freshListing
            phase = .loaded
        } catch {
            guard thisGeneration == generation else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            guard readScope() == .authenticated else { lock(); return }
            phase = .failed(PrivacySettingsText.loadFailure(error))
        }
    }

    /// Opens the question for one capability, route and profile version. Nothing is granted.
    func propose(routeID: String, capability: String, profileVersion: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking,
            Self.grantableCapabilities.contains(capability),
            let route = routes.first(where: { $0.routeID == routeID }),
            let found = offeredVersions(for: capability).first(where: { $0.version.profileVersion == profileVersion })
        else { return }
        actionState = nil
        proposal = PrivacyGrantProposal(
            routeID: route.routeID, routeLabel: PrivacySettingsText.routeLabel(route), capability: capability,
            profileVersion: profileVersion,
            versionLabel: PrivacySettingsText.versionLabel(profileID: found.profileID, version: found.version))
    }

    /// Closes the open question. Nothing is granted and nothing else changes.
    func cancel() {
        guard proposal != nil, !isWorking else { return }
        proposal = nil
        actionState = .cancelled
    }

    /// Grants exactly the shown proposal. `shown` must be the proposal on screen, so a stale confirmation of an earlier
    /// question grants nothing.
    func confirm(_ shown: PrivacyGrantProposal) async {
        guard readScope() == .authenticated else { lock(); return }
        guard let current = proposal, current == shown, !isWorking else { return }
        actionState = .working
        do {
            let outcome = try await controls.grant(
                routeID: current.routeID, capability: current.capability, profileVersion: current.profileVersion)
            proposal = nil
            actionState = .allowed(PrivacySettingsText.result(outcome, current))
        } catch {
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            proposal = nil
            actionState = .failed(PrivacySettingsText.failure(error))
        }
        await refresh()
    }

    /// Drops everything shown, as when the screen goes away.
    func clear() {
        generation += 1
        proposal = nil
        actionState = nil
        routes = []
        listing = nil
        if phase == .loaded || phase == .loading { phase = .idle }
    }

    private func lock() {
        generation += 1
        proposal = nil
        actionState = nil
        routes = []
        listing = nil
        phase = .locked
    }
}
