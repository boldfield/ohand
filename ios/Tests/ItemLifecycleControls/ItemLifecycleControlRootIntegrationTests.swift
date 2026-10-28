import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: lifecycle controls never reach a provider.
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

/// Drives `ServiceCompositionRoot.itemLifecycleControls` into the real Rust core over an on-disk store: the root's
/// assembly (client, router registration, operation range), the core's compare-and-set, replay and capture-only rules,
/// and the reminder pass that removes a completed item's notification.
///
/// The service never holds the notification center, so a control cannot reach the OS directly; the tests show the
/// notification disappears only once `reconcileReminders` runs. Reminder rows are seeded into the open store because
/// no export creates reminders for a plain capture; every later state is produced by the real core and pass.
final class ItemLifecycleControlRootIntegrationTests: XCTestCase {
    private static let routeID = "route-item-lifecycle"
    private static let captureText = "synthetic capture for lifecycle controls"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-lifecycle-controls-\(UUID().uuidString)", isDirectory: true)
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

    func testCompletingAnItemWithALiveReminderLeavesNoPendingNotificationAfterThePass() throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(authenticated: true, center: center)
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let reminderID = seedLiveReminder(for: itemID)
        let notificationID = "\(reminderID)#1"

        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, [notificationID], "the reminder pass installed the notification")
        let start = try revision(of: itemID)

        let receipt = try run {
            try await root.itemLifecycleControls.complete(commandID: "cmd-complete", itemID: itemID, expectedRevision: start)
        }
        XCTAssertEqual(receipt, ItemLifecycleControlReceipt(itemID: itemID, revision: start + 1, replayed: false))
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
        XCTAssertEqual(scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "cancelled")

        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, [], "no notification is pending after the pass")
        XCTAssertTrue(center.removedIdentifiers.contains(notificationID))
        XCTAssertEqual(center.addedRequests.count, 1, "the pass installed nothing new")
    }

    func testCancellingAnItemWithALiveReminderLeavesNoPendingNotificationAfterThePass() throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(authenticated: true, center: center)
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let reminderID = seedLiveReminder(for: itemID)
        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, ["\(reminderID)#1"])
        let start = try revision(of: itemID)

        _ = try run {
            try await root.itemLifecycleControls.cancel(commandID: "cmd-cancel", itemID: itemID, expectedRevision: start)
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "cancelled")

        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, [])
    }

    func testNotNowAndStopSuggestingQuietTheItemAndLeaveItsReminderInstalled() throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(authenticated: true, center: center)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        let reminderID = seedLiveReminder(for: itemID)
        _ = try run { try await root.reconcileReminders() }
        let start = try revision(of: itemID)

        let snoozed = try run { try await controls.notNow(commandID: "cmd-snooze", itemID: itemID, expectedRevision: start) }
        XCTAssertEqual(snoozed.revision, start + 1)
        XCTAssertEqual(scalar("SELECT snoozed FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "1")

        let muted = try run {
            try await controls.stopSuggesting(commandID: "cmd-mute", itemID: itemID, expectedRevision: start + 1)
        }
        XCTAssertEqual(muted.revision, start + 2)
        XCTAssertEqual(scalar("SELECT pull_only FROM suggestion_eligibility WHERE item_id = '\(itemID)'"), "1")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")

        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, ["\(reminderID)#1"], "optional help never removes an explicit reminder")
    }

    func testAStaleRevisionIsRefusedWithTheCurrentRevisionAndWritesNothing() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        _ = try run { try await controls.notNow(commandID: "cmd-1", itemID: itemID, expectedRevision: start) }

        XCTAssertThrowsError(try run {
            try await controls.complete(commandID: "cmd-2", itemID: itemID, expectedRevision: start)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .staleRevision(currentRevision: start + 1))
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(try revision(of: itemID), start + 1)
    }

    func testARepeatedCommandReportsTheFirstOutcomeAndAChangedOneConflicts() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        let first = try run { try await controls.complete(commandID: "cmd-1", itemID: itemID, expectedRevision: start) }
        let eventCount = scalar("SELECT COUNT(*) FROM events")

        let repeated = try run { try await controls.complete(commandID: "cmd-1", itemID: itemID, expectedRevision: start) }
        XCTAssertEqual(repeated.revision, first.revision)
        XCTAssertTrue(repeated.replayed)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events"), eventCount, "a replay writes nothing")

        XCTAssertThrowsError(try run {
            try await controls.cancel(commandID: "cmd-1", itemID: itemID, expectedRevision: start)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .commandConflict)
        }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
    }

    func testAnEndedAndADeletedItemTakeNoFurtherControl() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        _ = try run { try await controls.complete(commandID: "cmd-1", itemID: itemID, expectedRevision: start) }

        XCTAssertThrowsError(try run {
            try await controls.notNow(commandID: "cmd-2", itemID: itemID, expectedRevision: start + 1)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .itemEnded(lifecycle: "completed"))
        }

        execute("UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(itemID)'")
        XCTAssertThrowsError(try run {
            try await controls.stopSuggesting(commandID: "cmd-3", itemID: itemID, expectedRevision: start + 1)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .itemDeleted)
        }
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE item_id = '\(itemID)'"), "1")
    }

    func testReopeningACompletedItemKeepsItsReminderCancelledAndReturnsTheStoredInstant() throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(authenticated: true, center: center)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        _ = seedLiveReminder(for: itemID)
        let storedInstant = scalar("SELECT resolved_instant FROM reminders WHERE item_id = '\(itemID)'")
        let start = try revision(of: itemID)

        XCTAssertThrowsError(try run {
            try await controls.reopen(commandID: "cmd-early", itemID: itemID, expectedRevision: start)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .alreadyOpen)
        }
        _ = try run { try await controls.complete(commandID: "cmd-1", itemID: itemID, expectedRevision: start) }

        let receipt = try run {
            try await controls.reopen(commandID: "cmd-2", itemID: itemID, expectedRevision: start + 1)
        }
        XCTAssertEqual(
            receipt,
            ItemReopenReceipt(
                itemID: itemID, revision: start + 2, replayed: false, formerReminderInstant: storedInstant))
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "cancelled")

        _ = try run { try await root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers, [], "reopening never reinstalls the reminder")

        let replay = try run {
            try await controls.reopen(commandID: "cmd-2", itemID: itemID, expectedRevision: start + 1)
        }
        XCTAssertEqual(replay.revision, start + 2)
        XCTAssertTrue(replay.replayed)
        XCTAssertEqual(replay.formerReminderInstant, storedInstant)
    }

    func testAnUnknownItemIsNotFound() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        XCTAssertThrowsError(try run {
            try await controls.complete(
                commandID: "cmd-1", itemID: UUID().uuidString.lowercased(), expectedRevision: 0)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .itemNotFound)
        }
    }

    func testACaptureOnlySessionCannotControlAnItemAndNothingChanges() throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(authenticated: false, center: center)
        defer { root.shutDown() }
        let controls = root.itemLifecycleControls
        let itemID = try saveCapture(root)
        let start = try revision(of: itemID)
        XCTAssertThrowsError(try run {
            try await controls.complete(commandID: "cmd-1", itemID: itemID, expectedRevision: start)
        }) { error in
            XCTAssertEqual(error as? ItemLifecycleControlError, .notAuthenticated)
        }
        XCTAssertEqual(try revision(of: itemID), start)
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE item_id = '\(itemID)'"), "0")
    }

    func testTheLifecycleControlRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(
            ranges.first { $0.name == ServiceCompositionRoot.itemLifecycleControlClientName })
        XCTAssertEqual(mine.start, CoreItemLifecycleControlClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func makeRoot(
        authenticated: Bool, center: FakeNotificationCenter = FakeNotificationCenter()
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
                sender: UnreachableProviderSender(), readSession: session,
                notificationCenter: center, reachability: FakeReachability(reachable: true)))
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

    /// Gives the item a scheduled-but-not-yet-installed reminder, exactly as the core stores one before a pass.
    private func seedLiveReminder(for itemID: String) -> String {
        let reminderID = UUID().uuidString.lowercased()
        let notificationID = "\(reminderID)#1"
        let dueInstant = "2090-01-01T09:00:00Z"
        execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                + "'not_acknowledged', '\(dueInstant)', 'UTC', 1, '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
        execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                + "effect_identity, external_id, scheduled_for, created_at) VALUES "
                + "('schedule:\(notificationID)', '\(reminderID)', 'schedule', 'pending', "
                + "'schedule:\(notificationID)', '\(notificationID)', '\(dueInstant)', '2026-10-08T09:30:02Z')")
        return reminderID
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
        let box = ControlResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func execute(_ sql: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "statement failed: \(detail)")
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
