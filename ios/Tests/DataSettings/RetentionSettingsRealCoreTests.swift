import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class UnreachableSettingsProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private struct BuiltRoot: @unchecked Sendable {
    let root: ServiceCompositionRoot
    let coordinator: LifecycleCoordinator
}

/// Drives `RetentionSettingsModel` over the real composition root and Rust core on an on-disk store: the real policy,
/// the real unresolved list, the core's refusals, and a capture saved while a sweep runs.
@MainActor
final class RetentionSettingsRealCoreTests: XCTestCase {
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
    private func makeModel() async throws -> RetentionSettingsModel {
        let layoutPath = world.layout.rootDirectory.path
        let readSession = session!
        let built = try await Task.detached { () -> BuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: UnreachableSettingsProviderSender(), readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)),
                modules: [coordinator])
            try coordinator.connect(to: root)
            return BuiltRoot(root: root, coordinator: coordinator)
        }.value
        root = built.root
        coordinator = built.coordinator
        return RetentionSettingsModel.assembled(root: root)
    }

    func testTheModelShowsTheRealDefaultPolicyAndSoleAudioAndStoresAChangedPeriod() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        _ = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let model = try await makeModel()

        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.status?.policy, RetentionPolicySetting(periodSeconds: 7 * 86_400, isDefault: true))
        XCTAssertEqual(model.status?.unresolved.map(\.itemID), [sole.itemID])

        model.chosenDays = 30
        await model.applyPeriod()
        XCTAssertNil(model.periodState)
        XCTAssertEqual(model.status?.policy, RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))
        XCTAssertTrue(world.audioExists(sole.fileName), "changing the period deletes nothing")
    }

    func testRetryAndDeleteUseTheCoresOperationsAndSoleSourceAudioIsKept() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let model = try await makeModel()
        await model.refresh()

        await model.deleteAudio(itemID: sole.itemID)
        XCTAssertEqual(
            model.actions[sole.itemID],
            .failed(RetentionSettingsText.failure(RetentionControlError.soleSourceUseItemDeletion)))
        XCTAssertTrue(world.audioExists(sole.fileName), "the core refused, so the file is untouched")
        XCTAssertEqual(try world.ledgerEntries(), [])

        await model.retryTranscription(itemID: sole.itemID)
        XCTAssertEqual(
            model.actions[sole.itemID],
            .failed(RetentionSettingsText.failure(RetentionControlError.jobInFlight)),
            "a freshly imported voice capture already has a queued job")

        world.clearJobs(itemID: sole.itemID)
        await model.retryTranscription(itemID: sole.itemID)
        XCTAssertEqual(model.actions[sole.itemID], .queued)
    }

    func testARelockedSessionShowsNothingFromTheCore() async throws {
        _ = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let model = try await makeModel()
        await model.refresh()
        XCTAssertNotNil(model.status)

        session.relock(.explicit)
        await model.refresh()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.status)
    }

    func testANewCaptureIsSavedWhileASweepAndAStatusReadRun() async throws {
        _ = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 10)
        let model = try await makeModel()
        let retention = try XCTUnwrap(coordinator.audioRetention)

        let saved = expectation(description: "capture saved")
        async let sweep = retention.sweep()
        async let refresh: Void = model.refresh()
        root.saveText(captureID: "cap-new", text: "synthetic note during sweep", capturedAt: Date()) { outcome in
            if case .saved = outcome { saved.fulfill() }
        }
        await fulfillment(of: [saved], timeout: 30)
        let outcome = await sweep
        await refresh

        XCTAssertNil(outcome.listingFailure)
        XCTAssertEqual(model.phase, .loaded)
    }
}
