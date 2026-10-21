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
    /// The existing route-policy read, used only to tell whether sending has been allowed under Privacy.
    func listRoutePolicy() async throws -> [RoutePolicyRoute]
}

extension ProviderConfigurationService: ProviderSettingsControlling {}

/// The credential operations this screen uses. `ProviderCredentialEntry` is the production implementation: it hands the
/// secret straight to the native credential service and returns only an opaque reference, a status or a redacted error.
/// No call returns a secret.
protocol ProviderSettingsCredentialControlling: AnyObject {
    /// Stores `secret` as a new credential, leased so the sweep leaves it alone until `releaseLease`.
    func addCredential(_ secret: Data) throws -> String
    func updateCredential(_ secret: Data, reference: String) throws
    func deleteCredential(reference: String) throws
    func credentialStatus(reference: String) throws -> CredentialStatus
    /// Ends the lease on a new credential: once a committed version holds it, or once nothing will and deleting it
    /// failed, so a later sweep can remove it.
    func releaseLease(reference: String)
    /// By stored profile version: the services layer resolves the version's reference itself and never returns it.
    func replaceCredential(_ secret: Data, profileVersion: String) async throws
    func credentialStatus(profileVersion: String) async throws -> CredentialStatus
    func removeCredential(profileVersion: String) async throws
}

extension ProviderCredentialEntry: ProviderSettingsCredentialControlling {}

/// The sweep of provider secrets nothing owns. `ProviderCredentialSweep` is the production implementation.
protocol ProviderSettingsCredentialSweeping: AnyObject {
    func run() async -> ProviderCredentialSweepOutcome
}

extension ProviderCredentialSweep: ProviderSettingsCredentialSweeping {}

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

    /// The types the Add provider form offers. A private server is offered only when `offersOwnServer` is true.
    static func offered(offersOwnServer: Bool) -> [ProviderSettingsProtocol] {
        allCases.filter { $0 != .selfHosted || offersOwnServer }
    }
}

/// The parts of the profile form that can show a problem of their own.
enum ProviderSettingsField: Equatable, Hashable {
    case profileName, model, endpoint, credential, timeout
}

