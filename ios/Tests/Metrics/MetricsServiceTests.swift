import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A core that follows the T01 contract without a database. It records every request and either answers inline or holds
/// the completion so a test can relock the session while an export is in flight.
private final class FakeMetricsCore: MetricsCore, @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [CoreMetricsRequest] = []
    private var held: [@Sendable (Result<CoreMetricsAnswer, CoreFailure>) -> Void] = []
    var holdsCompletions = false
    var answer: Result<CoreMetricsAnswer, CoreFailure> = .success(CoreMetricsAnswer(outcome: "recorded"))

    var requests: [CoreMetricsRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    var heldCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return held.count
    }

    func submit(
        _ request: CoreMetricsRequest,
        completion: @escaping @Sendable (Result<CoreMetricsAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        recorded.append(request)
        let shouldHold = holdsCompletions
        if shouldHold { held.append(completion) }
        let reply = answer
        lock.unlock()
        if !shouldHold { completion(reply) }
    }

    func releaseHeld(with reply: Result<CoreMetricsAnswer, CoreFailure>) {
        lock.lock()
        let waiting = held
        held = []
        lock.unlock()
        waiting.forEach { $0(reply) }
    }
}

private final class MetricsResultBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<CoreMetricsAnswer, CoreFailure>?

    var value: Result<CoreMetricsAnswer, CoreFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<CoreMetricsAnswer, CoreFailure>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

private final class FakeClock: @unchecked Sendable {
    private let lock = NSLock()
    private var nanoseconds: UInt64 = 1_000
    func advance(seconds: Double) {
        lock.lock()
        nanoseconds += UInt64(seconds * 1_000_000_000)
        lock.unlock()
    }
    func now() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return nanoseconds
    }
}

