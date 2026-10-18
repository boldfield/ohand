import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private struct PrivacyBuiltRoot: @unchecked Sendable {
    let root: ServiceCompositionRoot
    let coordinator: LifecycleCoordinator
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

    override func setUpWithError() throws {
        try super.setUpWithError()
        keychain = FakeKeychain()
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
        let built = try await Task.detached { () -> PrivacyBuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: PrivacyUnreachableSender(), readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                    credentials: credentials),
                modules: [coordinator])
            try coordinator.connect(to: root)
            return PrivacyBuiltRoot(root: root, coordinator: coordinator)
        }.value
        root = built.root
        coordinator = built.coordinator
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

    private func saveCapture() async throws {
        let box = PrivacySaveOutcomeBox()
        root.saveText(captureID: UUID().uuidString.lowercased(), text: "Synthetic capture text", capturedAt: Date()) {
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
        try await saveCapture()
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly)
        XCTAssertEqual(model.routes.map(\.routeID), ["route-personal"])
        XCTAssertTrue(model.offeredVersions(for: "text_interpretation").isEmpty)
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
        XCTAssertNotEqual(personal.grantedCapabilities.first?.destinationClasses, ["private_server"])
        _ = privateServer
    }
}

/// No request ever leaves the test.
private final class PrivacyUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw ProviderTransportError.connectionFailed
    }
}
