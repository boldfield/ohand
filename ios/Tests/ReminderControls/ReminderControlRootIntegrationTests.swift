import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: reminder controls never reach a provider.
private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class ControlResultBox<Value>: @unchecked Sendable {
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

/// Drives `ServiceCompositionRoot.reminderControls` into the real Rust core over an on-disk store, and the root's
/// reminder pass into a fake notification centre: reads through the gate, compare-and-set edits, the refusal of a past
/// time, and edit and cancel reaching the notification centre only after a pass.
final class ReminderControlRootIntegrationTests: XCTestCase {
    private static let routeID = "route-reminder-controls"
    private static let captureText = "synthetic capture about the garage"
    private static let day: TimeInterval = 86_400

    private var storageRoot: URL!
    private var center: FakeNotificationCenter!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("reminder-controls-\(UUID().uuidString)", isDirectory: true)
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

    // MARK: tests

    func testAnEditAndACancelReachTheNotificationCentreOnlyThroughTheNextReminderPass() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.reminderControls
        let itemID = try saveActionItem(root)

        let empty = try run { try await controls.reminder(itemID: itemID) }
        XCTAssertNil(empty.reminder, "a new action has no reminder")
        XCTAssertEqual(empty.itemType, "action")

        let firstDue = Date().addingTimeInterval(2 * Self.day)
        let created = try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: empty.itemRevision, expectedReminderVersion: 0,
            instant: firstDue, timeZoneID: "UTC") }
        XCTAssertFalse(created.replayed)
        XCTAssertEqual(created.reminder.requestState, "resolved")
        XCTAssertEqual(created.reminder.timezoneID, "UTC")
        XCTAssertEqual(wholeSeconds(created.reminder.resolvedDate), wholeSeconds(firstDue))
        try runReminderPass(root)
        let firstIdentifier = "\(created.reminder.reminderID)#\(created.reminder.scheduleGeneration)"
        XCTAssertEqual(try pendingIdentifiers(), [firstIdentifier], "the pass installs the edited reminder")

        let secondDue = Date().addingTimeInterval(5 * Self.day)
        let moved = try run { try await controls.correctTime(
            commandID: "cmd-2", itemID: itemID, expectedRevision: empty.itemRevision,
            expectedReminderVersion: created.reminder.stateVersion, instant: secondDue, timeZoneID: "UTC") }
        XCTAssertGreaterThan(moved.reminder.scheduleGeneration, created.reminder.scheduleGeneration)
        XCTAssertEqual(wholeSeconds(moved.reminder.resolvedDate), wholeSeconds(secondDue))
        try runReminderPass(root)
        let secondIdentifier = "\(moved.reminder.reminderID)#\(moved.reminder.scheduleGeneration)"
        XCTAssertEqual(try pendingIdentifiers(), [secondIdentifier], "the new generation replaces the old one")
        let installed = try XCTUnwrap(center.addedRequests.last)
        XCTAssertEqual(wholeSeconds(installed.dueInstant), wholeSeconds(secondDue))

        let read = try run { try await controls.reminder(itemID: itemID) }
        XCTAssertEqual(read.reminder?.stateVersion, moved.reminder.stateVersion)
        XCTAssertEqual(wholeSeconds(read.reminder?.resolvedDate), wholeSeconds(secondDue))

        let cancelled = try run { try await controls.cancel(
            commandID: "cmd-3", itemID: itemID, expectedRevision: empty.itemRevision,
            expectedReminderVersion: moved.reminder.stateVersion) }
        XCTAssertEqual(cancelled.reminder.requestState, "cancelled")
        try runReminderPass(root)
        XCTAssertEqual(try pendingIdentifiers(), [], "the pass removes the cancelled reminder")
        let afterCancel = try run { try await controls.reminder(itemID: itemID) }
        XCTAssertEqual(afterCancel.reminder?.requestState, "cancelled")
        XCTAssertEqual(afterCancel.lifecycleState, "active", "cancelling a reminder keeps the item")
    }

    func testAStaleVersionAndAPastTimeAreRefusedAndNothingChanges() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.reminderControls
        let itemID = try saveActionItem(root)
        let start = try run { try await controls.reminder(itemID: itemID) }
        let due = Date().addingTimeInterval(2 * Self.day)
        let created = try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: due, timeZoneID: "UTC") }

        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-2", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: due.addingTimeInterval(Self.day), timeZoneID: "UTC") }) { error in
            XCTAssertEqual(
                error as? ReminderControlError, .staleReminderVersion(currentVersion: created.reminder.stateVersion))
        }
        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-3", itemID: itemID, expectedRevision: start.itemRevision + 5,
            expectedReminderVersion: created.reminder.stateVersion, instant: due.addingTimeInterval(Self.day),
            timeZoneID: "UTC") }) { error in
            XCTAssertEqual(error as? ReminderControlError, .staleRevision(currentRevision: start.itemRevision))
        }
        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-4", itemID: itemID, expectedRevision: start.itemRevision,
            expectedReminderVersion: created.reminder.stateVersion, instant: Date().addingTimeInterval(-Self.day),
            timeZoneID: "UTC") }) { error in
            XCTAssertEqual(error as? ReminderControlError, .timeInPast)
        }

        let after = try run { try await controls.reminder(itemID: itemID) }
        XCTAssertEqual(after.reminder, created.reminder, "refusals change nothing and substitute no other time")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminder_commands"), "1")
    }

    func testARepeatedEditReportsTheFirstOutcomeAndHistoryRecordsTheRequest() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.reminderControls
        let itemID = try saveActionItem(root)
        let start = try run { try await controls.reminder(itemID: itemID) }
        let due = Date().addingTimeInterval(2 * Self.day)

        let first = try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: due, timeZoneID: "UTC") }
        let repeated = try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: due, timeZoneID: "UTC") }
        XCTAssertTrue(repeated.replayed)
        XCTAssertEqual(repeated.reminder.stateVersion, first.reminder.stateVersion)
        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: due.addingTimeInterval(Self.day), timeZoneID: "UTC") }) { error in
            XCTAssertEqual(error as? ReminderControlError, .commandConflict)
        }

        let history = try run { try await controls.history(itemID: itemID) }
        XCTAssertEqual(history.reminderID, first.reminder.reminderID)
        XCTAssertEqual(history.entries.first?.kind, "time_requested")
        XCTAssertEqual(history.summary?.due, "upcoming")
        XCTAssertFalse(history.truncated)
    }

    func testACaptureOnlySessionCannotReadOrWriteAndNothingChanges() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session)
        defer { root.shutDown() }
        let controls = root.reminderControls
        let itemID = try saveActionItem(root)
        let start = try run { try await controls.reminder(itemID: itemID) }
        session.relock(.explicit)
        XCTAssertNotEqual(session.scope, .authenticated)

        XCTAssertThrowsError(try run { try await controls.reminder(itemID: itemID) }) { error in
            XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await controls.history(itemID: itemID) }) { error in
            XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: start.itemRevision, expectedReminderVersion: 0,
            instant: Date().addingTimeInterval(2 * Self.day), timeZoneID: "UTC") }) { error in
            XCTAssertEqual(error as? ReminderControlError, .notAuthenticated)
        }
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminders"), "0")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminder_commands"), "0")
    }

    func testAnItemThatIsNotAnActionCannotCarryAReminderAndAnUnknownItemIsNotFound() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.reminderControls
        let itemID = try saveCapture(root)
        let corrections = root.itemCorrections
        let start = try revision(of: itemID)
        let correctedType = try run { try await corrections.correctType(
            commandID: "cmd-type", itemID: itemID, expectedRevision: start, itemType: .note) }

        XCTAssertThrowsError(try run { try await controls.correctTime(
            commandID: "cmd-1", itemID: itemID, expectedRevision: correctedType.revision, expectedReminderVersion: 0,
            instant: Date().addingTimeInterval(2 * Self.day), timeZoneID: "UTC") }) { error in
            XCTAssertEqual(error as? ReminderControlError, .itemCannotCarryReminder)
        }
        XCTAssertThrowsError(try run { try await controls.reminder(itemID: UUID().uuidString.lowercased()) }) { error in
            XCTAssertEqual(error as? ReminderControlError, .itemNotFound)
        }
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminders"), "0")
    }

    func testTheReminderControlRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.reminderControlClientName })
        XCTAssertEqual(mine.start, CoreReminderControlClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func wholeSeconds(_ date: Date?) -> Int? {
        date.map { Int($0.timeIntervalSince1970.rounded(.down)) }
    }

    private func makeRoot(authenticated: Bool) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return try makeRoot(session: session)
    }

    private func makeRoot(session: ReadAuthenticationSession) throws -> ServiceCompositionRoot {
        try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true)))
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

    /// Saves a capture and corrects it to an action, the only item type that carries a reminder.
    private func saveActionItem(_ root: ServiceCompositionRoot) throws -> String {
        let itemID = try saveCapture(root)
        let corrections = root.itemCorrections
        let start = try revision(of: itemID)
        _ = try run { try await corrections.correctType(
            commandID: "cmd-action-\(itemID)", itemID: itemID, expectedRevision: start, itemType: .action) }
        return itemID
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
    }

    private func runReminderPass(_ root: ServiceCompositionRoot) throws {
        try run { _ = try await root.reconcileReminders() }
    }

    private func pendingIdentifiers() throws -> [String] {
        let center = self.center!
        return try run { try await center.pendingRequests().map(\.identifier).sorted() }
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
        let box = ControlResultBox<Value>()
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
