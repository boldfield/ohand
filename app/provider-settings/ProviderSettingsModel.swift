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
    func checkConnection(profileVersion: String) async throws -> ProviderConnectionOutcome
}

extension ProviderConfigurationService: ProviderSettingsControlling {}

/// The credential operations this screen uses. `ProviderCredentialEntry` is the production implementation: it hands the
/// secret straight to the native credential service and returns only an opaque reference, a status or a redacted error.
/// No call returns a secret.
protocol ProviderSettingsCredentialControlling: AnyObject {
    func addCredential(_ secret: Data) throws -> String
    func updateCredential(_ secret: Data, reference: String) throws
    func deleteCredential(reference: String) throws
    func credentialStatus(reference: String) throws -> CredentialStatus
    /// By stored profile version: the services layer resolves the version's reference itself and never returns it.
    func replaceCredential(_ secret: Data, profileVersion: String) async throws
    func credentialStatus(profileVersion: String) async throws -> CredentialStatus
    func removeCredential(profileVersion: String) async throws
}

extension ProviderCredentialEntry: ProviderSettingsCredentialControlling {}

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

/// Whether a stored version can serve text interpretation, from the support the core declared.
enum ProviderSettingsAvailability: Equatable {
    case ready
    /// Declared but without verification evidence: the core will not send work to it.
    case unverified
    case unavailable
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

/// What the last credential entry, removal or status check reported. It never holds a secret or a reference.
enum ProviderSettingsCredentialState: Equatable {
    case working
    case stored
    case updated
    case removed
    case status(CredentialStatus)
    case failed(String)

    var message: String { ProviderSettingsText.credentialState(self) }
}

/// What a connection check of one stored version reported.
enum ProviderSettingsConnectionState: Equatable {
    case checking
    case finished(ProviderConnectionOutcome)
    case failed(String)

    var message: String { ProviderSettingsText.connectionState(self) }
}

/// The text the user is typing into the profile form.
struct ProviderSettingsDraft: Equatable {
    var profileName = ""
    /// Choosing a provider type that takes no address drops any address typed earlier: its field is no longer shown,
    /// so a leftover value would block saving with nothing on screen to clear.
    var providerProtocol: ProviderSettingsProtocol = .openAI {
        didSet { if !providerProtocol.takesEndpoint { endpoint = "" } }
    }
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
/// * A secret is never held here. It arrives as `Data` from a field that clears itself, goes directly to the credential
///   service and is not stored, published or reported; only an opaque reference and fixed messages come back.
/// * A connection check runs only when the user asks for one version, sends the core's fixed synthetic request (no
///   capture text) and shows only the core's fixed outcome. Nothing runs one in the background.
/// * Saving a profile or changing a default calls only the profile service. It never touches jobs, route grants or
///   captures, so work already queued stays pinned to the version it was created with.
@MainActor
final class ProviderSettingsModel: ObservableObject {
    @Published private(set) var phase: ProviderSettingsPhase = .idle
    @Published private(set) var listing: ProviderProfileListAnswer?
    @Published private(set) var saveState: ProviderSettingsActionState?
    @Published private(set) var defaultState: ProviderSettingsActionState?
    @Published private(set) var credentialState: ProviderSettingsCredentialState?
    /// What the last secret replacement, status check or removal reported for a stored version.
    @Published private(set) var savedCredentialStates: [String: ProviderSettingsCredentialState] = [:]
    @Published private(set) var connectionStates: [String: ProviderSettingsConnectionState] = [:]
    @Published var draft = ProviderSettingsDraft() {
        didSet { if let added = addedReference, draft.credentialReference != added { discardAddedReference() } }
    }

    static let capability = "text_interpretation"
    static let defaultTimeoutSeconds = 60
    static let minimumTimeoutSeconds = 1
    static let maximumTimeoutSeconds = 300
    static let maximumProfileNameBytes = 128
    static let maximumModelBytes = 256
    static let maximumCredentialReferenceBytes = 256

