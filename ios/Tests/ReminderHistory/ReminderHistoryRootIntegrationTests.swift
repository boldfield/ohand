import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class UnreachableHistorySender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Reads reminder history from the real Rust core through the real composition root.
final class ReminderHistoryRootIntegrationTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("reminder-history-\(UUID().uuidString)", isDirectory: true)
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

    @MainActor func testSavedTimeAppearsInHistoryAndReentryAfterALapseShowsTheSameListWithoutADigest() async throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-reminder-history", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableHistorySender(), readSession: session, notificationCenter: FakeNotificationCenter(),
                reachability: FakeReachability(reachable: true)))
        defer { root.shutDown() }

        let itemID: String = try await withCheckedThrowingContinuation { continuation in
            root.saveText(captureID: UUID().uuidString.lowercased(), text: "synthetic capture about the garage", capturedAt: Date()) {
                outcome in
                if case .saved(_, let itemID, _) = outcome {
                    continuation.resume(returning: itemID)
                } else {
                    continuation.resume(throwing: CoreFailure.handleClosed)
                }
            }
        }
        let revision = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        _ = try await root.itemCorrections.correctType(
            commandID: "cmd-action-\(itemID)", itemID: itemID, expectedRevision: revision, itemType: .action)

        let edit = ReminderEditModel.assembled(itemID: itemID, root: root)
        await edit.load()
        edit.draftDate = Date().addingTimeInterval(2 * 86_400)
        edit.draftTimeZoneID = "UTC"
        await edit.save()
        XCTAssertEqual(edit.message, .saved)

        let history = ReminderHistoryModel(itemID: itemID, reader: root.reminderControls)
        await history.load()
        XCTAssertNil(history.unavailable)
        XCTAssertTrue(history.rows.contains { $0.title == "Time saved" }, "\(history.rows)")
        let before = history.rows

        history.lock()
        XCTAssertTrue(history.rows.isEmpty)
        XCTAssertEqual(history.unavailable, .locked)

        await history.load()
        XCTAssertEqual(history.rows, before, "re-entry shows the stored list, not a replay")
        let text = (history.rows.flatMap { [$0.title, $0.detail ?? ""] } + history.summaryLines).joined(separator: " ").lowercased()
        XCTAssertFalse(text.contains("digest"))
        XCTAssertFalse(text.contains("while you were away"))
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
