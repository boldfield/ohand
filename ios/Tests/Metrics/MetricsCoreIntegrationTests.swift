import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class MetricsAnswerBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// Drives `MetricsService` through the production `CoreMetricsClient` into the real Rust core over an on-disk store.
/// Events arrive on the main queue, where XCTest runs synchronous tests, so waiting pumps the run loop.
final class MetricsCoreIntegrationTests: XCTestCase {
    private var directory: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private let boundary = FakeReadAuthenticationBoundary()

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-metrics-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { directory.appendingPathComponent("core.sqlite").path }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = MetricsAnswerBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    /// Opens the store as one "process": a core, its metrics client and a service over a session.
    private func openProcess(authenticated: Bool) throws -> (CoreHandle, CoreMetricsClient, MetricsService) {
        let core = try CoreHandle(path: storePath)
        let client = CoreMetricsClient(core: core)
        try core.setEventHandler { _ = client.handle($0) }
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated { session.authenticate(reason: "test") { _ in } }
        return (core, client, MetricsService(core: client, session: session))
    }

    func testRecordedFiguresExportAndSurviveRelaunch() throws {
        let (core, client, service) = try openProcess(authenticated: true)
        service.recordTriggerToReady(.cold, elapsed: 0.3)
        service.recordTriggerToReady(.cold, elapsed: 0.5)
        service.recordEndOfInputToSave(.voice, elapsed: 0.08)
        service.recordProcessingFailure(.timeout)
        service.recordSchedulingFailure(.capacityExceeded)
        let first = try run { try await service.exportReport() }
        XCTAssertEqual(first.report.schema, "ohand.metrics.v1")
        let cold = try XCTUnwrap(first.report.timings.first { $0.series == "trigger_to_ready" })
        XCTAssertEqual(cold.dimension, "cold")
        XCTAssertEqual([cold.minMilliseconds, cold.maxMilliseconds, cold.retainedSamples], [300, 500, 2])
        XCTAssertEqual(first.report.timings.first { $0.series == "end_of_input_to_save" }?.medianMilliseconds, 80)
        XCTAssertEqual(
            first.report.failures.map { "\($0.series)/\($0.dimension)/\($0.occurrences)" },
            ["processing_failure/timeout/1", "scheduling_failure/capacity_exceeded/1"])
        XCTAssertEqual(service.droppedRecordCount, 0)
        client.shutDown()
        core.close()

        let (relaunchedCore, relaunchedClient, relaunchedService) = try openProcess(authenticated: true)
        let second = try run { try await relaunchedService.exportReport() }
        XCTAssertEqual(second.report.timings, first.report.timings, "figures survive a relaunch")
        relaunchedClient.shutDown()
        relaunchedCore.close()
    }

    func testCaptureOnlySessionCannotExportAndNothingIsRequestedFromTheCore() throws {
        let (core, client, service) = try openProcess(authenticated: false)
        service.recordEndOfInputToSave(.text, elapsed: 0.02)
        XCTAssertThrowsError(try run { try await service.exportReport() }) { error in
            XCTAssertEqual(error as? MetricsExportFailure, .notAuthenticated)
        }
        client.shutDown()
        core.close()
    }

    func testCaptureOnlySessionCannotDisableOrClearAndRecordingContinues() throws {
        let (core, client, capturingService) = try openProcess(authenticated: false)
        capturingService.recordEndOfInputToSave(.text, elapsed: 0.02)
        XCTAssertFalse(try run { await capturingService.setRecordingEnabled(false) })
        XCTAssertFalse(try run { await capturingService.clearAll() })
        capturingService.recordEndOfInputToSave(.text, elapsed: 0.04)

        let authenticatedSession = ReadAuthenticationSession(boundary: boundary)
        authenticatedSession.authenticate(reason: "test") { _ in }
        let readingService = MetricsService(core: client, session: authenticatedSession)
        let report = try run { try await readingService.exportReport() }
        XCTAssertTrue(report.report.recordingEnabled, "a capture-only session cannot switch recording off")
        XCTAssertEqual(report.report.timings.first?.retainedSamples, 2, "a capture-only session cannot clear")
        client.shutDown()
        core.close()
    }

    func testDisablingStopsRecordingAndClearingRemovesFigures() throws {
        let (core, client, service) = try openProcess(authenticated: true)
        service.recordEndOfInputToSave(.text, elapsed: 0.02)
        XCTAssertTrue(try run { await service.setRecordingEnabled(false) })
        service.recordEndOfInputToSave(.text, elapsed: 0.03)
        let disabled = try run { try await service.exportReport() }
        XCTAssertFalse(disabled.report.recordingEnabled)
        XCTAssertEqual(disabled.report.timings.first?.retainedSamples, 1)

        XCTAssertTrue(try run { await service.clearAll() })
        let cleared = try run { try await service.exportReport() }
        XCTAssertTrue(cleared.report.timings.isEmpty && cleared.report.failures.isEmpty)
        XCTAssertFalse(cleared.report.recordingEnabled, "clearing leaves the switch alone")
        client.shutDown()
        core.close()
    }

    func testAnImplausibleDurationIsRefusedByTheCoreAndCounted() throws {
        let (core, client, service) = try openProcess(authenticated: true)
        service.recordTriggerToReady(.warm, elapsed: 7_200)
        XCTAssertTrue(pump(until: { service.droppedRecordCount == 1 }))
        let report = try run { try await service.exportReport() }
        XCTAssertTrue(report.report.timings.isEmpty)
        client.shutDown()
        core.close()
    }
}
