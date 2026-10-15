import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: deletion never reaches a provider.
private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Drives `ServiceCompositionRoot.itemDeletion` into the real Rust core over an on-disk store with the real
/// `LifecycleCoordinator` and its deletion cleanup service: the intent the core records, the native effects the cleanup
/// removes (the pending notification and the retained audio) and the progress the core reports.
final class ItemDeletionRootIntegrationTests: XCTestCase {
    private static let routeID = "route-item-deletion"
    private static let captureText = "synthetic capture to delete"
    private static let reminderID = "rem-1"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-deletion-\(UUID().uuidString)", isDirectory: true)
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

    func testDeletionRemovesThePendingNotificationAndRetainedAudioAndReportsProgressThroughToCompletion() throws {
        let center = FakeNotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator], center: center)
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudioAndReminder(itemID: itemID, captureID: captureID, center: center)
        let start = try revision(of: itemID)
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
        XCTAssertEqual(center.pendingIdentifiers, ["\(Self.reminderID)#1"])
        XCTAssertEqual(try run { try await root.itemDeletion.progress(itemID: itemID) }, .notRequested)

        let receipt = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }

        XCTAssertEqual(receipt.itemID, itemID)
        XCTAssertEqual(receipt.deletionRevision, start)
        XCTAssertFalse(receipt.replayed)
        XCTAssertEqual(receipt.progress, .complete)
        XCTAssertTrue(receipt.cleanupSettled)
        XCTAssertEqual(try run { try await root.itemDeletion.progress(itemID: itemID) }, .complete)
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "deleted")
        XCTAssertEqual(center.pendingIdentifiers, [], "the item's pending notification was removed")
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path), "the retained audio was removed")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM deletion_work WHERE item_id = '\(itemID)' AND status != 'completed'"), "0")
        XCTAssertEqual(scalar("SELECT COALESCE(audio_reference, '') FROM captures WHERE capture_id = '\(captureID)'"), "")
        XCTAssertEqual(scalar("SELECT text FROM captures WHERE capture_id = '\(captureID)'"), "", "the text is erased")
    }

    func testAnIntentWhoseCleanupCouldNotRunIsFinishedByTheNormalLifecyclePass() async throws {
        let center = FakeNotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator], center: center)
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudioAndReminder(itemID: itemID, captureID: captureID, center: center)
        let start = try revision(of: itemID)

        let receipt = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }

        XCTAssertFalse(receipt.cleanupSettled, "the coordinator is not connected, so no cleanup ran")
        XCTAssertEqual(
            receipt.progress, .inProgress(pending: [.cancelNotifications, .clearIngress, .removeAudio], failed: []))
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "deleted")
        XCTAssertEqual(center.pendingIdentifiers, ["\(Self.reminderID)#1"], "the native effects are still to do")
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))

        try coordinator.connect(to: root)
        root.setAppActiveForTests(true)
        let pass = await coordinator.reconcile(.foreground)
        XCTAssertEqual(pass.outcome(of: .deletion), .completed)

        XCTAssertEqual(try run { try await root.itemDeletion.progress(itemID: itemID) }, .complete)
        XCTAssertEqual(center.pendingIdentifiers, [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path))

        let repeated = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }
        XCTAssertTrue(repeated.replayed, "asking again reports the same intent")
        XCTAssertEqual(repeated.progress, .complete)
    }

    func testAStaleRevisionDeletesNothing() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, _) = try saveCapture(root)
        let start = try revision(of: itemID)

        XCTAssertThrowsError(try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start + 3) }) {
            XCTAssertEqual($0 as? ItemDeletionError, .staleRevision(currentRevision: start))
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(try run { try await root.itemDeletion.progress(itemID: itemID) }, .notRequested)
    }

    func testAnUnknownItemIsNotFound() throws {
        let root = try makeRoot(authenticated: true, modules: [])
        defer { root.shutDown() }
        XCTAssertThrowsError(try run { try await root.itemDeletion.delete(itemID: "no-such-item", expectedRevision: 0) }) {
            XCTAssertEqual($0 as? ItemDeletionError, .itemNotFound)
        }
        XCTAssertThrowsError(try run { try await root.itemDeletion.progress(itemID: "no-such-item") }) {
            XCTAssertEqual($0 as? ItemDeletionError, .itemNotFound)
        }
    }

    func testACaptureOnlySessionCannotDeleteAndNothingChanges() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: false, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, _) = try saveCapture(root)
        let start = try revision(of: itemID)

        XCTAssertThrowsError(try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }) {
            XCTAssertEqual($0 as? ItemDeletionError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await root.itemDeletion.progress(itemID: itemID) }) {
            XCTAssertEqual($0 as? ItemDeletionError, .notAuthenticated)
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM deletion_work"), "0")
    }

    func testTheDeletionRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.itemDeletionClientName })
        XCTAssertEqual(mine.start, CoreItemDeletionClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private var layout: ProtectedStorageLayout { ProtectedStorageLayout(rootDirectory: storageRoot) }

    private var databasePath: String { layout.databaseURL.path }

    private func makeRoot(
        authenticated: Bool, modules: [CompositionModule], center: FakeNotificationCenter = FakeNotificationCenter()
    ) throws -> ServiceCompositionRoot {
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
                sender: UnreachableProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true)),
            modules: modules)
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> (itemID: String, captureID: String) {
        let captureID = UUID().uuidString.lowercased()
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: captureID, text: Self.captureText, capturedAt: Date()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return (itemID, captureID)
    }

    /// Gives the saved item retained audio and a scheduled reminder whose notification the OS still holds, which is
    /// what a voice capture with a reminder looks like. Returns the audio file.
    private func seedAudioAndReminder(
        itemID: String, captureID: String, center: FakeNotificationCenter
    ) throws -> URL {
        let finalizedName = "\(captureID).m4a"
        let audioURL = layout.directory(for: .finalizedAudio).appendingPathComponent(finalizedName)
        try FileManager.default.createDirectory(
            at: audioURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("synthetic audio".utf8).write(to: audioURL)
        let reference = IngressRecord.audioReference(finalizedFileName: finalizedName)
        try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'")
        try execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(Self.reminderID)', '\(itemID)', 'resolved', 'scheduled', 'unknown', 'not_acknowledged', "
                + "'2090-01-01T09:00:00Z', 'UTC', 1, '2026-10-10T09:31:00Z', '2026-10-10T09:32:00Z')")
        try execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                + "effect_identity, external_id, created_at) VALUES ('\(Self.reminderID)-op-0', '\(Self.reminderID)', "
                + "'schedule', 'acknowledged', 'schedule:\(Self.reminderID)#1', '\(Self.reminderID)#1', "
                + "'2026-10-10T09:31:00Z')")
        center.installForeignRequest(identifier: "\(Self.reminderID)#1", dueInstant: Date().addingTimeInterval(3600))
        return audioURL
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
        let box = DeletionResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func execute(_ sql: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        if code != SQLITE_OK { throw CompositionError.storageUnavailable }
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

private final class DeletionResultBox<Value>: @unchecked Sendable {
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