    private let controls: ProviderSettingsControlling
    private let credentials: ProviderSettingsCredentialControlling
    private let readScope: () -> CompositionReadScope
    private var generation = 0
    private var session = 0
    /// The opaque reference of a secret this screen stored that no saved profile version names yet. It is deleted if
    /// the draft is abandoned, so the screen never leaves a secret that nothing can find or remove.
    private var addedReference: String?
    /// Opaque references whose deletion could not be done when the draft was abandoned (for example the session had
    /// locked). They are retried the next time the screen reads while authenticated. They name no secret bytes.
    private var referencesAwaitingDeletion: [String] = []

    nonisolated init(
        controls: ProviderSettingsControlling, credentials: ProviderSettingsCredentialControlling,
        readScope: @escaping () -> CompositionReadScope
    ) {
        self.controls = controls
        self.credentials = credentials
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

    /// Whether `version` can serve text interpretation: the core's dispatcher uses only capabilities declared supported.
    func availability(of version: ProviderProfileListAnswer.Version) -> ProviderSettingsAvailability {
        switch version.capabilities.first(where: { $0.capability == Self.capability })?.support {
        case "supported": return .ready
        case "unverified": return .unverified
        default: return .unavailable
        }
    }

    /// Whether the current default can actually process notes. Nil when no default is chosen.
    var defaultAvailability: ProviderSettingsAvailability? { defaultVersion.map(availability(of:)) }

    /// Whether `version` may be chosen as the default: unrevoked and declared supported. An unverified or unsupported
    /// version is shown as unavailable, because the core refuses to dispatch to it.
    func canMakeDefault(_ version: ProviderProfileListAnswer.Version) -> Bool {
        !version.revoked && !isChangingDefault && availability(of: version) == .ready
            && defaultVersion?.profileVersion != version.profileVersion
    }

    /// Reads the stored profiles and defaults again.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        sweepAbandonedReferences()
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
            credentialState = nil
            addedReference = nil
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

    // MARK: credentials

    /// Stores `secret` for the draft: a new credential when the draft names none, otherwise a replacement behind the
    /// named reference. The draft then names the opaque reference. The secret is passed on and not kept.
    func storeCredential(_ secret: Data) {
        guard readScope() == .authenticated else { lock(); return }
        let reference = draft.credentialReference.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            if reference.isEmpty {
                let added = try credentials.addCredential(secret)
                addedReference = added
                draft.credentialReference = added
                credentialState = .stored
            } else {
                try credentials.updateCredential(secret, reference: reference)
                credentialState = .updated
            }
        } catch {
            credentialFailed(error)
        }
    }

    /// Removes the secret behind the draft's reference and forgets the reference. A stored profile version that still
    /// names it can no longer send, and its work waits.
    func removeCredential() {
        guard readScope() == .authenticated else { lock(); return }
        let reference = draft.credentialReference.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !reference.isEmpty else { credentialState = .failed(ProviderSettingsText.credentialMissingReference); return }
        do {
            try credentials.deleteCredential(reference: reference)
            if reference == addedReference { addedReference = nil }
            draft.credentialReference = ""
            credentialState = .removed
        } catch {
            credentialFailed(error)
        }
    }

    /// Reports whether the secret behind the draft's reference is present, absent or unusable. Never reads the secret.
    func checkCredential() {
        guard readScope() == .authenticated else { lock(); return }
        let reference = draft.credentialReference.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !reference.isEmpty else { credentialState = .failed(ProviderSettingsText.credentialMissingReference); return }
        do {
            credentialState = .status(try credentials.credentialStatus(reference: reference))
        } catch {
            credentialFailed(error)
        }
    }

    /// Reports that no secret was entered, without calling the credential service.
    func noteEmptySecret() {
        credentialState = .failed(ProviderSettingsText.credentialEmptySecret)
    }

    /// Reports an input that was refused as typed, for the draft or for a stored version, without calling the
    /// credential service. Nothing is stored and the input is not changed to make it acceptable.
    func noteRejectedSecret(_ rejection: ProviderSecretRejection, for version: ProviderProfileListAnswer.Version? = nil) {
        let state = ProviderSettingsCredentialState.failed(ProviderSettingsText.rejectedSecret(rejection))
        if let version { savedCredentialStates[version.profileVersion] = state } else { credentialState = state }
    }

    // MARK: credentials of stored profile versions

    /// Whether a secret action is running for `version`.
    func isChangingSavedCredential(_ version: ProviderProfileListAnswer.Version) -> Bool {
        savedCredentialStates[version.profileVersion] == .working
    }