final class MetricsServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()
    private var session: ReadAuthenticationSession!

    override func setUp() {
        super.setUp()
        session = ReadAuthenticationSession(boundary: boundary)
    }

    private func authenticate() {
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    private func sampleReport() -> CoreMetricsReport {
        CoreMetricsReport(
            schema: "ohand.metrics.v1", generatedAt: "2026-01-15T10:30:00Z", recordingEnabled: true,
            sampleLimitPerSeries: 500,
            timings: [
                CoreMetricsTimingSummary(
                    series: "trigger_to_ready", dimension: "cold", retainedSamples: 3, minMilliseconds: 100,
                    medianMilliseconds: 200, p95Milliseconds: 300, maxMilliseconds: 300)
            ],
            failures: [CoreMetricsFailureSummary(series: "processing_failure", dimension: "timeout", occurrences: 2)])
    }

    func testReadyAndSaveLatenciesAreSeparateSeriesInMilliseconds() {
        let core = FakeMetricsCore()
        let service = MetricsService(core: core, session: session)
        service.recordTriggerToReady(.lockedHandoff, elapsed: 0.4504)
        service.recordEndOfInputToSave(.voice, elapsed: 1.2)
        service.recordEndOfInputToSave(.text, elapsed: 0)
        XCTAssertEqual(
            core.requests,
            [
                .recordTiming(.triggerToReady(.lockedHandoff), durationMilliseconds: 450),
                .recordTiming(.endOfInputToSave(.voice), durationMilliseconds: 1200),
                .recordTiming(.endOfInputToSave(.text), durationMilliseconds: 0),
            ])
        XCTAssertEqual(service.droppedRecordCount, 0)
    }

    func testFailuresAreRecordedAsObservedCountsOnly() {
        let core = FakeMetricsCore()
        let service = MetricsService(core: core, session: session)
        service.recordProcessingFailure(.timeout)
        service.recordSchedulingFailure(.permissionDenied)
        XCTAssertEqual(
            core.requests,
            [.recordFailure(.processing(.timeout)), .recordFailure(.scheduling(.permissionDenied))])
    }

    func testRecordingNeedsNoAuthenticationAndNeverBlocksOnTheCore() {
        let core = FakeMetricsCore()
        core.holdsCompletions = true
        let service = MetricsService(core: core, session: session)
        XCTAssertEqual(session.scope, .captureOnly)
        service.recordEndOfInputToSave(.text, elapsed: 0.05)
        XCTAssertEqual(core.requests.count, 1)
        XCTAssertEqual(core.heldCount, 1, "the caller returned while the core had not answered")
    }

    func testRecordsTheCoreCannotTakeAreCountedAndNeverSurfaced() {
        let core = FakeMetricsCore()
        core.answer = .failure(CoreFailure(errorClass: .permanent, code: "storage_error", message: "store"))
        let service = MetricsService(core: core, session: session)
        service.recordProcessingFailure(.other)
        service.recordTriggerToReady(.cold, elapsed: -1)
        service.recordTriggerToReady(.cold, elapsed: .nan)
        service.recordTriggerToReady(.cold, elapsed: .infinity)
        XCTAssertEqual(service.droppedRecordCount, 4)
        XCTAssertEqual(core.requests.count, 1, "an inexpressible duration is dropped before it reaches the core")
    }

    func testStopwatchUsesTheMonotonicClock() {
        let clock = FakeClock()
        let service = MetricsService(core: FakeMetricsCore(), session: session, monotonicNanoseconds: { clock.now() })
        let stopwatch = service.startStopwatch()
        clock.advance(seconds: 0.25)
        XCTAssertEqual(stopwatch.elapsed(), 0.25, accuracy: 0.000_001)
    }

    func testSwitchAndClearAreAcknowledgedOnlyWhenTheCoreApplies() async {
        let core = FakeMetricsCore()
        core.answer = .success(CoreMetricsAnswer(outcome: "ok"))
        let service = MetricsService(core: core, session: session)
        let disabled = await service.setRecordingEnabled(false)
        let cleared = await service.clearAll()
        XCTAssertTrue(disabled)
        XCTAssertTrue(cleared)
        XCTAssertEqual(core.requests, [.setEnabled(false), .clear])

        core.answer = .failure(CoreFailure(errorClass: .transient, code: "store_busy", message: "busy"))
        let refused = await service.clearAll()
        XCTAssertFalse(refused)
    }

    func testExportRefusesACaptureOnlySessionWithoutAskingTheCore() async {
        let core = FakeMetricsCore()
        let service = MetricsService(core: core, session: session)
        do {
            _ = try await service.exportReport()
            XCTFail("a capture-only session must not export")
        } catch {
            XCTAssertEqual(error as? MetricsExportFailure, .notAuthenticated)
        }
        XCTAssertTrue(core.requests.isEmpty)
    }

    func testAuthenticatedExportReturnsStableAggregateJSON() async throws {
        authenticate()
        let core = FakeMetricsCore()
        core.answer = .success(CoreMetricsAnswer(outcome: "report", report: sampleReport()))
        let service = MetricsService(core: core, session: session)
        let export = try await service.exportReport()
        XCTAssertEqual(core.requests, [.export(readAuthentication: .authenticated)])
        XCTAssertEqual(export.report, sampleReport())
        let decoded = try JSONDecoder().decode(CoreMetricsReport.self, from: export.json)
        XCTAssertEqual(decoded, sampleReport())
        let text = try XCTUnwrap(String(data: export.json, encoding: .utf8))
        XCTAssertTrue(text.contains("\"occurrences\" : 2"))
    }

    func testARelockWhileTheCoreIsAnsweringDiscardsTheReport() async {
        authenticate()
        let core = FakeMetricsCore()
        core.holdsCompletions = true
        let service = MetricsService(core: core, session: session)
        let export = Task { try await service.exportReport() }
        while core.heldCount == 0 { await Task.yield() }
        session.relock(.enteredBackground)
        core.releaseHeld(with: .success(CoreMetricsAnswer(outcome: "report", report: sampleReport())))
        do {
            _ = try await export.value
            XCTFail("a report must not be handed over after a relock")
        } catch {
            XCTAssertEqual(error as? MetricsExportFailure, .interrupted)
        }
    }

    func testCoreRefusalAndAnAnswerWithoutAReportAreFailuresNotEmptyReports() async {
        authenticate()
        let core = FakeMetricsCore()
        let service = MetricsService(core: core, session: session)
        let refusal = CoreFailure(
            errorClass: .unauthorized, code: "metrics_export_not_authorized", message: "not authorized")
        core.answer = .failure(refusal)
        do {
            _ = try await service.exportReport()
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? MetricsExportFailure, .core(refusal))
        }
        core.answer = .success(CoreMetricsAnswer(outcome: "recorded"))
        do {
            _ = try await service.exportReport()
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? MetricsExportFailure, .unreadable)
        }
    }

    func testRequestsOnlyUseTheClosedVocabulary() throws {
        let timing = try CoreMetricsRequest.recordTiming(.triggerToReady(.lockedHandoff), durationMilliseconds: 7)
            .encoded()
        XCTAssertEqual(
            try JSONSerialization.jsonObject(with: timing) as? NSDictionary,
            [
                "op": "record_timing", "series": "trigger_to_ready", "dimension": "locked_handoff",
                "duration_ms": 7,
            ] as NSDictionary)
        let failure = try CoreMetricsRequest.recordFailure(.scheduling(.capacityExceeded)).encoded()
        XCTAssertEqual(
            try JSONSerialization.jsonObject(with: failure) as? NSDictionary,
            ["op": "record_failure", "series": "scheduling_failure", "dimension": "capacity_exceeded"] as NSDictionary)
        let export = try CoreMetricsRequest.export(readAuthentication: .captureOnly).encoded()
        XCTAssertEqual(
            try JSONSerialization.jsonObject(with: export) as? NSDictionary,
            ["op": "export", "read_authentication": "capture_only"] as NSDictionary)
    }

    func testTheClientAnswersOnlyItsOwnOperationsAndFailsWaitersOnShutDown() throws {
        let core = try CoreHandle(path: ":memory:")
        defer { core.close() }
        let client = CoreMetricsClient(core: core)
        XCTAssertFalse(client.handle(CoreEvent(operationID: 1, outcome: .success(Data()))))
        client.shutDown()
        let box = MetricsResultBox()
        client.submit(.clear) { box.set($0) }
        XCTAssertEqual(box.value, .failure(CoreMetricsClient.shutDownFailure))
    }
}