extension ProviderSettingsIssue {
    var field: ProviderSettingsField {
        switch self {
        case .profileNameRequired, .profileNameTooLong: return .profileName
        case .modelRequired, .modelTooLong: return .model
        case .endpointRequired, .endpointNotHTTPS, .endpointHasQueryOrFragment, .endpointNotAllowed: return .endpoint
        case .credentialRequired, .credentialTooLong: return .credential
        case .timeoutOutOfBounds: return .timeout
        }
    }
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
    /// The person chooses a type deliberately: Anthropic is preselected, not the type that takes no address at all.
    var providerProtocol: ProviderSettingsProtocol = .anthropic {
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
/// * A secret stored for the draft is deleted at once, best effort, when the draft is abandoned. Whatever that misses
///   (a failed deletion, an ended process) is removed by the credential sweep, which runs each time the screen loads,
///   before a save is accepted. The credential service leases a new secret until a committed version holds it, so the
///   sweep never takes a secret still being saved for garbage. A secret is taken back from a save only when the save was
///   definitely refused; when its outcome is unknown the lease ends and the sweep keeps the secret if a version holds it.
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
    /// Whether the capture route allows sending to a cloud provider; nil when the policy could not be read.
    @Published private(set) var cloudSendingAllowed: Bool?
    /// The route new captures are pinned to. Only a grant on this route counts; nil means it cannot be established.
    private let captureRouteID: String?
    /// Set by the first Save attempt, after which every field's problem is shown.
    @Published private(set) var hasAttemptedSave = false
    /// The fields edited since the form was last empty; each shows its own problem once edited.
    @Published private(set) var editedFields: Set<ProviderSettingsField> = []
    @Published var draft = ProviderSettingsDraft() {
        didSet {
            if let added = addedReference, draft.credentialReference != added { discardAddedReference() }
            noteEdits(from: oldValue)
        }
    }

    /// The one switch for the private-server option (M1 F39 in docs/features/m1-task-refinement.md). It is off because
    /// this build composes no self-hosted capability and a saved profile mints a new version that nothing registers.
    /// Turning it on later is this one line; a self-hosted profile already saved is listed either way.
    static let ownServerOffered = false

    static let capability = "text_interpretation"
    static let defaultTimeoutSeconds = 60
    static let minimumTimeoutSeconds = 1
    static let maximumTimeoutSeconds = 300
    static let maximumProfileNameBytes = 128
    static let maximumModelBytes = 256
    static let maximumCredentialReferenceBytes = 256

    private let controls: ProviderSettingsControlling
    private let credentials: ProviderSettingsCredentialControlling
    private let sweep: ProviderSettingsCredentialSweeping
    private let readScope: () -> CompositionReadScope
    /// Whether "With my own server" and the "Private server" type are offered. See `ownServerOffered`.
    let offersOwnServer: Bool
    private var generation = 0
    private var session = 0
    /// The opaque reference of a secret this screen stored that no saved profile version names yet. It is deleted if
    /// the draft is abandoned. While a save that names it is waiting on the core it is not here, so leaving the screen
    /// cannot delete a secret the core is committing.
    private var addedReference: String?

    nonisolated init(
        controls: ProviderSettingsControlling, credentials: ProviderSettingsCredentialControlling,
        sweep: ProviderSettingsCredentialSweeping, readScope: @escaping () -> CompositionReadScope,
        offersOwnServer: Bool = ProviderSettingsModel.ownServerOffered, captureRouteID: String? = nil
    ) {
        self.captureRouteID = captureRouteID
        self.offersOwnServer = offersOwnServer
        self.controls = controls
        self.credentials = credentials
        self.sweep = sweep
        self.readScope = readScope
    }

    var isSaving: Bool { saveState == .working }
    var isChangingDefault: Bool { defaultState == .working }

    /// Everything wrong with the draft as it stands; empty means it can be saved.
    var issues: [ProviderSettingsIssue] { Self.issues(for: draft) }
    /// Save stays pressable on an incomplete form: the first press is what shows each field's problem.
    var canSave: Bool { !isSaving && phase == .loaded }

    /// What is wrong with `field`, shown only after the first Save attempt or once that field has been edited.
    func visibleIssues(for field: ProviderSettingsField) -> [ProviderSettingsIssue] {
        guard hasAttemptedSave || editedFields.contains(field) else { return [] }
        return issues.filter { $0.field == field }
    }

    private func noteEdits(from previous: ProviderSettingsDraft) {
        if draft.profileName != previous.profileName { editedFields.insert(.profileName) }
        if draft.model != previous.model { editedFields.insert(.model) }
        if draft.endpoint != previous.endpoint { editedFields.insert(.endpoint) }
        if draft.credentialReference != previous.credentialReference { editedFields.insert(.credential) }
        if draft.timeoutSeconds != previous.timeoutSeconds { editedFields.insert(.timeout) }
    }

    /// Empties the form and forgets what was edited or attempted, so the next form starts untouched.
    func resetDraft() {
        draft = ProviderSettingsDraft()
        editedFields = []
        hasAttemptedSave = false
    }

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
            && !isUnofferedOwnServer(version) && defaultVersion?.profileVersion != version.profileVersion
    }

    /// Whether `version` is a private-server profile this build does not offer: still listed, never the default.
    func isUnofferedOwnServer(_ version: ProviderProfileListAnswer.Version) -> Bool {
        version.providerProtocol == "self_hosted" && !offersOwnServer
    }

    /// Whether the default is a cloud provider whose destination the person's captures are not yet allowed to reach,
    /// so new notes are still handled on the phone only. Only an exact positive on the capture route clears it: an
    /// unreadable policy or an unknown route keeps the prompt. The existing read lists destination classes, not origins,
    /// so a cloud grant made for another provider cannot be told apart from one for this default.
    var needsPrivacyAllowance: Bool {
        guard case .hosted = operatingMode else { return false }
        return cloudSendingAllowed != true
    }

