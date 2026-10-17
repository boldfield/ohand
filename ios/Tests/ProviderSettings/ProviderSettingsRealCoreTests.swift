import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class UnreachableProviderSettingsSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
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

    override func setUpWithError() throws {
        try super.setUpWithError()
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
        let built = try await Task.detached { () -> ProviderSettingsBuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: UnreachableProviderSettingsSender(), readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)),
                modules: [coordinator])
            try coordinator.connect(to: root)
            return ProviderSettingsBuiltRoot(root: root, coordinator: coordinator)
        }.value
        root = built.root
        coordinator = built.coordinator
        return ProviderSettingsModel.assembled(root: root)
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
        await model.checkConnection(hosted)

        guard case .finished(let outcome)? = model.connectionStates[hosted.profileVersion] else {
            return XCTFail("the check finished with a core outcome: \(String(describing: model.connectionStates))")
        }
        XCTAssertNotEqual(outcome, .ok)
        XCTAssertEqual(model.connectionStates[hosted.profileVersion]?.message, ProviderSettingsText.connectionOutcome(outcome))
        XCTAssertFalse(model.connectionStates[hosted.profileVersion]!.message.contains("example"))
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
