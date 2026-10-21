import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Stands in for the network the way the production transport does for credentials: a request naming a secret that is
/// absent from the Keychain fails as an unavailable credential; otherwise it answers with a canned status, or fails as
/// an unreachable destination when no status is set. No request ever leaves the test.
private final class CheckingProviderSettingsSender: ProviderRequestSender, @unchecked Sendable {
    private let credentials: CredentialService
    private let lock = NSLock()
    private var cannedStatus: Int?
    private var sent = 0

    init(credentials: CredentialService) { self.credentials = credentials }

    var sentCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return sent
    }

    func respond(withStatus status: Int?) {
        lock.lock()
        cannedStatus = status
        lock.unlock()
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        sent += 1
        let status = cannedStatus
        lock.unlock()
        if let reference = request.credential?.reference,
            (try? credentials.credentialStatus(reference: reference)) != .present
        {
            throw ProviderTransportError.credentialUnavailable
        }
        if let status { return ProviderHTTPResponse(status: status, headers: [:], body: Data()) }
        throw ProviderTransportError.connectionFailed
    }
}

private struct ProviderSettingsInjected: @unchecked Sendable {
    let sender: CheckingProviderSettingsSender
    let credentials: CredentialService
}

private struct ProviderSettingsBuiltRoot: @unchecked Sendable {
    let root: ServiceCompositionRoot
    let coordinator: LifecycleCoordinator
}

private final class SaveOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: CompositionSaveOutcome?

    var value: CompositionSaveOutcome? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ outcome: CompositionSaveOutcome) {
        lock.lock()
        stored = outcome
        lock.unlock()
    }
}

/// The real profile service, except that a save ends the way the client reports a lost answer: after the core committed
/// it (`unreadable_event`), or without the core having run it (`client_shut_down`).
private final class LostSaveAnswerControls: ProviderSettingsControlling, @unchecked Sendable {
    private let service: ProviderConfigurationService
    private let coreCommits: Bool

    init(service: ProviderConfigurationService, coreCommits: Bool) {
        self.service = service
        self.coreCommits = coreCommits
    }

    func createProfile(_ draft: ProviderProfileDraft) async throws -> ProviderProfileCreatedAnswer {
        if coreCommits {
            _ = try await service.createProfile(draft)
            throw ProviderConfigurationError.core(ProviderConfigurationClientFailure.unreadableAnswer)
        }
        throw ProviderConfigurationError.core(ProviderConfigurationClientFailure.shutDown)
    }

    func setDefault(capability: String, profileVersion: String) async throws {
        try await service.setDefault(capability: capability, profileVersion: profileVersion)
    }

    func clearDefault(capability: String) async throws { try await service.clearDefault(capability: capability) }

    func listProfiles() async throws -> ProviderProfileListAnswer { try await service.listProfiles() }

    func listRoutePolicy() async throws -> [RoutePolicyRoute] { try await service.listRoutePolicy() }

    func checkConnection(profileVersion: String) async throws -> ProviderConnectionOutcome {
        try await service.checkConnection(profileVersion: profileVersion)
    }
}