    /// Reads the stored profiles and defaults again. When the screen loads, it first sweeps secrets nothing owns.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        generation += 1
        let thisGeneration = generation
        if listing == nil {
            phase = .loading
            _ = await sweep.run()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
        }
        do {
            let fresh = try await controls.listProfiles()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            listing = fresh
            phase = .loaded
            await refreshSendingAllowance(generation: thisGeneration)
        } catch {
            guard thisGeneration == generation else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            guard readScope() == .authenticated else { lock(); return }
            phase = .failed(ProviderSettingsText.failure(error))
        }
    }

    /// Reads the route policy again. A failed read leaves the answer unknown, which shows no prompt.
    private func refreshSendingAllowance(generation thisGeneration: Int) async {
        let routes = try? await controls.listRoutePolicy()
        guard thisGeneration == generation, readScope() == .authenticated else { return }
        cloudSendingAllowed = routes.map { routes in
            routes.contains { route in
                route.routeID == captureRouteID
                    && route.grantedCapabilities.contains {
                        $0.capability == Self.capability && $0.destinationClasses.contains("cloud")
                    }
            }
        }
    }

    /// Stores the draft as a new immutable profile version. It does not become the default. Accepted only once the
    /// screen has loaded, so the sweep has run first.
    func save() async {
        guard readScope() == .authenticated else { lock(); return }
        guard !isSaving, phase == .loaded else { return }
        guard issues.isEmpty else {
            hasAttemptedSave = true
            saveState = .failed(ProviderSettingsText.formNeedsAttention)
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
        // The saved version may come to name the secret this screen added, so until the core answers the reference is
        // not the screen's to delete. A definite refusal gives it back below; an unknown outcome leaves it to the sweep.
        let namedReference = addedReference.flatMap { $0 == request.credentialReference ? $0 : nil }
        if namedReference != nil { addedReference = nil }
        do {
            _ = try await controls.createProfile(request)
            // Committed: a stored version holds the reference, whatever happened to the screen meanwhile.
            credentials.releaseLease(reference: request.credentialReference)
            guard mayPublish(from: thisSession) else { return }
            saveState = .saved
            credentialState = nil
            resetDraft()
            await refresh()
        } catch {
            if let namedReference {
                if ProviderConfigurationError.isDefiniteRefusal(error) {
                    takeBackUnsavedReference(namedReference, sameSession: thisSession == session)
                } else {
                    leaveUnknownReferenceToTheSweep(namedReference, sameSession: thisSession == session)
                }
            }
            guard mayPublish(from: thisSession, after: error) else { return }
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
            guard mayPublish(from: thisSession) else { return }
            savedCredentialStates[profileVersion] = reported ?? success
        } catch {
            guard mayPublish(from: thisSession, after: error) else { return }
            savedCredentialStates[profileVersion] = .failed(ProviderSettingsText.credentialFailure(error))
        }
    }

    /// Deletes the secret this screen added, which no saved version names. Called when the draft is abandoned.
    private func discardAddedReference() {
        guard let reference = addedReference else { return }
        addedReference = nil
        deleteUnsaved(reference)
    }

    /// Deletes a secret nothing will save. If that fails (for example the session has locked) its lease ends instead,
    /// so the next sweep removes it.
    private func deleteUnsaved(_ reference: String) {
        do {
            try credentials.deleteCredential(reference: reference)
        } catch {
            credentials.releaseLease(reference: reference)
        }
    }

    /// A save the core refused, or that never reached it, names no version: the secret is unsaved again. It returns to
    /// the draft if the draft is still there, otherwise it is deleted.
    private func takeBackUnsavedReference(_ reference: String, sameSession: Bool) {
        if sameSession && draft.credentialReference == reference && addedReference == nil {
            addedReference = reference
        } else {
            deleteUnsaved(reference)
        }
    }

    /// A save whose outcome is unknown (the client shut down, or the answer could not be read) may have committed a
    /// version that names the secret, so it is neither taken back nor deleted. Its lease ends and the draft stops naming
    /// it; the next sweep deletes it only if no stored version holds it.
    private func leaveUnknownReferenceToTheSweep(_ reference: String, sameSession: Bool) {
        credentials.releaseLease(reference: reference)
        guard sameSession, draft.credentialReference == reference else { return }
        draft.credentialReference = ""
        credentialState = nil
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
            guard mayPublish(from: thisSession) else { return }
            connectionStates[profileVersion] = .finished(outcome)
        } catch {
            guard mayPublish(from: thisSession, after: error) else { return }
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
        resetDraft()
        cloudSendingAllowed = nil
        phase = .idle
    }

    private func changeDefault(_ operation: () async throws -> Void) async {
        let thisSession = session
        defaultState = .working
        do {
            try await operation()
            guard mayPublish(from: thisSession) else { return }
            defaultState = .defaultChanged
        } catch {
            guard mayPublish(from: thisSession, after: error) else { return }
            defaultState = .failed(ProviderSettingsText.failure(error))
        }
        await refresh()
    }

    /// Whether an answer that has just arrived may be shown: the session that asked is still on screen and still
    /// authenticated. A relock since the call empties the screen instead, whatever the answer was.
    private func mayPublish(from thisSession: Int) -> Bool {
        guard thisSession == session else { return false }
        guard readScope() == .authenticated else { lock(); return false }
        return true
    }

    /// Whether a failure that has just arrived may be shown. Besides the checks above, a lapse the service itself
    /// reported locks the screen; any other failure is shown only if the session is still authenticated now.
    private func mayPublish(from thisSession: Int, after error: Error) -> Bool {
        guard thisSession == session else { return false }
        if case ProviderConfigurationError.notAuthenticated = error { lock(); return false }
        return mayPublish(from: thisSession)
    }

    private func lock() {
        clear()
        phase = .locked
    }
}
