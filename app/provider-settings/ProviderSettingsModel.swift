import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The profile operations this screen uses. `ProviderConfigurationService` is the production implementation; the screen
/// reuses its operations and adds none.
protocol ProviderSettingsControlling: AnyObject {
    func createProfile(_ draft: ProviderProfileDraft) async throws -> ProviderProfileCreatedAnswer
    func setDefault(capability: String, profileVersion: String) async throws
    func clearDefault(capability: String) async throws
    func listProfiles() async throws -> ProviderProfileListAnswer
}

extension ProviderConfigurationService: ProviderSettingsControlling {}

/// A reason a draft cannot be saved yet, found before the core is asked.
enum ProviderSettingsIssue: Equatable, Hashable {
    case profileNameRequired
    case profileNameTooLong
    case modelRequired
    case modelTooLong
    case endpointRequired
    case endpointNotHTTPS
    case endpointHasQueryOrFragment
    case endpointNotAllowed
    case credentialRequired
    case credentialTooLong
    case timeoutOutOfBounds
}

/// The provider types the screen offers.
enum ProviderSettingsProtocol: String, CaseIterable, Equatable {
    case openAI = "open_ai"
    case anthropic
    case selfHosted = "self_hosted"

    var takesEndpoint: Bool { self == .selfHosted }
}

/// What the screen is showing.
enum ProviderSettingsPhase: Equatable {
    case idle
    case loading
    case loaded
    /// The read session is not authenticated, so no stored state is shown.
    case locked
    case failed(String)
}

/// What the last save or default change reported.
enum ProviderSettingsActionState: Equatable {
    case working
    case saved
    case defaultChanged
    case failed(String)

    var message: String? {
        switch self {
        case .working: return "Working…"
        case .saved: return "Saved as a new version. It is not the default until you choose it."
        case .defaultChanged: return "Default changed. Work already waiting keeps its original provider version."
        case .failed(let message): return message
        }
    }
}

/// The text the user is typing into the profile form.
struct ProviderSettingsDraft: Equatable {
    var profileName = ""
    var providerProtocol: ProviderSettingsProtocol = .openAI
    var endpoint = ""
    var model = ""
    var credentialReference = ""
    var timeoutSeconds = ProviderSettingsModel.defaultTimeoutSeconds
}

/// How the current default destination is described to the user.
enum ProviderSettingsOperatingMode: Equatable {
    /// No default: notes stay on the device.
    case localOnly
    case privateServer(model: String)
    case hosted(protocolName: String, model: String)
}

/// The state of the provider settings screen: stored profiles, the form for a new version and default changes.
///
/// Rules this type keeps:
/// * Stored state appears only while the read session is authenticated, checked before a call and again when its answer
///   arrives; a relock empties the screen and the form.
/// * Incomplete or unsupported drafts are refused here and never sent. The core still validates every accepted draft;
///   its answer decides what is stored, and nothing is reported as saved unless it succeeded.
/// * The destination is never typed: the draft carries a protocol and, for a private server, an https address. The core
///   derives the processing destination. A draft never names a credential secret.
/// * Saving a profile or changing a default calls only the profile service. It never touches jobs, route grants or
///   captures, so work already queued stays pinned to the version it was created with.
@MainActor
final class ProviderSettingsModel: ObservableObject {
    @Published private(set) var phase: ProviderSettingsPhase = .idle
    @Published private(set) var listing: ProviderProfileListAnswer?
    @Published private(set) var saveState: ProviderSettingsActionState?
    @Published private(set) var defaultState: ProviderSettingsActionState?
    @Published var draft = ProviderSettingsDraft()

    static let capability = "text_interpretation"
    static let defaultTimeoutSeconds = 60
    static let minimumTimeoutSeconds = 1
    static let maximumTimeoutSeconds = 300
    static let maximumProfileNameBytes = 128
    static let maximumModelBytes = 256
    static let maximumCredentialReferenceBytes = 256

    private let controls: ProviderSettingsControlling
    private let readScope: () -> CompositionReadScope
    private var generation = 0
    private var session = 0