    /// Replaces the secret bound to the stored `version` in place, so work pinned to it can send again. The services
    /// layer resolves the reference; this screen never sees it.
    func replaceSavedCredential(_ secret: Data, for version: ProviderProfileListAnswer.Version) async {
        await changeSavedCredential(version, success: .updated) { [credentials] profileVersion in
            try await credentials.replaceCredential(secret, profileVersion: profileVersion)
            return nil
        }
    }

    /// Reports whether the secret bound to the stored `version` is present, absent or unusable. Never reads it.
    func checkSavedCredential(_ version: ProviderProfileListAnswer.Version) async {
        await changeSavedCredential(version, success: nil) { [credentials] profileVersion in
            .status(try await credentials.credentialStatus(profileVersion: profileVersion))
        }
    }

    /// Removes the secret bound to the stored `version`. It stays saved but cannot send until a secret is entered.
    func removeSavedCredential(_ version: ProviderProfileListAnswer.Version) async {
        await changeSavedCredential(version, success: .removed) { [credentials] profileVersion in
            try await credentials.removeCredential(profileVersion: profileVersion)
            return nil
        }
    }

    private func changeSavedCredential(
        _ version: ProviderProfileListAnswer.Version, success: ProviderSettingsCredentialState?,
        _ operation: (String) async throws -> ProviderSettingsCredentialState?
    ) async {
        guard readScope() == .authenticated else { lock(); return }
        let profileVersion = version.profileVersion
        guard savedCredentialStates[profileVersion] != .working else { return }
        let thisSession = session
        savedCredentialStates[profileVersion] = .working
        do {
            let reported = try await operation(profileVersion)
            guard thisSession == session else { return }
            guard readScope() == .authenticated else { lock(); return }
            savedCredentialStates[profileVersion] = reported ?? success
        } catch {
            guard thisSession == session else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            savedCredentialStates[profileVersion] = .failed(ProviderSettingsText.credentialFailure(error))
        }
    }

    /// Deletes the secret this screen added if no saved version names it. Called when the draft is abandoned.
    private func discardAddedReference() {
        guard let reference = addedReference else { return }
        addedReference = nil
        do {
            try credentials.deleteCredential(reference: reference)
        } catch {
            referencesAwaitingDeletion.append(reference)
        }
    }

    private func sweepAbandonedReferences() {
        let waiting = referencesAwaitingDeletion
        referencesAwaitingDeletion = []
        for reference in waiting {
            do { try credentials.deleteCredential(reference: reference) }
            catch { referencesAwaitingDeletion.append(reference) }
        }
    }

    private func credentialFailed(_ error: Error) {
        if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
        credentialState = .failed(ProviderSettingsText.credentialFailure(error))
    }

    // MARK: connection check

    func isChecking(_ version: ProviderProfileListAnswer.Version) -> Bool {
        connectionStates[version.profileVersion] == .checking
    }

    /// Whether a check may be started for `version`: unrevoked and not already running.
    func canCheckConnection(_ version: ProviderProfileListAnswer.Version) -> Bool {
        !version.revoked && !isChecking(version)
    }

    /// Sends the core's one fixed synthetic request to `version`'s own destination, on the user's request only.
    func checkConnection(_ version: ProviderProfileListAnswer.Version) async {
        guard readScope() == .authenticated else { lock(); return }
        guard canCheckConnection(version) else { return }
        let thisSession = session
        let profileVersion = version.profileVersion
        connectionStates[profileVersion] = .checking
        do {
            let outcome = try await controls.checkConnection(profileVersion: profileVersion)
            guard thisSession == session else { return }
            guard readScope() == .authenticated else { lock(); return }
            connectionStates[profileVersion] = .finished(outcome)
        } catch {
            guard thisSession == session else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            connectionStates[profileVersion] = .failed(ProviderSettingsText.failure(error))
        }
    }

    /// Call when the screen goes away or the app locks: drops every stored detail and ignores answers still pending.
    func clear() {
        generation += 1
        session += 1
        listing = nil
        saveState = nil
        defaultState = nil
        credentialState = nil
        connectionStates = [:]
        savedCredentialStates = [:]
        discardAddedReference()
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
