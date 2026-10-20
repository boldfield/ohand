import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class UnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Drives `ReminderEditModel` into the real Rust core through the real composition root: successive edits leave one
/// installed request (the newest generation), a cancel removes it, and the stored reminder survives a restart.
final class ReminderEditRootIntegrationTests: XCTestCase {
    private static let routeID = "route-reminder-edit"
    private static let day: TimeInterval = 86_400

    private var storageRoot: URL!
    private var center: FakeNotificationCenter!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("reminder-edit-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        center = FakeNotificationCenter()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    func testEditsLeaveOneInstalledRequestACancelRemovesItAndARestartKeepsTheReminder() async throws {
        let root = try makeRoot()
        let itemID = try await saveActionItem(root)
        let model = ReminderEditModel.assembled(itemID: itemID, root: root)
        await model.load()
        XCTAssertEqual(model.standing, .none)

        model.draftDate = Date().addingTimeInterval(2 * Self.day)
        model.draftTimeZoneID = "UTC"
        await model.save()
        XCTAssertEqual(model.message, .saved)
        let firstPending = try await pendingIdentifiers()
        XCTAssertEqual(firstPending.count, 1)
        XCTAssertEqual(model.standing, .installed, "installed only after the pass the edit asked for")

        model.draftDate = Date().addingTimeInterval(5 * Self.day)
        await model.save()
        let secondPending = try await pendingIdentifiers()
        XCTAssertEqual(secondPending.count, 1, "the earlier request is not left beside the new one")
        XCTAssertNotEqual(secondPending, firstPending)

        root.shutDown()
        let restarted = try makeRoot()
        defer { restarted.shutDown() }
        let reopened = ReminderEditModel.assembled(itemID: itemID, root: restarted)
        await reopened.load()
        XCTAssertEqual(reopened.reminder?.requestState, "resolved")
        XCTAssertEqual(reopened.reminder?.timezoneID, "UTC")

        await reopened.cancel()
        XCTAssertEqual(reopened.standing, .cancelled)
        let afterCancel = try await pendingIdentifiers()
        XCTAssertEqual(afterCancel, [])
    }

    // MARK: helpers

    private func makeRoot() throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true)))
    }

    private func saveActionItem(_ root: ServiceCompositionRoot) async throws -> String {
        let itemID: String = try await withCheckedThrowingContinuation { continuation in
            root.saveText(
                captureID: UUID().uuidString.lowercased(), text: "synthetic capture about the garage",
                capturedAt: Date()
            ) { outcome in
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
        return itemID
    }

    private func pendingIdentifiers() async throws -> [String] {
        try await center.pendingRequests().map(\.identifier).sorted()
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
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
