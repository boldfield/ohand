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

    private func fill(_ model: ProviderSettingsModel, name: String, providerProtocol: ProviderSettingsProtocol) {
        model.draft = ProviderSettingsDraft(
            profileName: name, providerProtocol: providerProtocol,
            endpoint: providerProtocol == .selfHosted ? "https://private.example/v1" : "",
            model: "synthetic-model", credentialReference: "cred-\(name)", timeoutSeconds: 30)
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

        fill(model, name: "private", providerProtocol: .selfHosted)
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
        XCTAssertEqual(model.operatingMode, .privateServer(model: "synthetic-model"))
        XCTAssertTrue(
            model.listing!.profiles.flatMap(\.versions).contains { $0.profileVersion == hosted.profileVersion },
            "the earlier version is still stored, so work pinned to it stays resolvable")

        await model.useLocalOnly()
        XCTAssertEqual(model.operatingMode, .localOnly)
    }

    func testTheCoreRefusesWhatTheFormShouldHaveCaught() async throws {
        let model = try await makeModel()
        await model.refresh()
        fill(model, name: "hosted", providerProtocol: .openAI)
        model.draft.endpoint = "https://not-allowed.example"
        XCTAssertFalse(model.canSave)
        XCTAssertTrue(model.issues.contains(.endpointNotAllowed))
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
