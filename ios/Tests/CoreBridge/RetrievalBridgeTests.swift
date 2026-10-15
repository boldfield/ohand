import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge

/// Wait at least as long as the core's own 5 s busy timeout (core/src/store/schema/mod.rs) for its write lock.
private let retrievalStoreBusyTimeoutMilliseconds: Int32 = 5_000

/// Imports synthetic captures through the real Rust core and retrieves them through `CoreHandle.startRetrieval`.
/// Outcomes are events delivered on the main queue, where XCTest runs.
final class RetrievalBridgeTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!
    private var core: CoreHandle!
    private var events: [CoreEvent] = []
    private var nextOperationID: UInt64 = 1

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-retrieval-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
        let path = storeDirectory.appendingPathComponent("core.sqlite").path
        try CoreHandle(path: path).close()
        try seedRoutes(path: path)
        core = try CoreHandle(path: path)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            self.events.append(event)
        }
        try importCapture(id: "personal-1", text: "Buy oat milk and apples", scope: "personal")
        try importCapture(id: "work-1", text: "Quarterly milk budget review zebrafinch", scope: "work")
    }

    override func tearDown() {
        core?.close()
        core = nil
        try? FileManager.default.removeItem(at: storeDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func seedRoutes(path: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        sqlite3_busy_timeout(database, retrievalStoreBusyTimeoutMilliseconds)
        defer { sqlite3_close(database) }
        for scope in ["personal", "work"] {
            let statement = "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-\(scope)', 'route-\(scope)', '\(scope)', '[]', '2026-10-08T09:00:00Z')"
            XCTAssertEqual(sqlite3_exec(database, statement, nil, nil, nil), SQLITE_OK)
        }
    }

    private func run(_ start: (UInt64) throws -> Void, timeout: TimeInterval = 20) throws -> CoreEvent {
        let operationID = nextOperationID
        nextOperationID += 1
        try start(operationID)
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let event = events.first(where: { $0.operationID == operationID }) { return event }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no event for the operation")
    }

    private func importCapture(id: String, text: String, scope: String) throws {
        let record = CaptureRecord(
            captureID: id,
            text: text,
            captureInstant: "2026-10-08T09:30:00Z",
            timezoneID: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: scope,
            routeID: "route-\(scope)",
            entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z"
        )
        let event = try run { try core.startImportForegroundIngress(operationID: $0, record: record) }
        _ = try event.decode(IngressImportAcknowledgment.self)
    }

    private func retrieve(_ request: CoreRetrievalRequest) throws -> CoreRetrievalAnswer {
        let event = try run { try core.startRetrieval(operationID: $0, request: request) }
        return try event.decode(CoreRetrievalAnswer.self)
    }

    func testSearchReturnsTheHitAndSpanForAnAuthorizedScope() throws {
        let answer = try retrieve(CoreRetrievalRequest(operation: .search(query: "oat"), readScopes: [.personal]))
        XCTAssertEqual(answer.totalAccessible, 1)
        let hit = try XCTUnwrap(answer.hits.first)
        XCTAssertEqual(hit.captureID, "personal-1")
        XCTAssertEqual(hit.scope, "personal")
        XCTAssertEqual(hit.text, "Buy oat milk and apples")
        XCTAssertEqual(hit.textBasis, .original)
        XCTAssertNil(hit.originalText)
        XCTAssertEqual(hit.matched, .current)
        XCTAssertEqual(hit.matchSpan, CoreRetrievalMatchSpan(start: 4, end: 7))
        XCTAssertEqual(hit.capturedAt, "2026-10-08T09:30:00Z")
    }

    func testAnExcludedScopeContributesNoHitOrCount() throws {
        let personalOnly = try retrieve(
            CoreRetrievalRequest(operation: .search(query: "milk"), readScopes: [.personal]))
        XCTAssertEqual(personalOnly.totalAccessible, 1)
        XCTAssertEqual(personalOnly.hits.map(\.captureID), ["personal-1"])

        let workTerm = try retrieve(
            CoreRetrievalRequest(operation: .search(query: "zebrafinch"), readScopes: [.personal]))
        XCTAssertEqual(workTerm.totalAccessible, 0)
        XCTAssertTrue(workTerm.hits.isEmpty)

        let both = try retrieve(CoreRetrievalRequest(operation: .list, readScopes: [.personal, .work]))
        XCTAssertEqual(both.totalAccessible, 2)
    }

    func testARequestWithoutReadScopesIsRefusedBeforeAnythingRuns() throws {
        let request = CoreRetrievalRequest(operation: .search(query: "milk"), readScopes: [])
        XCTAssertThrowsError(try core.startRetrieval(operationID: 900, request: request)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request")
        }
    }

    func testAPageOutsideTheBoundsIsRefused() throws {
        let request = CoreRetrievalRequest(operation: .list, readScopes: [.personal], limit: 101)
        XCTAssertThrowsError(try core.startRetrieval(operationID: 901, request: request)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request")
        }
    }

    func testPagingReportsTheUnpagedTotal() throws {
        let answer = try retrieve(
            CoreRetrievalRequest(operation: .list, readScopes: [.personal, .work], limit: 1, offset: 1))
        XCTAssertEqual(answer.totalAccessible, 2)
        XCTAssertEqual(answer.hits.count, 1)
    }
}
