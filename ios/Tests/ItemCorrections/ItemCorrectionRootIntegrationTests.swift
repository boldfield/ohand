import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: corrections never reach a provider.
private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class ChangeLog: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [ItemChange] = []

    var changes: [ItemChange] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func append(_ change: ItemChange) {
        lock.lock()
        stored.append(change)
        lock.unlock()
    }
}

private final class CorrectionResultBox<Value>: @unchecked Sendable {
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

/// Drives `ServiceCompositionRoot.itemCorrections` into the real Rust core over an on-disk store: the root's assembly
/// (client, router registration, operation range) and the core's compare-and-set, replay and capture-only rules.
final class ItemCorrectionRootIntegrationTests: XCTestCase {
    private static let routeID = "route-item-corrections"
    private static let captureText = "synthetic capture for corrections"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-corrections-\(UUID().uuidString)", isDirectory: true)
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

    func testCorrectionsAreStoredThroughTheEventPathAndLeaveTheCaptureAlone() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)

        let text = try run { try await corrections.correctText(
            commandID: "cmd-text", itemID: itemID, expectedRevision: start, text: "call the roofer tomorrow") }
        XCTAssertEqual(text, ItemCorrectionReceipt(itemID: itemID, revision: start + 1, replayed: false, reminderCancelled: false))
        let type = try run { try await corrections.correctType(
            commandID: "cmd-type", itemID: itemID, expectedRevision: start + 1, itemType: .idea) }
        XCTAssertEqual(type.revision, start + 2)
        let topic = try run { try await corrections.correctSessionTopic(
            commandID: "cmd-topic", itemID: itemID, expectedRevision: start + 2, sessionTopic: "garage") }
        XCTAssertEqual(topic.revision, start + 3)

        XCTAssertEqual(try revision(of: itemID), start + 3)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'user-correction:%'"), "3")
        XCTAssertEqual(
            scalar("SELECT text FROM captures WHERE capture_id = (SELECT capture_id FROM items WHERE item_id = '\(itemID)')"),
            Self.captureText, "the original capture is never altered")
    }

    func testAStaleRevisionIsRefusedWithTheCurrentRevision() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        _ = try run { try await corrections.correctText(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, text: "first correction") }

        XCTAssertThrowsError(try run { try await corrections.correctText(
            commandID: "cmd-2", itemID: itemID, expectedRevision: start, text: "second correction") }) { error in
            XCTAssertEqual(error as? ItemCorrectionError, .staleRevision(currentRevision: start + 1))
        }
        XCTAssertEqual(try revision(of: itemID), start + 1, "a refused correction writes nothing")
    }

    func testAStoredCorrectionSignalsTheChangeAndAStaleOneDoesNot() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        let seen = ChangeLog()
        let remove = root.observeItemChanges { seen.append($0) }
        defer { remove() }

        _ = try run { try await corrections.correctText(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, text: "call the roofer tomorrow") }
        XCTAssertTrue(pump(until: { !seen.changes.isEmpty }), "the signal reaches the main thread")
        XCTAssertThrowsError(try run { try await corrections.correctText(
            commandID: "cmd-2", itemID: itemID, expectedRevision: start, text: "stale") })
        _ = pump(until: { false }, timeout: 0.3)
        XCTAssertEqual(seen.changes, [.item(itemID)], "only the stored correction published, and only the identifier")
    }

    func testARepeatedCommandReportsTheFirstOutcomeAndAChangedOneConflicts() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        let first = try run { try await corrections.correctSessionTopic(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, sessionTopic: "garage") }
        let eventCount = scalar("SELECT COUNT(*) FROM events")

        let repeated = try run { try await corrections.correctSessionTopic(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, sessionTopic: "garage") }
        XCTAssertEqual(repeated.revision, first.revision)
        XCTAssertTrue(repeated.replayed)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events"), eventCount, "a replay writes nothing")
        XCTAssertEqual(try revision(of: itemID), start + 1)

        XCTAssertThrowsError(try run { try await corrections.correctSessionTopic(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, sessionTopic: "kitchen") }) { error in
            XCTAssertEqual(error as? ItemCorrectionError, .commandConflict)
        }
        XCTAssertEqual(try revision(of: itemID), start + 1)
    }

    func testAnUnknownItemIsNotFound() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        XCTAssertThrowsError(try run { try await corrections.correctText(
            commandID: "cmd-1", itemID: UUID().uuidString.lowercased(), expectedRevision: 0, text: "x") }) { error in
            XCTAssertEqual(error as? ItemCorrectionError, .itemNotFound)
        }
    }

    func testACaptureOnlySessionCannotCorrectAndNothingChanges() throws {
        let root = try makeRoot(authenticated: false)
        defer { root.shutDown() }
        let corrections = root.itemCorrections
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        XCTAssertThrowsError(try run { try await corrections.correctText(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start, text: "not allowed") }) { error in
            XCTAssertEqual(error as? ItemCorrectionError, .notAuthenticated)
        }
        XCTAssertEqual(try revision(of: itemID), start)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'user-correction:%'"), "0")
    }

    func testTheCorrectionRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.itemCorrectionClientName })
        XCTAssertEqual(mine.start, CoreItemCorrectionClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func makeRoot(authenticated: Bool) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(), readSession: session,
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

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
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
        let box = CorrectionResultBox<Value>()
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
