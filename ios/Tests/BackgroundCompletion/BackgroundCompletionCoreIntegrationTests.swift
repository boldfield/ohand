import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Capture end, screen lock and execution-window expiry against the real Rust core on the simulator: the real
/// `JobRunnerService` drains a real stored job, the real `ProcessingHealthService` reads what is still queued, and the
/// real `BackgroundCompletionService` and `PendingProcessingAlert` sit between them. Only the OS pieces are replaced:
/// the background grant (a test fires its expiration the way the system does), the notification center, and the
/// provider vendor URL. Real-device timing of the window is T05's evidence, not this file's.
final class BackgroundCompletionCoreIntegrationTests: ProviderTransportTestCase {
    private var storeDirectory: URL!
    private var sessions: [BackgroundCompletionSession] = []
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-background-completion-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        sessions.forEach { $0.close() }
        sessions = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core was released")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was released")
        try? FileManager.default.removeItem(at: storeDirectory)
        super.tearDown()
    }

    // MARK: helpers

    private func makeServer() throws -> FixtureTLSServer {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": 4]],
        ]
        let reply: [String: Any] = [
            "id": "msg_synthetic_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_synthetic_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        let body = try JSONSerialization.data(withJSONObject: reply)
        return try startServer { _ in .complete(status: 200, headers: ["Content-Type": "application/json"], body: body) }
    }

    private func makeStore() throws -> String {
        let reference = try storeSecret()
        return try ProviderExchangeSeed.makeStore(
            in: storeDirectory, scenario: ProviderExchangeScenario(credentialReference: reference))
    }

    private func open(
        _ storePath: String, sender: ProviderRequestSender, capabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = true
    ) throws -> BackgroundCompletionSession {
        let session = try BackgroundCompletionSession(
            storePath: storePath, sender: sender, capabilities: capabilities, isReachable: isReachable)
        sessions.append(session)
        return session
    }

    private func job(_ session: BackgroundCompletionSession, _ id: String = ProviderExchangeScenario.jobID) -> StoredJob? {
        JobStoreInspector.job(at: session.storePath, id: id)
    }

    private func pendingIdentifiers(_ session: BackgroundCompletionSession) async throws -> [String] {
        try await session.center.pendingRequests().map(\.identifier)
    }

    @MainActor private func startCapture(
        _ completion: BackgroundCompletionService, _ session: BackgroundCompletionSession
    ) async -> Task<BackgroundCompletionOutcome, Never> {
        let lease = Task { await completion.completeAfterCapture() }
        var isArmed = false
        let deadline = Date().addingTimeInterval(15)
        while !isArmed, Date() < deadline {
            let identifiers = (try? await pendingIdentifiers(session)) ?? []
            isArmed = session.grant.beginCount == 1 && identifiers.contains(PendingProcessingAlert.identifier)
            if !isArmed { try? await Task.sleep(nanoseconds: 5_000_000) }
        }
        XCTAssertTrue(isArmed, "the lease asked for time and armed its one alert")
        return lease
    }

    // MARK: lock and expiry

    @MainActor func testLockRightAfterRecordingThenExpiryCheckpointsTheJobAndLosesNothing() async throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let session = try open(storePath, sender: gated)
        let completion = try session.makeCompletion()

        session.service.activate()
        let didStart = await waitUntilTrue { gated.requestCount == 1 }
        XCTAssertTrue(didStart)
        XCTAssertEqual(job(session)?.status, "running")

        let lease = await startCapture(completion, session)
        session.service.suspend()
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertTrue(session.service.isDraining, "locking inside the completion window does not stop the drain")
        XCTAssertEqual(job(session)?.status, "running")

        session.grant.fireExpiration()
        let outcome = await lease.value
        XCTAssertEqual(outcome, .expired(.systemExpiration))

        let didFinish = await waitUntilTrue { session.service.finishedDrainCount == 1 && !session.service.isDraining }
        XCTAssertTrue(didFinish)
        let stored = try XCTUnwrap(job(session))
        XCTAssertEqual(stored.status, "queued", "the acknowledged work is back in the queue, saved and unprocessed")
        XCTAssertEqual(stored.failureReason, "cancelled", "recorded as a checkpoint, not a failure")
        XCTAssertEqual(session.grant.endedTokenList, [7], "the background task is ended once")

        let identifiers = try await pendingIdentifiers(session)
        XCTAssertEqual(identifiers, [PendingProcessingAlert.identifier], "one separate generic alert, no reminder")
        XCTAssertNil(session.center.addedRequests.first?.content.userInfo[NotificationContent.targetKey])

        session.service.reachabilityChanged(true)
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(session.service.startedDrainCount, 1, "nothing retries in the background after expiry")
        XCTAssertEqual(session.center.addedRequests.count, 1, "no endless alerts")
    }

    @MainActor func testTheWindowCapEndsAnUnfinishedDrainOnItsOwn() async throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let session = try open(storePath, sender: gated)
        let completion = try session.makeCompletion(maximumWindow: 0.4)

        session.service.suspend()
        let outcome = await completion.completeAfterCapture()

        XCTAssertEqual(outcome, .expired(.windowElapsed))
        XCTAssertEqual(gated.requestCount, 1, "the drain did run in the background window")
        let didFinish = await waitUntilTrue { session.service.finishedDrainCount == 1 && !session.service.isDraining }
        XCTAssertTrue(didFinish)
        XCTAssertEqual(job(session)?.status, "queued")
        XCTAssertEqual(job(session)?.failureReason, "cancelled", "recorded as a checkpoint, not a failure")
        XCTAssertEqual(session.grant.endedTokenList, [7])
        let identifiers = try await pendingIdentifiers(session)
        XCTAssertEqual(identifiers, [PendingProcessingAlert.identifier])
    }

    @MainActor func testTheCheckpointedJobRunsOnTheNextLaunchAfterAnExpiredWindow() async throws {
        let server = try makeServer()
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let first = try open(storePath, sender: gated)
        let completion = try first.makeCompletion(maximumWindow: 0.3)

        first.service.suspend()
        _ = await completion.completeAfterCapture()
        let didFinish = await waitUntilTrue { first.service.finishedDrainCount == 1 && !first.service.isDraining }
        XCTAssertTrue(didFinish)
        first.close()

        let restarted = try open(storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))
        restarted.service.activate()
        let didComplete = await waitUntilTrue { restarted.service.finishedDrainCount == 1 && !restarted.service.isDraining }
        XCTAssertTrue(didComplete)
        XCTAssertEqual(job(restarted)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
    }

    // MARK: finishing inside the window

    @MainActor func testWorkThatFinishesInsideTheWindowCompletesAndRemovesTheAlert() async throws {
        let server = try makeServer()
        let storePath = try makeStore()
        let session = try open(storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))
        let completion = try session.makeCompletion(maximumWindow: 90)

        session.service.suspend()
        let outcome = await completion.completeAfterCapture()

        XCTAssertEqual(outcome, .completed)
        XCTAssertEqual(job(session)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
        XCTAssertEqual(session.center.addedRequests.count, 1, "the safety-net alert was armed first")
        let identifiers = try await pendingIdentifiers(session)
        XCTAssertTrue(identifiers.isEmpty, "and removed once nothing was pending")
        XCTAssertEqual(session.grant.endedTokenList, [7])

        session.service.reachabilityChanged(true)
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(session.service.startedDrainCount, 1, "the closed window lets nothing else start")
    }

    @MainActor func testJobsTheCoreHoldsBackStayQueuedAndTheAlertStaysArmed() async throws {
        let server = try makeServer()
        let storePath = try makeStore()
        try JobStoreInspector.insertQueuedJob(at: storePath, id: "job-native-b04", jobType: "transcribe")
        let capability = SettlingCapability(jobType: "transcribe", storePath: storePath)
        let session = try open(
            storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server),
            capabilities: [capability], isReachable: false)
        let completion = try session.makeCompletion()

        session.service.suspend()
        let outcome = await completion.completeAfterCapture()

        guard case .pendingRemains(let pendingJobs?) = outcome else {
            return XCTFail("expected pending work to remain, got \(outcome)")
        }
        XCTAssertGreaterThanOrEqual(pendingJobs, 1)
        XCTAssertEqual(job(session, "job-native-b04")?.status, "completed", "the on-device work did finish")
        XCTAssertEqual(job(session)?.status, "queued", "the offline provider job stays saved and unprocessed")
        XCTAssertTrue(server.requests.isEmpty)
        let identifiers = try await pendingIdentifiers(session)
        XCTAssertEqual(identifiers, [PendingProcessingAlert.identifier])
        XCTAssertEqual(session.grant.endedTokenList, [7])
    }

    @MainActor func testBecomingActiveRemovesTheAlertAndRunsTheQueuedWork() async throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let session = try open(storePath, sender: gated)
        let completion = try session.makeCompletion(maximumWindow: 0.3)

        session.service.suspend()
        _ = await completion.completeAfterCapture()
        let before = try await pendingIdentifiers(session)
        XCTAssertEqual(before, [PendingProcessingAlert.identifier])

        await completion.appBecameActive()
        let after = try await pendingIdentifiers(session)
        XCTAssertTrue(after.isEmpty)
    }
}
