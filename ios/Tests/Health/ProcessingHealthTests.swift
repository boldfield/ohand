import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Reads processing health through `ProcessingHealthService` against the real Rust core on the simulator. Events are
/// delivered on the main queue, where XCTest runs.
private final class OutcomeRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var outcomes: [ProcessingHealthOutcome] = []

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return outcomes.count
    }

    func record(_ outcome: ProcessingHealthOutcome) {
        lock.lock()
        outcomes.append(outcome)
        lock.unlock()
    }
}

final class ProcessingHealthTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-health-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: storeDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { storeDirectory.appendingPathComponent("core.sqlite").path }

    /// Creates the schema by opening and closing a core, then runs SQL against the closed store file.
    private func seed(_ statements: [String]) throws {
        try CoreHandle(path: storePath).close()
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(storePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        }
    }

    private func waitForOutcome(_ service: ProcessingHealthService, timeout: TimeInterval = 20) throws
        -> ProcessingHealthOutcome
    {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if !service.isReading, let outcome = service.latestOutcome { return outcome }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no health outcome")
    }

    private func openService() throws -> (CoreHandle, ProcessingHealthService) {
        let core = try CoreHandle(path: storePath)
        let service = ProcessingHealthService(core: core, stallAfterSeconds: 60, leaseGraceSeconds: 30)
        try core.setEventHandler { _ = service.handle($0) }
        return (core, service)
    }

    func testSnapshotDecodesTheCoreWireFormat() throws {
        let json = """
            {"operation_id":5,"generated_at":"2026-01-15T10:30:00Z","summary":"needs_attention","stalled":false,
            "stall_reasons":[],"pending_jobs":2,"running_jobs":1,"overdue_jobs":0,"oldest_pending_age_seconds":90,
            "oldest_overdue_seconds":null,"last_interpretation_success_at":"2026-01-15T10:00:00Z",
            "errors":[{"kind":"destination_unavailable","reason":"destination_unavailable","recovery":"user_action",
            "job_count":2,"oldest_age_seconds":90,"next_attempt_at":null}]}
            """
        let health = try JSONDecoder().decode(ProcessingHealth.self, from: Data(json.utf8))
        XCTAssertEqual(health.summary, .needsAttention)
        XCTAssertEqual(health.errors.first?.kind, .destinationUnavailable)
        XCTAssertEqual(health.errors.first?.recovery, .userAction)
        XCTAssertEqual(health.oldestOverdueSeconds, nil)
        XCTAssertEqual(health.lastInterpretationSuccessAt, "2026-01-15T10:00:00Z")
    }

    func testAnEmptyStoreReadsIdle() throws {
        try seed([])
        let (core, service) = try openService()
        defer { core.close() }
        service.refresh()
        guard case .snapshot(let health) = try waitForOutcome(service) else {
            return XCTFail("expected a snapshot")
        }
        XCTAssertEqual(health.summary, .idle)
        XCTAssertFalse(health.stalled)
        XCTAssertEqual(health.pendingJobs, 0)
        XCTAssertTrue(health.errors.isEmpty)
    }

    func testDueWorkNothingDrainsIsReportedAsAStallWithoutAnyContent() throws {
        try seed([
            "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, "
                + "calendar, item_scope, route_id, entry_locked, created_at) VALUES ('capture-1', "
                + "'call the roofer about the leak', '2020-01-01T00:00:00Z', 'UTC', 0, 'en', 'gregorian', "
                + "'personal', 'route-1', 0, '2020-01-01T00:00:00Z')",
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
                + "processing_state, transcription_state, created_at, updated_at) VALUES ('item-1', 'capture-1', 0, "
                + "'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', "
                + "'2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
            "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, status, "
                + "attempt_count, created_at) VALUES ('job-1', 1, 'item-1', 'interpret', 0, 'queued', 0, "
                + "'2020-01-01T00:00:00Z')",
        ])
        let (core, service) = try openService()
        defer { core.close() }
        let recorder = OutcomeRecorder()
        service.setObserver { recorder.record($0) }
        service.refresh()
        guard case .snapshot(let health) = try waitForOutcome(service) else {
            return XCTFail("expected a snapshot")
        }
        XCTAssertEqual(health.summary, .stalled)
        XCTAssertEqual(health.stallReasons, [.overdue])
        XCTAssertNil(health.lastInterpretationSuccessAt, "a stall is reported although no model ever answered")
        XCTAssertEqual(recorder.count, 1)
        XCTAssertEqual(service.latestOutcome, .snapshot(health))
    }

    func testOverlappingRefreshesShareOneRead() throws {
        try seed([])
        let (core, service) = try openService()
        defer { core.close() }
        service.refresh()
        service.refresh()
        XCTAssertTrue(service.isReading)
        _ = try waitForOutcome(service)
        XCTAssertFalse(service.isReading)
    }

    func testEventsOutsideTheHealthRangeAreNotConsumed() throws {
        try seed([])
        let (core, service) = try openService()
        defer { core.close() }
        let ordinary = CoreEvent(operationID: 3, outcome: .success(Data()))
        let drain = CoreEvent(operationID: JobRunnerService.firstOperationID, outcome: .success(Data()))
        XCTAssertFalse(service.handle(ordinary))
        XCTAssertFalse(service.handle(drain))
    }
}