    nonisolated init(controls: ProviderSettingsControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isSaving: Bool { saveState == .working }
    var isChangingDefault: Bool { defaultState == .working }

    /// Everything wrong with the draft as it stands; empty means it can be saved.
    var issues: [ProviderSettingsIssue] { Self.issues(for: draft) }
    var canSave: Bool { issues.isEmpty && !isSaving && phase == .loaded }

    static func issues(for draft: ProviderSettingsDraft) -> [ProviderSettingsIssue] {
        var found: [ProviderSettingsIssue] = []
        let name = draft.profileName.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty { found.append(.profileNameRequired) }
        else if name.utf8.count > maximumProfileNameBytes { found.append(.profileNameTooLong) }
        let model = draft.model.trimmingCharacters(in: .whitespacesAndNewlines)
        if model.isEmpty { found.append(.modelRequired) }
        else if model.utf8.count > maximumModelBytes { found.append(.modelTooLong) }
        let endpoint = draft.endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        if draft.providerProtocol.takesEndpoint {
            if endpoint.isEmpty {
                found.append(.endpointRequired)
            } else if endpoint.contains("?") || endpoint.contains("#") {
                found.append(.endpointHasQueryOrFragment)
            } else if !isHTTPSAddress(endpoint) {
                found.append(.endpointNotHTTPS)
            }
        } else if !endpoint.isEmpty {
            found.append(.endpointNotAllowed)
        }
        let credential = draft.credentialReference.trimmingCharacters(in: .whitespacesAndNewlines)
        if credential.isEmpty { found.append(.credentialRequired) }
        else if credential.utf8.count > maximumCredentialReferenceBytes { found.append(.credentialTooLong) }
        if draft.timeoutSeconds < minimumTimeoutSeconds || draft.timeoutSeconds > maximumTimeoutSeconds {
            found.append(.timeoutOutOfBounds)
        }
        return found
    }

    private static func isHTTPSAddress(_ text: String) -> Bool {
        guard let components = URLComponents(string: text), components.scheme?.lowercased() == "https",
            let host = components.host, !host.isEmpty
        else { return false }
        return true
    }

    /// The version currently serving text interpretation, if any.
    var defaultVersion: ProviderProfileListAnswer.Version? {
        guard let listing,
            let pointer = listing.defaults.first(where: { $0.capability == Self.capability })
        else { return nil }
        return listing.profiles.lazy.flatMap(\.versions).first { $0.profileVersion == pointer.profileVersion }
    }

    var operatingMode: ProviderSettingsOperatingMode {
        guard let version = defaultVersion else { return .localOnly }
        if version.providerProtocol == "self_hosted" { return .privateServer(model: version.model) }
        return .hosted(
            protocolName: ProviderSettingsText.protocolName(version.providerProtocol), model: version.model)
    }

    /// Whether `version` may be chosen as the default: unrevoked, and not declared unable to serve text interpretation (an unverified
    /// private server may still be chosen).
    func canMakeDefault(_ version: ProviderProfileListAnswer.Version) -> Bool {
        !version.revoked && !isChangingDefault && version.capabilities.contains {
            $0.capability == Self.capability && $0.support != "unsupported"
        } && defaultVersion?.profileVersion != version.profileVersion
    }

    /// Reads the stored profiles and defaults again.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        generation += 1
        let thisGeneration = generation
        if listing == nil { phase = .loading }
        do {
            let fresh = try await controls.listProfiles()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            listing = fresh
            phase = .loaded
        } catch {
            guard thisGeneration == generation else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock() }
            else { phase = .failed(ProviderSettingsText.failure(error)) }
        }
    }

    /// Stores the draft as a new immutable profile version. It does not become the default.
    func save() async {
        guard readScope() == .authenticated else { lock(); return }
        guard !isSaving else { return }
        guard issues.isEmpty else {
            saveState = .failed(issues.map(ProviderSettingsText.issue).joined(separator: " "))
            return
        }
        let thisSession = session
        let request = ProviderProfileDraft(
            profileID: draft.profileName.trimmingCharacters(in: .whitespacesAndNewlines),
            providerProtocol: draft.providerProtocol.rawValue,
            endpoint: draft.providerProtocol.takesEndpoint
                ? draft.endpoint.trimmingCharacters(in: .whitespacesAndNewlines) : nil,
            model: draft.model.trimmingCharacters(in: .whitespacesAndNewlines),
            credentialReference: draft.credentialReference.trimmingCharacters(in: .whitespacesAndNewlines),
            timeoutSeconds: draft.timeoutSeconds,
            capabilities: [Self.capability])
        saveState = .working
        do {
            _ = try await controls.createProfile(request)
            guard thisSession == session else { return }
            guard readScope() == .authenticated else { lock(); return }
            saveState = .saved
            draft = ProviderSettingsDraft()
            await refresh()
        } catch {
            guard thisSession == session else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            saveState = .failed(ProviderSettingsText.failure(error))
        }
    }

    /// Points the default at `version`. Queued work keeps the version it was pinned to.
    func makeDefault(_ version: ProviderProfileListAnswer.Version) async {
        guard readScope() == .authenticated else { lock(); return }
        guard !isChangingDefault else { return }
        guard canMakeDefault(version) else {
            defaultState = .failed(ProviderSettingsText.failure(ProviderConfigurationError.invalidRequest))
            return
        }
        await changeDefault { [controls] in
            try await controls.setDefault(capability: Self.capability, profileVersion: version.profileVersion)
        }
    }

    /// Removes the default so no provider is used and notes stay on the device.
    func useLocalOnly() async {
        guard readScope() == .authenticated else { lock(); return }
        guard !isChangingDefault else { return }
        await changeDefault { [controls] in try await controls.clearDefault(capability: Self.capability) }
    }

    /// Call when the screen goes away or the app locks: drops every stored detail and ignores answers still pending.
    func clear() {
        generation += 1
        session += 1
        listing = nil
        saveState = nil
        defaultState = nil
        draft = ProviderSettingsDraft()
        phase = .idle
    }

    private func changeDefault(_ operation: () async throws -> Void) async {
        let thisSession = session
        defaultState = .working
        do {
            try await operation()
            guard thisSession == session else { return }
            guard readScope() == .authenticated else { lock(); return }
            defaultState = .defaultChanged
        } catch {
            guard thisSession == session else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            defaultState = .failed(ProviderSettingsText.failure(error))
        }
        await refresh()
    }

    private func lock() {
        clear()
        phase = .locked
    }
}
