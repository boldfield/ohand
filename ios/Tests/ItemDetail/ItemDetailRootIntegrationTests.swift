import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class DetailUnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class DetailResultBox<Value>: @unchecked Sendable {
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

private final class DetailResultList: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [Result<CoreItemDetail, CoreFailure>] = []

    func append(_ value: Result<CoreItemDetail, CoreFailure>) {
        lock.lock()
        stored.append(value)
        lock.unlock()
    }

    var values: [Result<CoreItemDetail, CoreFailure>] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }
}

/// Drives `ServiceCompositionRoot.itemDetails` into the real Rust core over an on-disk store: the root's assembly
/// (gated client, router registration, operation range), the read gate and the core's detail read.
final class ItemDetailRootIntegrationTests: XCTestCase {
    private static let routeID = "route-item-detail"
    private static let captureText = "synthetic capture for the detail read"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-detail-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testASeededItemIsReadEndToEndThroughTheRoot() throws {
        let root = try makeRoot(session: authenticatedSession())
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let details = root.itemDetails

        let detail = try run { try await details.detail(itemID: itemID) }

        XCTAssertEqual(detail.itemID, itemID)
        XCTAssertEqual(detail.text, Self.captureText)
        XCTAssertEqual(detail.textBasis, .original)
        XCTAssertEqual(detail.originalText, Self.captureText)
        XCTAssertNil(detail.correctedText)
        XCTAssertEqual(detail.scope, "personal")
        XCTAssertEqual(detail.lifecycleState, "active")
        XCTAssertEqual(detail.revision, Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        XCTAssertNil(detail.audioReference)
        XCTAssertNil(detail.transcript)
        XCTAssertFalse(detail.capturedAt.isEmpty)
        XCTAssertFalse(detail.historyTruncated)
    }

    func testACorrectionShowsAsTheCurrentTextBesideTheOriginalAndInTheHistory() throws {
        let root = try makeRoot(session: authenticatedSession())
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let corrections = root.itemCorrections
        let details = root.itemDetails
        let start = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        _ = try run { try await corrections.correctText(
            commandID: "cmd-text", itemID: itemID, expectedRevision: start, text: "corrected synthetic text") }

        let detail = try run { try await details.detail(itemID: itemID) }

        XCTAssertEqual(detail.text, "corrected synthetic text")
        XCTAssertEqual(detail.textBasis, .corrected)
        XCTAssertEqual(detail.originalText, Self.captureText)
        XCTAssertEqual(detail.correctedText, "corrected synthetic text")
        XCTAssertEqual(detail.history.count, 1)
        XCTAssertEqual(detail.history.first?.eventType, "correction")
        XCTAssertEqual(detail.history.first?.correction?.kind, "text")
        XCTAssertEqual(detail.history.first?.correction?.newValue, "corrected synthetic text")
    }

    func testAnUnknownItemIsTheFixedNotFound() throws {
        let root = try makeRoot(session: authenticatedSession())
        defer { root.shutDown() }
        let details = root.itemDetails

        XCTAssertThrowsError(try run { try await details.detail(itemID: UUID().uuidString.lowercased()) }) { error in
            XCTAssertEqual(error as? ItemDetailError, .notFound)
        }
    }

    func testACaptureOnlySessionIsRefusedAndTheCoreIsNeverAsked() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let root = try makeRoot(session: session)
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let details = root.itemDetails

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertThrowsError(try run { try await details.detail(itemID: itemID) }) { error in
            XCTAssertEqual(error as? ItemDetailError, .notAuthenticated)
        }

        session.authenticate(reason: "test") { _ in }
        let detail = try run { try await details.detail(itemID: itemID) }
        XCTAssertEqual(detail.text, Self.captureText, "the same item reads once the session is authenticated")

        session.relock(.enteredBackground)
        XCTAssertThrowsError(try run { try await details.detail(itemID: itemID) }) { error in
            XCTAssertEqual(error as? ItemDetailError, .notAuthenticated)
        }
    }

    func testAnAnswerThatArrivesAfterARelockIsDiscardedBeforeItIsDecoded() throws {
        let seedingRoot = try makeRoot(session: authenticatedSession())
        let itemID = try saveCapture(seedingRoot)
        seedingRoot.shutDown()

        let session = authenticatedSession()
        let core = try CoreHandle(path: databasePath)
        defer { core.close() }
        let gate = PrivateReadGate(session: session, source: ItemDetailCoreSource(core: core))
        let client = CoreItemDetailClient(gate: gate)
        var held: [CoreEvent] = []
        try core.setEventHandler { held.append($0) }

        let outcomes = DetailResultList()
        client.read(itemID: itemID) { outcomes.append($0) }
        client.read(itemID: "unknown-item") { outcomes.append($0) }
        session.relock(.enteredBackground)
        XCTAssertTrue(pump(until: { held.count == 2 }), "the core answers both reads")
        for event in held {
            XCTAssertTrue(client.handle(event), "the client claims its own answers")
        }

        XCTAssertEqual(outcomes.values.count, 2)
        for outcome in outcomes.values {
            guard case .failure(let refusal) = outcome else { return XCTFail("the answer must not be delivered") }
            XCTAssertEqual(refusal.code, "read_relocked", "neither the item nor its absence is revealed")
        }
        client.shutDown()
    }

    func testTheItemDetailRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.itemDetailClientName })
        XCTAssertEqual(mine.start, CoreItemDetailClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private final class ItemDetailCoreSource: PrivateReadSource {
        private let core: CoreHandle
        init(core: CoreHandle) { self.core = core }

        func startStoreCheck(operationID: UInt64) throws { try core.startStoreCheck(operationID: operationID) }
        func startGetCapture(operationID: UInt64, captureID: String) throws {
            try core.startGetCapture(operationID: operationID, captureID: captureID)
        }
        func startItemStatus(operationID: UInt64, itemID: String) throws {
            try core.startItemStatus(operationID: operationID, itemID: itemID)
        }
        func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {
            try core.startRetrieval(operationID: operationID, request: request)
        }
        func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {
            try core.startItemDetail(operationID: operationID, itemID: itemID, readScopes: readScopes)
        }
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func authenticatedSession() -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        return session
    }

    private func makeRoot(session: ReadAuthenticationSession) throws -> ServiceCompositionRoot {
        try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: DetailUnreachableProviderSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)))
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.captureText, capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = DetailResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
