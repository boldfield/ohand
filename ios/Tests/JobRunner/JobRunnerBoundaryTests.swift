import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The job loop on the simulator with nothing replaced inside the app: the real Rust core drains a real stored
/// job, the real `JobRunnerService` hands its provider request to the real `ProviderHTTPTransport`, and the answer
/// travels back through the core's provider adapter. Only the vendor URL is replaced, by a fixture server whose
/// certificate the transport trusts. Restarts open a second real core over the same store file.
final class JobRunnerBoundaryTests: ProviderTransportTestCase {
    private var storeDirectory: URL!
    private var sessions: [JobRunnerSession] = []
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-job-runner-\(UUID().uuidString)")
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

    private func anthropicReply() throws -> Data {
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
        return try JSONSerialization.data(withJSONObject: reply)
    }

    private func makeServer() throws -> FixtureTLSServer {
        let reply = try anthropicReply()
        return try startServer { _ in .complete(status: 200, headers: ["Content-Type": "application/json"], body: reply) }
    }

    private func makeStore() throws -> String {
        let reference = try storeSecret()
        return try ProviderExchangeSeed.makeStore(
            in: storeDirectory, scenario: ProviderExchangeScenario(credentialReference: reference))
    }

    private func open(
        _ storePath: String,
        sender: ProviderRequestSender,
        capabilities: [JobCapabilityHandler] = [],
        isReachable: Bool = true,
        retryDelay: TimeInterval = 3600
    ) throws -> JobRunnerSession {
        let session = try JobRunnerSession(
            storePath: storePath, sender: sender, capabilities: capabilities, isReachable: isReachable,
            retryDelay: retryDelay)
        sessions.append(session)
        return session
    }

    private func job(_ session: JobRunnerSession, _ id: String = ProviderExchangeScenario.jobID) -> StoredJob? {
        JobStoreInspector.job(at: session.storePath, id: id)
    }

    // MARK: launch and completion

    func testLaunchDrainsTheReadyJobThroughTheRealTransportAndCompletesIt() throws {
        let server = try makeServer()
        let storePath = try makeStore()
        let session = try open(storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(1))

        let summary = try XCTUnwrap(session.summary())
        XCTAssertEqual(summary.stop, "idle")
        XCTAssertEqual(summary.jobs.map(\.jobID), [ProviderExchangeScenario.jobID])
        XCTAssertEqual(summary.jobs.first?.jobType, "interpret")
        XCTAssertEqual(summary.jobs.first?.settlement, "completed")
        XCTAssertEqual(job(session)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
        XCTAssertEqual(server.requests.first?.headers["x-api-key"], syntheticSecret, "the transport attached the secret")
        XCTAssertFalse(String(decoding: server.requests.first?.body ?? Data(), as: UTF8.self).contains(syntheticSecret))
        XCTAssertTrue(session.otherEvents.isEmpty, "drain events are consumed by the service")
    }

    func testNothingRunsBeforeTheAppIsActive() throws {
        let server = try makeServer()
        let session = try open(try makeStore(), sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))

        session.pumpFor(0.5)

        XCTAssertEqual(session.service.startedDrainCount, 0)
        XCTAssertEqual(job(session)?.status, "queued")
        XCTAssertTrue(server.requests.isEmpty)
    }

    // MARK: duplicate drainers

    func testRepeatedAndOverlappingActivationNeverRunsTwoDrainsAtOnce() throws {
        let server = try makeServer()
        let gated = GatedSender(forwarding: FixtureRoutingSender(underlying: transport, fixtureServer: server))
        let session = try open(try makeStore(), sender: gated)

        for _ in 0..<5 {
            session.service.activate()
            session.service.reachabilityChanged(true)
        }
        XCTAssertTrue(session.pump(until: { gated.requestCount == 1 }))
        session.pumpFor(0.3)

        XCTAssertEqual(session.service.startedDrainCount, 1, "overlapping triggers coalesce")
        XCTAssertTrue(session.service.isDraining)
        XCTAssertEqual(gated.requestCount, 1)

        // The core refuses a second concurrent drain by itself, whoever asks.
        XCTAssertThrowsError(try session.core.startJobDrain(operationID: 7, storePath: session.storePath)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "drain_in_progress")
        }

