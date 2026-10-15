import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives the session and gate against the real Rust core on the simulator, over the database inside the protected
/// store location. The simulator cannot lock the device or show the app-switcher snapshot, so those two physical
/// checks are device evidence (task T05, `docs/validation/m1-device-results.md`); what is proven here is that the native
/// authentication session decides whether the core is asked for stored content at all, and whether its answer is
/// delivered.
final class PrivateReadCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var layout: ProtectedStorageLayout!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-private-read-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        let report = ProtectedStorageService(layout: layout).prepare()
        XCTAssertTrue(report.isFullyConfigured, "failures: \(report.failures)")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func makeCapture(
        id: String, text: String, itemScope: String = "personal", routeID: String = "route-default"
    ) -> CaptureRecord {
        CaptureRecord(
            captureID: id,
            text: text,
            captureInstant: "2026-10-08T09:30:00Z",
            timezoneID: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: itemScope,
            routeID: routeID,
            entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z"
        )
    }

    /// Adapts the real core handle to the gate's read-verb protocol, as app assembly does.
    private final class CoreHandleReadSource: PrivateReadSource {
        private let core: CoreHandle
        init(core: CoreHandle) { self.core = core }

        func startStoreCheck(operationID: UInt64) throws {
            try core.startStoreCheck(operationID: operationID)
        }

        func startGetCapture(operationID: UInt64, captureID: String) throws {
            try core.startGetCapture(operationID: operationID, captureID: captureID)
        }

        func startItemStatus(operationID: UInt64, itemID: String) throws {
            try core.startItemStatus(operationID: operationID, itemID: itemID)
        }

        func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {
            try core.startRetrieval(operationID: operationID, request: request)
        }
    }

    /// One app launch: a real core, a fresh (capture-only) session and the gate in front of the core's read verbs.
    private final class Launch {
        let core: CoreHandle
        let boundary = FakeReadAuthenticationBoundary()
        let session: ReadAuthenticationSession
        let gate: PrivateReadGate
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String, authorizedScopes: [CoreExportScope] = [.personal, .work]) throws {
            core = try CoreHandle(path: path)
            session = ReadAuthenticationSession(boundary: boundary)
            gate = PrivateReadGate(
                session: session, source: CoreHandleReadSource(core: core), authorizedScopes: authorizedScopes)
            try core.setEventHandler { [unowned self] event in
                self.events.append(event)
            }
        }

        deinit { core.close() }

        func close() { core.close() }

        func allocateOperationID() -> UInt64 {
            defer { nextOperationID += 1 }
            return nextOperationID
        }

        func waitForEvent(_ operationID: UInt64, timeout: TimeInterval = 20) throws -> CoreEvent {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if let event = events.first(where: { $0.operationID == operationID }) { return event }
                RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
            }
            throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no event for the operation")
        }

        /// Capture is deliberately not gated: it needs no read access.
        func save(_ capture: CaptureRecord) throws -> SaveCaptureAcknowledgment {
            let operationID = allocateOperationID()
            try core.startSaveCapture(operationID: operationID, capture: capture)
            return try waitForEvent(operationID).decode(SaveCaptureAcknowledgment.self)
        }

        /// Starts a gated capture read and returns once the core has answered, without admitting the answer.
        func startGatedCaptureRead(_ captureID: String) throws -> CoreEvent {
            let operationID = allocateOperationID()
            try gate.startGetCapture(operationID: operationID, captureID: captureID)
            return try waitForEvent(operationID)
        }

        /// Starts a gated search or list and returns once the core has answered, without admitting the answer.
        func startGatedRetrieval(
            query: String?, filters: PrivateRetrievalFilters = PrivateRetrievalFilters()
        ) throws -> CoreEvent {
            let operationID = allocateOperationID()
            if let query {
                try gate.startSearch(operationID: operationID, query: query, filters: filters)
            } else {
                try gate.startList(operationID: operationID, filters: filters)
            }
            return try waitForEvent(operationID)
        }

        func retrieve(
            query: String?, filters: PrivateRetrievalFilters = PrivateRetrievalFilters()
        ) throws -> PrivateRetrievalAnswer {
            let event = try startGatedRetrieval(query: query, filters: filters)
            try gate.admit(operationID: event.operationID)
            return try event.decode(PrivateRetrievalAnswer.self)
        }

        func readStoreCheck() throws -> StoreCheckReport {
            let operationID = allocateOperationID()
            try gate.startStoreCheck(operationID: operationID)
            let event = try waitForEvent(operationID)
            try gate.admit(operationID: operationID)
            return try event.decode(StoreCheckReport.self)
        }

        func readCapture(_ captureID: String) throws -> CaptureRecord {
            let event = try startGatedCaptureRead(captureID)
            try gate.admit(operationID: event.operationID)
            return try event.decode(CaptureReadout.self).capture
        }
    }

    private func assertDenied(
        _ launch: Launch, captureID: String, as expected: PrivateReadDenied = .sessionNotAuthenticated,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        let eventsBefore = launch.events.count
        XCTAssertThrowsError(
            try launch.gate.startGetCapture(operationID: launch.allocateOperationID(), captureID: captureID),
            file: file, line: line
        ) { error in
            XCTAssertEqual(error as? PrivateReadDenied, expected, file: file, line: line)
        }
        XCTAssertThrowsError(
            try launch.gate.startItemStatus(operationID: launch.allocateOperationID(), itemID: "item-\(captureID)"),
            file: file, line: line
        ) { error in
            XCTAssertEqual(error as? PrivateReadDenied, expected, file: file, line: line)
        }
        XCTAssertThrowsError(
            try launch.gate.startStoreCheck(operationID: launch.allocateOperationID()), file: file, line: line
        ) { error in
            XCTAssertEqual(error as? PrivateReadDenied, expected, file: file, line: line)
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertEqual(launch.events.count, eventsBefore, "the core was never asked, so it never answered", file: file, line: line)
    }

    func testPrivateReadsAreDeniedBeforeAuthenticationWhileCaptureStillSucceeds() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        XCTAssertEqual(launch.session.scope, .captureOnly)

        let capture = makeCapture(id: "capture-before-auth", text: "synthetic private note")
        XCTAssertFalse(try launch.save(capture).alreadySaved, "capture works without any read access")

        assertDenied(launch, captureID: capture.captureID)
        XCTAssertEqual(launch.boundary.authenticateCallCount, 0, "denial did not need, or trigger, an authentication")

        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readCapture(capture.captureID), capture, "the capture saved before authentication is intact")
    }

    func testStoreCheckIsDeniedBeforeAuthenticationAndAfterRelockButCapturesAreCounted() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        _ = try launch.save(makeCapture(id: "capture-counted", text: "synthetic note behind the store check"))

        let eventsBefore = launch.events.count
        XCTAssertThrowsError(try launch.readStoreCheck()) { error in
            XCTAssertEqual(error as? PrivateReadDenied, .sessionNotAuthenticated)
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertEqual(launch.events.count, eventsBefore, "the denied store check never reached the core")
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readStoreCheck().captureCount, 1)

        launch.session.relock(.enteredBackground)
        XCTAssertThrowsError(try launch.readStoreCheck()) { error in
            XCTAssertEqual(error as? PrivateReadDenied, .sessionNotAuthenticated)
        }

        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        let operationID = launch.allocateOperationID()
        try launch.gate.startStoreCheck(operationID: operationID)
        launch.session.relock(.willEnterForeground)
        let lateEvent = try launch.waitForEvent(operationID)
        XCTAssertThrowsError(try launch.gate.admit(operationID: lateEvent.operationID)) { error in
            XCTAssertEqual(error as? PrivateReadDenied, .relockedBeforeDelivery)
        }
    }

    private func seedRoutes() {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        sqlite3_busy_timeout(database, 5_000)
        defer { sqlite3_close(database) }
        for scope in ["personal", "work"] {
            let statement = "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-\(scope)', 'route-\(scope)', '\(scope)', '[]', '2026-10-08T09:00:00Z')"
            XCTAssertEqual(sqlite3_exec(database, statement, nil, nil, nil), SQLITE_OK)
        }
    }

    func testRetrievalIsDeniedBeforeAuthenticationAndAfterRelockWithoutReachingTheCore() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        _ = try launch.save(makeCapture(id: "capture-retrieval-denied", text: "synthetic note behind retrieval"))
        let eventsBefore = launch.events.count

        func assertRetrievalDenied(file: StaticString = #filePath, line: UInt = #line) {
            XCTAssertThrowsError(
                try launch.gate.startSearch(
                    operationID: launch.allocateOperationID(), query: "synthetic", filters: PrivateRetrievalFilters()),
                file: file, line: line
            ) { XCTAssertEqual($0 as? PrivateReadDenied, .sessionNotAuthenticated, file: file, line: line) }
            XCTAssertThrowsError(
                try launch.gate.startList(operationID: launch.allocateOperationID(), filters: PrivateRetrievalFilters()),
                file: file, line: line
            ) { XCTAssertEqual($0 as? PrivateReadDenied, .sessionNotAuthenticated, file: file, line: line) }
        }
        assertRetrievalDenied()
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.retrieve(query: "synthetic").hits.map(\.text), ["synthetic note behind retrieval"])
        let eventsAfterRead = launch.events.count
        launch.session.relock(.enteredBackground)
        assertRetrievalDenied()
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertGreaterThan(eventsAfterRead, eventsBefore)
        XCTAssertEqual(launch.events.count, eventsAfterRead, "denied retrievals never reached the core")
    }

    func testRetrievalInFlightWhenTheSessionRelocksIsNotDelivered() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        _ = try launch.save(makeCapture(id: "capture-retrieval-late", text: "synthetic note read during a relock"))
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)

        let operationID = launch.allocateOperationID()
        try launch.gate.startSearch(operationID: operationID, query: "synthetic", filters: PrivateRetrievalFilters())
        launch.session.relock(.enteredBackground)
        let lateEvent = try launch.waitForEvent(operationID)
        XCTAssertThrowsError(try launch.gate.admit(operationID: lateEvent.operationID)) { error in
            XCTAssertEqual(error as? PrivateReadDenied, .relockedBeforeDelivery)
        }
    }

    func testRetrievalNeverAsksForMoreThanTheSessionsAuthorizedScopes() throws {
        let launch = try Launch(path: layout.databaseURL.path, authorizedScopes: [.personal])
        defer { launch.close() }
        seedRoutes()
        _ = try launch.save(
            makeCapture(id: "capture-scope-personal", text: "synthetic zebrafinch personal", routeID: "route-personal"))
        _ = try launch.save(
            makeCapture(
                id: "capture-scope-work", text: "synthetic zebrafinch work", itemScope: "work", routeID: "route-work"))
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)

        let everything = PrivateRetrievalFilters(scopes: [.personal, .work])
        for answer in [
            try launch.retrieve(query: "zebrafinch", filters: everything),
            try launch.retrieve(query: nil, filters: everything),
            try launch.retrieve(query: "zebrafinch"),
        ] {
            XCTAssertEqual(answer.totalAccessible, 1, "the work item is outside the session's scopes")
            XCTAssertEqual(answer.hits.map(\.text), ["synthetic zebrafinch personal"])
        }

        XCTAssertThrowsError(
            try launch.gate.startSearch(
                operationID: launch.allocateOperationID(), query: "zebrafinch",
                filters: PrivateRetrievalFilters(scopes: [.work]))
        ) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request", "no authorized scope is left to read")
        }
    }

    func testRelockDeniesNewReadsAndAuthenticatingAgainRestoresThemWithSourceIntact() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        let capture = makeCapture(id: "capture-relock", text: "synthetic note kept across relock")
        _ = try launch.save(capture)
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readCapture(capture.captureID), capture)

        launch.session.relock(.enteredBackground)

        assertDenied(launch, captureID: capture.captureID)
        XCTAssertTrue(FileManager.default.fileExists(atPath: layout.databaseURL.path), "relock removed nothing")
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readCapture(capture.captureID), capture, "the source survived the relock unchanged")
    }

    func testReadInFlightWhenTheSessionRelocksIsNotDelivered() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        let capture = makeCapture(id: "capture-in-flight", text: "synthetic note read during a relock")
        _ = try launch.save(capture)
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)

        let operationID = launch.allocateOperationID()
        try launch.gate.startGetCapture(operationID: operationID, captureID: capture.captureID)
        launch.session.relock(.enteredBackground)
        let lateEvent = try launch.waitForEvent(operationID)

        XCTAssertThrowsError(try launch.gate.admit(operationID: lateEvent.operationID)) { error in
            XCTAssertEqual(error as? PrivateReadDenied, .relockedBeforeDelivery)
        }
        XCTAssertThrowsError(try launch.gate.admit(operationID: lateEvent.operationID), "an answer is admitted at most once") { error in
            XCTAssertEqual(error as? PrivateReadDenied, .notAGatedRead)
        }

        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readCapture(capture.captureID), capture)
    }

    func testDeniedCancelledAndUnavailableAuthenticationNeverReadAndNeverDeleteSource() throws {
        let launch = try Launch(path: layout.databaseURL.path)
        defer { launch.close() }
        let capture = makeCapture(id: "capture-failed-auth", text: "synthetic note behind failed authentication")
        _ = try launch.save(capture)

        for outcome in [ReadAuthenticationOutcome.cancelled, .denied, .unavailable] {
            launch.boundary.nextOutcome = outcome
            guard case .notAuthenticated? = authenticateAndWait(launch.session) else {
                return XCTFail("\(outcome) must not authenticate")
            }
            assertDenied(launch, captureID: capture.captureID)
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: layout.databaseURL.path))

        launch.boundary.nextOutcome = .succeeded
        XCTAssertEqual(authenticateAndWait(launch.session), .authenticated)
        XCTAssertEqual(try launch.readCapture(capture.captureID), capture)

        launch.boundary.nextOutcome = .cancelled
        guard case .notAuthenticated? = authenticateAndWait(launch.session) else {
            return XCTFail("a cancelled re-authentication must not stay authenticated")
        }
        assertDenied(launch, captureID: capture.captureID)
    }

    func testRelaunchStartsCaptureOnlyAndStoredCapturesSurvive() throws {
        let capture = makeCapture(id: "capture-relaunch", text: "synthetic note across relaunch")
        let first = try Launch(path: layout.databaseURL.path)
        _ = try first.save(capture)
        XCTAssertEqual(authenticateAndWait(first.session), .authenticated)
        XCTAssertEqual(try first.readCapture(capture.captureID), capture)
        first.close()

        let second = try Launch(path: layout.databaseURL.path)
        defer { second.close() }
        XCTAssertEqual(second.session.scope, .captureOnly, "authentication never carries over to a new launch")
        assertDenied(second, captureID: capture.captureID)

        XCTAssertEqual(authenticateAndWait(second.session), .authenticated)
        XCTAssertEqual(try second.readCapture(capture.captureID), capture)
    }
}