/// Drives `ProviderSettingsModel` over the real composition root and Rust core on an on-disk store: the core's own
/// validation, versioning and default pointer.
@MainActor
final class ProviderSettingsRealCoreTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreRetentionWorld!
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var coordinator: LifecycleCoordinator!
    private var keychain: FakeKeychain!
    private var sender: CheckingProviderSettingsSender!
    private var credentialService: CredentialService!

    override func setUpWithError() throws {
        try super.setUpWithError()
        keychain = FakeKeychain()
        credentialService = CredentialService(
            serviceName: "com.boldfield.ohand.tests.provider-settings.\(UUID().uuidString)", keychain: keychain)
        sender = CheckingProviderSettingsSender(credentials: credentialService)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreRetentionWorld()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    override func tearDown() {
        root?.shutDown()
        root = nil
        world.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        super.tearDown()
    }

    /// Builds the root off the main thread, as the other real-core suites do: its route provisioning blocks until the
    /// core answers.
    private func makeModel() async throws -> ProviderSettingsModel {
        let layoutPath = world.layout.rootDirectory.path
        let readSession = session!
        let injected = ProviderSettingsInjected(sender: sender, credentials: credentialService)
        let built = try await Task.detached { () -> ProviderSettingsBuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: injected.sender, readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                    credentials: injected.credentials),
                modules: [coordinator])
            try coordinator.connect(to: root)
            return ProviderSettingsBuiltRoot(root: root, coordinator: coordinator)
        }.value
        root = built.root
        coordinator = built.coordinator
        return ProviderSettingsModel.assembled(root: root, offersOwnServer: true)
    }

    /// The one private-server model the core declares supported; any other model name is declared unverified.
    private static let verifiedPrivateModel = "deepseek-flash-iq3:latest"

    private func fill(
        _ model: ProviderSettingsModel, name: String, providerProtocol: ProviderSettingsProtocol,
        modelName: String = "synthetic-model"
    ) {
        model.draft = ProviderSettingsDraft(
            profileName: name, providerProtocol: providerProtocol,
            endpoint: providerProtocol == .selfHosted ? "https://private.example/v1" : "",
            model: modelName, credentialReference: "cred-\(name)", timeoutSeconds: 30)
    }

    private func version(
        _ model: ProviderSettingsModel, profile: String
    ) throws -> ProviderProfileListAnswer.Version {
        try XCTUnwrap(model.listing?.profiles.first { $0.profileID == profile }?.versions.last)
    }

    private func execute(_ statement: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(world.databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, statement, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(world.databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    private func saveCapture() async throws -> String {
        let box = SaveOutcomeBox()
        let savedRoot = root!
        savedRoot.saveText(captureID: UUID().uuidString.lowercased(), text: "Synthetic capture text", capturedAt: Date()) {
            box.set($0)
        }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil && Date() < deadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let outcome = box.value
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    func testProfilesAreStoredAsVersionsAndTheDefaultFollowsTheUsersChoice() async throws {
        let model = try await makeModel()
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.operatingMode, .localOnly)

        fill(model, name: "hosted", providerProtocol: .openAI)
        await model.save()
        XCTAssertEqual(model.saveState, .saved)
        XCTAssertEqual(model.operatingMode, .localOnly, "saving a version does not select it")

        fill(model, name: "private", providerProtocol: .selfHosted, modelName: Self.verifiedPrivateModel)
        await model.save()
        XCTAssertEqual(model.saveState, .saved)

        let versions = model.listing!.profiles.flatMap(\.versions)
        XCTAssertEqual(versions.count, 2)
        let hosted = try XCTUnwrap(versions.first { $0.providerProtocol == "open_ai" })
        let privateServer = try XCTUnwrap(versions.first { $0.providerProtocol == "self_hosted" })

        await model.makeDefault(hosted)
        XCTAssertEqual(model.defaultState, .defaultChanged)
        XCTAssertEqual(model.defaultVersion?.profileVersion, hosted.profileVersion)

        await model.makeDefault(privateServer)
        XCTAssertEqual(model.operatingMode, .privateServer(model: Self.verifiedPrivateModel))
        XCTAssertEqual(model.defaultAvailability, .ready)
        XCTAssertTrue(
            model.listing!.profiles.flatMap(\.versions).contains { $0.profileVersion == hosted.profileVersion },
            "the earlier version is still stored, so work pinned to it stays resolvable")

        await model.useLocalOnly()
        XCTAssertEqual(model.operatingMode, .localOnly)
    }

    func testAConnectionCheckThroughTheRealCoreEndsInAFixedRedactedOutcomeAndNeverSucceedsOverAnUnreachableSender() async throws {
        let model = try await makeModel()
        await model.refresh()
        fill(model, name: "hosted", providerProtocol: .openAI)
        model.draft.credentialReference = UUID().uuidString
        await model.save()
        XCTAssertEqual(model.saveState, .saved)

        let hosted = try version(model, profile: "hosted")
        XCTAssertNil(model.connectionStates[hosted.profileVersion], "saving a profile checks nothing")
        XCTAssertEqual(sender.sentCount, 0, "nothing was sent before the user asked")
        await model.checkConnection(hosted)

        guard case .finished(let outcome)? = model.connectionStates[hosted.profileVersion] else {
            return XCTFail("the check finished with a core outcome: \(String(describing: model.connectionStates))")
        }
        XCTAssertEqual(outcome, .credentialMissing, "the profile names a secret that was never stored")
        XCTAssertEqual(model.connectionStates[hosted.profileVersion]?.message, ProviderSettingsText.connectionOutcome(outcome))
        XCTAssertFalse(model.connectionStates[hosted.profileVersion]!.message.contains("example"))
    }

    private static let syntheticSecret = Data("synthetic-secret-value".utf8)

    /// Stores a secret through the model, saves a hosted profile that names it and returns the stored version.
    private func savedProfileWithSecret(_ model: ProviderSettingsModel) async throws -> ProviderProfileListAnswer.Version {
        await model.refresh()
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(model.credentialState, .stored)
        model.draft.profileName = "hosted"
        model.draft.model = "synthetic-model"
        await model.save()
        XCTAssertEqual(model.saveState, .saved)
        XCTAssertEqual(keychain.items.count, 1)
        return try version(model, profile: "hosted")
    }

    func testAStoredSecretIsCheckedThroughTheRealCoreAndReachesOnlyTheUnreachableNetworkPath() async throws {
        let model = try await makeModel()
        let hosted = try await savedProfileWithSecret(model)

        await model.checkConnection(hosted)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion], .finished(.unreachable))
        XCTAssertEqual(sender.sentCount, 1, "exactly the one requested check was sent")

        sender.respond(withStatus: 401)
        await model.checkConnection(hosted)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion], .finished(.credentialRejected))

        sender.respond(withStatus: 404)
        await model.checkConnection(hosted)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion], .finished(.capabilityMismatch))
        for state in model.connectionStates.values {
            XCTAssertFalse(state.message.contains("synthetic-secret-value"))
        }
    }

    func testASavedProfilesSecretIsRemovedReportedMissingAndReplacedAfterTheDraftWasReset() async throws {
        let model = try await makeModel()
        let hosted = try await savedProfileWithSecret(model)
        XCTAssertEqual(model.draft.credentialReference, "", "the draft was reset by the save")

        await model.checkSavedCredential(hosted)
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .status(.present))

        await model.removeSavedCredential(hosted)
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .removed)
        XCTAssertTrue(keychain.items.isEmpty, "the saved profile's own secret was removed")
        await model.checkSavedCredential(hosted)
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .status(.absent))

        await model.checkConnection(hosted)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion], .finished(.credentialMissing))

        await model.replaceSavedCredential(Data("synthetic-replacement-value".utf8), for: hosted)
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .updated)
        XCTAssertEqual(keychain.items.count, 1, "the same reference was re-bound; no second credential exists")
        await model.checkConnection(hosted)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion], .finished(.unreachable))

        await model.refresh()
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .updated)
    }

    func testAnInvalidReferenceAndAnUnknownVersionAreReportedRedactedByTheRealServices() async throws {
        let model = try await makeModel()
        await model.refresh()

        model.draft.credentialReference = "not-a-valid-reference"
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(model.credentialState, .failed(ProviderSettingsText.credentialFailure(CredentialError.invalidReference)))
        XCTAssertTrue(keychain.items.isEmpty)
        XCTAssertFalse(model.credentialState!.message.contains("not-a-valid-reference"))

        let unknown = providerListing(
            versions: [("ghost", "ghost-v1", "open_ai", false, true)], defaultVersion: nil
        ).profiles[0].versions[0]
        await model.replaceSavedCredential(Self.syntheticSecret, for: unknown)
        guard case .failed(let message)? = model.savedCredentialStates["ghost-v1"] else {
            return XCTFail("the core refuses a version it does not store")
        }
        XCTAssertFalse(message.contains("ghost-v1"))
        XCTAssertTrue(keychain.items.isEmpty, "nothing was written for an unknown version")
    }

    func testLeavingTheScreenDeletesASecretNoSavedProfileNames() async throws {
        let model = try await makeModel()
        await model.refresh()
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(keychain.items.count, 1)

        model.clear()
        XCTAssertTrue(keychain.items.isEmpty, "the unsaved secret was removed from the Keychain")
    }

    func testLoadingTheScreenSweepsASecretAnEarlierRunLeftAndKeepsTheSavedOne() async throws {
        let model = try await makeModel()
        let hosted = try await savedProfileWithSecret(model)
        // What an earlier process left: a secret stored for a draft that never saved, its lease gone with the process.
        let orphan = try credentialService.addCredential(Self.syntheticSecret)
        credentialService.releaseLease(reference: orphan)
        XCTAssertEqual(keychain.items.count, 2)

        model.clear()
        await model.refresh()

        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(keychain.items.count, 1, "the unowned secret was swept")
        XCTAssertFalse(keychain.items.keys.contains { $0.account == orphan })
        await model.checkSavedCredential(hosted)
        XCTAssertEqual(model.savedCredentialStates[hosted.profileVersion], .status(.present), "the saved one stays")
    }

    private func modelLosingTheSaveAnswer(coreCommits: Bool) async throws -> ProviderSettingsModel {
        _ = try await makeModel()
        let liveRoot = root!
        return ProviderSettingsModel(
            controls: LostSaveAnswerControls(service: liveRoot.providerConfiguration, coreCommits: coreCommits),
            credentials: liveRoot.providerCredentials, sweep: liveRoot.providerCredentialSweep,
            readScope: { [weak liveRoot] in liveRoot?.readScope ?? .captureOnly }, offersOwnServer: true)
    }

    private func saveWithLostAnswer(_ model: ProviderSettingsModel) async {
        await model.refresh()
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "lost"
        model.draft.model = "synthetic-model"
        await model.save()
        XCTAssertNotEqual(model.saveState, .saved)
        XCTAssertEqual(keychain.items.count, 1, "an unknown outcome deletes nothing at once")
        XCTAssertEqual(model.draft.credentialReference, "")
        model.clear()
        XCTAssertEqual(keychain.items.count, 1, "nor does leaving the screen")
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
    }

    func testASaveCommittedByTheCoreWhoseAnswerWasLostKeepsItsSecretThroughTheSweep() async throws {
        let model = try await modelLosingTheSaveAnswer(coreCommits: true)
        await saveWithLostAnswer(model)

        XCTAssertEqual(keychain.items.count, 1, "the sweep keeps the secret the committed version holds")
        let lost = try version(model, profile: "lost")
        await model.checkSavedCredential(lost)
        XCTAssertEqual(model.savedCredentialStates[lost.profileVersion], .status(.present))
    }

    func testASaveTheCoreNeverRanIsSweptOnTheNextLoad() async throws {
        let model = try await modelLosingTheSaveAnswer(coreCommits: false)
        await saveWithLostAnswer(model)

        XCTAssertTrue(keychain.items.isEmpty, "no version holds the secret, so the sweep deleted it")
        XCTAssertNil(model.listing?.profiles.first { $0.profileID == "lost" })
    }

    func testAnUnverifiedPrivateServerIsDeclaredUnavailableByTheCoreAndRefusedAsDefault() async throws {
        let model = try await makeModel()
        await model.refresh()
        fill(model, name: "unverified", providerProtocol: .selfHosted, modelName: "synthetic-model")
        await model.save()
        XCTAssertEqual(model.saveState, .saved)

        let stored = try version(model, profile: "unverified")
        XCTAssertEqual(
            stored.capabilities.first { $0.capability == "text_interpretation" }?.support, "unverified")
        XCTAssertEqual(model.availability(of: stored), .unverified)
        XCTAssertFalse(model.canMakeDefault(stored))

        await model.makeDefault(stored)
        XCTAssertEqual(model.operatingMode, .localOnly, "the unverified version did not become the default")
        XCTAssertNotNil(model.defaultState?.message)
        XCTAssertNotEqual(model.defaultState, .defaultChanged)
    }

    func testTheCoreRefusesADraftTheFormAccepted() async throws {
        let model = try await makeModel()
        await model.refresh()
        fill(model, name: "hosted", providerProtocol: .openAI, modelName: "bad\nmodel")
        XCTAssertTrue(model.issues.isEmpty, "the form cannot tell that the core rejects a control character")
        XCTAssertTrue(model.canSave)

        await model.save()

        XCTAssertEqual(
            model.saveState,
            .failed(
                ProviderSettingsText.failure(
                    ProviderConfigurationError.core(
                        CoreFailure(errorClass: .permanent, code: "model_invalid", message: "")))))
        XCTAssertNotEqual(model.saveState, .saved)
        XCTAssertTrue(model.listing?.profiles.isEmpty ?? true, "nothing was stored")
        XCTAssertEqual(model.draft.model, "bad\nmodel", "the form is kept so it can be corrected")
    }

    func testChangingTheDefaultLeavesCapturesAndPinnedQueuedJobsOnTheirOriginalVersion() async throws {
        let model = try await makeModel()
        await model.refresh()
        fill(model, name: "first", providerProtocol: .openAI)
        await model.save()
        fill(model, name: "second", providerProtocol: .anthropic)
        await model.save()
        let first = try version(model, profile: "first")
        let second = try version(model, profile: "second")
        await model.makeDefault(first)
        XCTAssertEqual(model.defaultVersion?.profileVersion, first.profileVersion)

        let itemID = try await saveCapture()
        let textBefore = scalar("SELECT COUNT(*) FROM items WHERE item_id = '\(itemID)'")
        XCTAssertEqual(textBefore, "1")
        execute(
            """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES ('job-pinned-settings', 1, '\(itemID)', \
            'interpret', 0, '\(first.profileVersion)', '5a1c0000-0000-4000-8000-0000000000a3', 'queued', 0, '2026-10-08T09:30:02Z')
            """)
        let jobs = "SELECT profile_version FROM jobs WHERE job_id = 'job-pinned-settings'"
        let pinned = first.profileVersion
        XCTAssertEqual(scalar(jobs), pinned)
        let jobCount = scalar("SELECT COUNT(*) FROM jobs")
        let itemCount = scalar("SELECT COUNT(*) FROM items")

        await model.makeDefault(second)
        XCTAssertEqual(model.defaultVersion?.profileVersion, second.profileVersion)
        XCTAssertEqual(scalar(jobs), pinned, "changing the default does not re-pin queued work")

        await model.useLocalOnly()
        XCTAssertEqual(model.operatingMode, .localOnly)
        XCTAssertEqual(scalar(jobs), pinned, "going local-only does not touch queued work")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM jobs"), jobCount)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM items"), itemCount)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM items WHERE item_id = '\(itemID)'"), "1")
    }

    func testARelockedSessionShowsNothingFromTheCore() async throws {
        let model = try await makeModel()
        await model.refresh()
        XCTAssertNotNil(model.listing)
        session.relock(.explicit)
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.listing)
    }
}