        gated.open()
        XCTAssertTrue(session.waitForFinishedDrains(2))
        XCTAssertEqual(session.service.startedDrainCount, 2, "the coalesced triggers run exactly one more drain")
        XCTAssertEqual(gated.requestCount, 1, "the job was sent once")
        XCTAssertEqual(server.requests.count, 1)
        XCTAssertEqual(job(session)?.status, "completed")
    }

    // MARK: suspension and restart

    func testSuspensionCheckpointsTheJobAndTheNextLaunchResumesItAfterRestart() throws {
        let server = try makeServer()
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let first = try open(storePath, sender: gated)

        first.service.activate()
        XCTAssertTrue(first.pump(until: { gated.requestCount == 1 }))
        XCTAssertEqual(job(first)?.status, "running")

        first.service.suspend()
        XCTAssertTrue(first.waitForFinishedDrains(1))

        let summary = try XCTUnwrap(first.summary())
        XCTAssertTrue(["cancelled", "interrupted"].contains(summary.stop), summary.stop)
        XCTAssertEqual(summary.jobs.first?.settlement, "interrupted")
        let checkpointed = try XCTUnwrap(job(first))
        XCTAssertEqual(checkpointed.status, "queued", "the job is back in the queue, not failed")
        XCTAssertTrue(server.requests.isEmpty, "the abandoned request never reached the provider")

        first.close()

        let restarted = try open(storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))
        restarted.service.activate()
        XCTAssertTrue(restarted.waitForFinishedDrains(1))
        XCTAssertEqual(restarted.summary()?.jobs.first?.settlement, "completed")
        XCTAssertEqual(job(restarted)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
    }

    func testTerminationTakesTheSameCheckpointAndStartsNothingFurther() throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let session = try open(storePath, sender: gated)

        session.service.activate()
        XCTAssertTrue(session.pump(until: { gated.requestCount == 1 }))
        session.service.terminate()
        XCTAssertTrue(session.waitForFinishedDrains(1))
        session.service.reachabilityChanged(true)
        session.pumpFor(0.3)

        XCTAssertEqual(job(session)?.status, "queued")
        XCTAssertEqual(session.service.startedDrainCount, 1, "a terminating app starts no new drain")
    }

    func testBackgroundedAppStartsNoDrainUntilItBecomesActiveAgain() throws {
        let server = try makeServer()
        let session = try open(try makeStore(), sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(1))
        session.service.suspend()
        session.service.reachabilityChanged(true)
        session.pumpFor(0.3)
        XCTAssertEqual(session.service.startedDrainCount, 1)

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(2))
        XCTAssertEqual(session.service.startedDrainCount, 2)
    }

    // MARK: network

    func testNetworkReturnResumesTheDurablePendingJob() throws {
        let server = try makeServer()
        let session = try open(
            try makeStore(), sender: FixtureRoutingSender(underlying: transport, fixtureServer: server), isReachable: false)

        session.service.activate()
        session.pumpFor(0.5)
        XCTAssertEqual(session.service.startedDrainCount, 0, "no provider work starts while offline")
        XCTAssertEqual(job(session)?.status, "queued")
        XCTAssertEqual(job(session)?.attemptCount, 0, "nothing was claimed while offline")

        session.service.reachabilityChanged(true)
        XCTAssertTrue(session.waitForFinishedDrains(1))
        XCTAssertEqual(job(session)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
    }

    func testLosingTheNetworkMidDrainPutsTheJobBackWithoutSpendingARetry() throws {
        let server = try makeServer()
        let gated = GatedSender(forwarding: FixtureRoutingSender(underlying: transport, fixtureServer: server))
        let session = try open(try makeStore(), sender: gated)

        session.service.activate()
        XCTAssertTrue(session.pump(until: { gated.requestCount == 1 }))
        let attemptsWhileRunning = try XCTUnwrap(job(session)).attemptCount

        session.service.reachabilityChanged(false)
        XCTAssertTrue(session.waitForFinishedDrains(1))
        let stored = try XCTUnwrap(job(session))
        XCTAssertEqual(stored.status, "queued")
        XCTAssertLessThanOrEqual(stored.attemptCount, attemptsWhileRunning)
        XCTAssertTrue(server.requests.isEmpty)

        gated.open()
        session.service.reachabilityChanged(true)
        XCTAssertTrue(session.waitForFinishedDrains(2))
        XCTAssertEqual(job(session)?.status, "completed")
        XCTAssertEqual(server.requests.count, 1)
    }

    func testATransientFailureIsRetriedByTheTimerAfterTheCoreBackoff() throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil, open: true)
        let session = try open(storePath, sender: gated, retryDelay: 1.5)

        session.service.activate()
        XCTAssertTrue(session.pump(until: { gated.requestCount >= 2 }, timeout: 20), "the timer drained again")

        var settled: StoredJob?
        XCTAssertTrue(
            session.pump(until: {
                settled = self.job(session)
                return ["queued", "failed"].contains(settled?.status ?? "")
            }),
            "the retried attempt settles back to a non-running state")
        XCTAssertGreaterThanOrEqual(try XCTUnwrap(settled).attemptCount, 2)
    }

    // MARK: capture stays responsive

    func testCaptureSavesWhileADrainIsWaitingOnTheNetwork() throws {
        let storePath = try makeStore()
        let gated = GatedSender(forwarding: nil)
        let session = try open(storePath, sender: gated)

        session.service.activate()
        XCTAssertTrue(session.pump(until: { gated.requestCount == 1 }))
        XCTAssertTrue(session.service.isDraining)

        let capture = CaptureRecord(
            captureID: "5a1c0000-0000-4000-8000-0000000000d1",
            text: "buy oat milk",
            captureInstant: "2026-10-08T09:31:00Z",
            timezoneID: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeID: "route-synthetic",
            entryLocked: false,
            createdAt: "2026-10-08T09:31:01Z")
        try session.core.startSaveCapture(operationID: 41, capture: capture)
        XCTAssertTrue(
            session.pump(until: { session.otherEvents.contains { $0.operationID == 41 } }, timeout: 5),
            "the save committed while the drain was still in flight")
        XCTAssertTrue(session.service.isDraining)
        let acknowledgment = try XCTUnwrap(session.otherEvents.first { $0.operationID == 41 })
        XCTAssertNoThrow(try acknowledgment.decode(SaveCaptureAcknowledgment.self))
    }

    // MARK: native capability seam

    func testARegisteredNativeCapabilityRunsItsJobTypeAndUsesTheCoreRetryRules() throws {
        let server = try makeServer()
        let storePath = try makeStore()
        try JobStoreInspector.insertQueuedJob(at: storePath, id: "job-native-1", jobType: "transcribe")
        let capability = ScriptedCapability(jobType: "transcribe", result: .transientFailure(reason: "audio_not_ready"))
        let session = try open(
            storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server),
            capabilities: [capability])

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(1))

        let runs = capability.receivedRuns
        XCTAssertEqual(runs.map(\.jobID), ["job-native-1"])
        XCTAssertEqual(runs.first?.jobType, "transcribe")
        let native = try XCTUnwrap(job(session, "job-native-1"))
        XCTAssertEqual(native.status, "queued")
        XCTAssertEqual(native.failureReason, "audio_not_ready")
        XCTAssertEqual(job(session)?.status, "completed", "core jobs still run beside the native one")
    }

    func testAPermanentNativeFailureEndsTheJobAndAnUnlabelledReasonIsNeverStored() throws {
        let storePath = try makeStore()
        try JobStoreInspector.insertQueuedJob(at: storePath, id: "job-native-2", jobType: "transcribe")
        let capability = ScriptedCapability(jobType: "transcribe", result: .permanentFailure(reason: "call the roofer at 9"))
        let session = try open(storePath, sender: GatedSender(forwarding: nil, open: true), capabilities: [capability])

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(1))

        let native = try XCTUnwrap(job(session, "job-native-2"))
        XCTAssertEqual(native.status, "failed")
        XCTAssertEqual(native.failureReason, "native_failure")
        XCTAssertFalse((native.failureReason ?? "").contains("roofer"))
    }

    func testAJobTypeWithoutAHandlerIsDeferredNotClaimedAndNothingPretendsItWorked() throws {
        let server = try makeServer()
        let storePath = try makeStore()
        try JobStoreInspector.insertQueuedJob(at: storePath, id: "job-native-3", jobType: "transcribe")
        let session = try open(storePath, sender: FixtureRoutingSender(underlying: transport, fixtureServer: server))

        session.service.activate()
        XCTAssertTrue(session.waitForFinishedDrains(1))

        let summary = try XCTUnwrap(session.summary())
        let deferred = try XCTUnwrap(summary.jobs.first { $0.jobType == "transcribe" })
        XCTAssertEqual(deferred.result, "capability_unavailable")
        XCTAssertEqual(job(session, "job-native-3")?.status, "queued")
    }
}
