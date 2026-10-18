import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private struct PrivacyBuiltRoot: @unchecked Sendable {
    let root: ServiceCompositionRoot
    let coordinator: LifecycleCoordinator
    let search: RetrievalSearchModule
}

private final class PrivacySearchOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<RetrievalPage, RetrievalSearchFailure>?

    var value: Result<RetrievalPage, RetrievalSearchFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

private final class PrivacySaveOutcomeBox: @unchecked Sendable {
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

/// Drives `PrivacySettingsModel` over the real composition root and Rust core on an on-disk store: the core's own route
/// provisioning, grant derivation and policy listing.
@MainActor
final class PrivacySettingsRealCoreTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreRetentionWorld!
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var coordinator: LifecycleCoordinator!
    private var keychain: FakeKeychain!
    private var credentialService: CredentialService!
    private var sender: PrivacyUnreachableSender!
    private var searchModule: RetrievalSearchModule!

    override func setUpWithError() throws {
        try super.setUpWithError()
        keychain = FakeKeychain()
        sender = PrivacyUnreachableSender()
        credentialService = CredentialService(
            serviceName: "com.boldfield.ohand.tests.privacy-settings.\(UUID().uuidString)", keychain: keychain)
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
    private func makeModel() async throws -> PrivacySettingsModel {
        let layoutPath = world.layout.rootDirectory.path
        let readSession = session!
        let credentials = credentialService!
        let requestSender = sender!
        let built = try await Task.detached { () -> PrivacyBuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = TimeZone(identifier: "UTC")!
            let search = RetrievalSearchModule(calendar: calendar)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: requestSender, readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                    credentials: credentials),
                modules: [coordinator, search])
            try coordinator.connect(to: root)
            return PrivacyBuiltRoot(root: root, coordinator: coordinator, search: search)
        }.value
        root = built.root
        coordinator = built.coordinator
        searchModule = built.search
        return PrivacySettingsModel.assembled(root: root)
    }

    private func execute(_ statement: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(world.databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, statement, nil, nil, &message)
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK)
    }

    /// Closes the root and opens a new one over the same on-disk store, as a relaunch does.
    private func relaunch() async throws -> PrivacySettingsModel {
        root.shutDown()
        root = nil
        return try await makeModel()
    }

    private func search(_ query: String) async throws -> RetrievalPage {
        let box = PrivacySearchOutcomeBox()
        searchModule.search(RetrievalSearchRequest(query: query)) { box.set($0) }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        return try XCTUnwrap(box.value, "the search completed").get()
    }

    /// Gives job processing time to run on a saved capture, so a test can show it authorized and sent nothing.
    private func letProcessingRun() async throws {
        try await Task.sleep(nanoseconds: 2_000_000_000)
    }

    private func saveCapture(_ text: String = "Synthetic capture text") async throws {
        let box = PrivacySaveOutcomeBox()
        root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) {
            box.set($0)
        }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        guard case .saved? = box.value else {
            XCTFail("expected a confirmed save, got \(String(describing: box.value))")
            return
        }
    }

    private func createProfile(_ name: String, providerProtocol: String, endpoint: String? = nil) async throws -> String {
        let created = try await root.providerConfiguration.createProfile(
            ProviderProfileDraft(
                profileID: name, providerProtocol: providerProtocol, endpoint: endpoint, model: "synthetic-model",
                credentialReference: "cred-\(name)", timeoutSeconds: 30))
        return created.profileVersion
    }

    func testFreshInstallCapturesBeforeAnyProviderSetupAndEveryRouteIsLocalOnly() async throws {
        let model = try await makeModel()
        try await saveCapture("synthetic roofer words")
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly)
        XCTAssertEqual(model.routes.map(\.routeID), ["route-personal"])
        XCTAssertTrue(model.offeredVersions(for: "text_interpretation").isEmpty)

        let page = try await search("roofer")
        XCTAssertEqual(page.totalAccessible, 1, "the capture is retrieved with no provider profile saved")
        XCTAssertEqual(page.rows.first?.text, "synthetic roofer words")
        XCTAssertEqual(sender.attemptCount, 0, "capture and search asked no provider anything")
        await model.refresh()
        XCTAssertTrue(model.isLocalOnly, "capture and search authorized nothing")
    }

    func testAConfirmedGrantAndTheRouteLabelsAreStickyAcrossARelaunch() async throws {
        var model = try await makeModel()
        let version = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        let before = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        guard case .allowed = model.actionState else { return XCTFail("expected allowed") }

        model = try await relaunch()
        await model.refresh()

        XCTAssertEqual(model.phase, .loaded)
        let after = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(after.routeName, before.routeName)
        XCTAssertEqual(after.scope, before.scope)
        XCTAssertEqual(after.grantedCapabilities.map(\.capability), ["text_interpretation"])
        XCTAssertEqual(after.grantedCapabilities.map(\.destinationClasses), [["cloud"]])
    }

    func testProcessingACaptureCreatesNoGrantAndSendsNothingWithoutOne() async throws {
        let model = try await makeModel()
        _ = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        let before = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertTrue(before.grantedCapabilities.isEmpty)
        XCTAssertEqual(model.offeredVersions(for: "text_interpretation").count, 1, "a usable profile exists")

        try await saveCapture("remind me to call the roofer tomorrow")
        try await letProcessingRun()
        await model.refresh()

        XCTAssertTrue(model.isLocalOnly, "processing a capture created no grant")
        XCTAssertEqual(sender.attemptCount, 0, "no request reached a provider without a grant")
    }

    func testAGrantOnOneCapabilityLeavesTheOtherUngrantedAfterProcessing() async throws {
        let model = try await makeModel()
        let version = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))

        try await saveCapture("remind me to call the roofer tomorrow")
        try await letProcessingRun()
        await model.refresh()

        let personal = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(personal.grantedCapabilities.map(\.capability), ["text_interpretation"])
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "review"))
    }

    func testCancellingSetupLeavesPolicyUnchangedAndCaptureUsable() async throws {
        let model = try await makeModel()
        let version = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        XCTAssertNotNil(model.proposal)
        model.cancel()
        await model.refresh()
        XCTAssertTrue(model.isLocalOnly, "the core holds no grant after a cancelled setup")
        try await saveCapture()
    }

    func testAcceptingOneRouteAndDestinationAuthorizesNoOther() async throws {
        let model = try await makeModel()
        execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-work', 'Work notes', 'work', '[]', '2026-10-01T07:00:00Z')")
        let hosted = try await createProfile("hosted", providerProtocol: "open_ai")
        let privateServer = try await createProfile(
            "private", providerProtocol: "self_hosted", endpoint: "https://private.example/v1")
        await model.refresh()
        XCTAssertEqual(Set(model.routes.map(\.routeID)), ["route-personal", "route-work"])

        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: hosted)
        await model.confirm(try XCTUnwrap(model.proposal))
        guard case .allowed = model.actionState else { return XCTFail("expected allowed, got \(String(describing: model.actionState))") }

        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-work", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "review"))
        let personal = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(personal.grantedCapabilities.map(\.destinationClasses), [["cloud"]])

        // The private server version has no grant of its own: proposing and cancelling it changes nothing.
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: privateServer)
        XCTAssertNotNil(model.proposal)
        model.cancel()
        await model.refresh()
        let afterCancel = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(afterCancel.grantedCapabilities.map(\.destinationClasses), [["cloud"]])
        let work = try XCTUnwrap(model.routes.first { $0.routeID == "route-work" })
        XCTAssertTrue(work.grantedCapabilities.isEmpty)
    }
}

/// No request ever leaves the test.
private final class PrivacyUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var attempts = 0

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        attempts += 1
        lock.unlock()
        throw ProviderTransportError.connectionFailed
    }
}
