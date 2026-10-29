import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ReopenUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Drives the real source-detail screen over the real composition root and Rust core on an on-disk store with synthetic
/// data: Undo after Complete, then Set a reminder again, then save. The screen keeps exactly one reminder editor, the
/// one the source detail draws, and exactly one reminder ends up scheduled.
final class ItemReopenReminderAgainRealCoreTests: XCTestCase {
    private static let routeID = "route-item-reopen"
    private static let captureText = "synthetic capture for reopening"
    private static let formerInstant = "2090-01-01T09:00:00Z"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var center: FakeNotificationCenter!
    private var root: ServiceCompositionRoot!
    private var retainedWindow: UIWindow?
    private var onScreen: Set<String> = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("item-reopen-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        center = FakeNotificationCenter()
        root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: ReopenUnreachableSender(), readSession: session,
                notificationCenter: center, reachability: FakeReachability(reachable: false)))
    }

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        root?.shutDown()
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    func testUndoThenSetAReminderAgainUsesTheScreensOneEditorAndSchedulesOneReminder() throws {
        let itemID = try saveCapture()
        let start = try revision(of: itemID)
        try run { try await self.root.itemCorrections.correctType(
            commandID: UUID().uuidString.lowercased(), itemID: itemID, expectedRevision: start, itemType: .action) }
        seedLiveReminder(for: itemID)
        try run { try await self.root.reconcileReminders() }
        XCTAssertEqual(center.pendingIdentifiers.count, 1, "the seeded reminder is installed")

        let detailModel = SourceDetailModel.assembled(itemID: itemID, root: root)
        let controls = try XCTUnwrap(detailModel.itemControls)
        let editor = try XCTUnwrap(detailModel.reminderEditor)
        XCTAssertTrue(controls.reminderEditor === editor, "the controls prepare the screen's own editor")
        host(AnyView(NavigationStack { SourceDetailView(model: detailModel, renderReporter: { [unowned self] id, shown in
            if shown { onScreen.insert(id) } else { onScreen.remove(id) }
        }) }))
        settle("the detail, the controls and the reminder editor loaded") {
            detailModel.content != nil && controls.canAct && editor.isShown && !editor.isLoading
                && self.drawnDatePickers() == 1
        }
        XCTAssertEqual(drawnDatePickers(), 1, "the screen draws one reminder editor")

        onMain { await controls.complete() }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
        settle("Undo is offered") { onScreen.contains(ItemControlsView.undoIdentifier) }
        onMain { await controls.reopen() }
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "cancelled")
        settle("the prompt is drawn and the editor knows the item is open") {
            onScreen.contains(ItemControlsView.reminderOffIdentifier)
                && onScreen.contains(ItemControlsView.setReminderAgainIdentifier) && !editor.isClosed && !editor.isLoading
        }
        XCTAssertTrue(onScreen.contains(ItemControlsView.setReminderAgainIdentifier))

        onMain { await controls.setReminderAgain() }
        let former = try XCTUnwrap(ReminderInstant.date(from: Self.formerInstant))
        XCTAssertEqual(editor.draftDate, former, "the screen's editor starts from the former time")
        settle("the button is spent") { !onScreen.contains(ItemControlsView.setReminderAgainIdentifier) }
        XCTAssertEqual(drawnDatePickers(), 1, "still exactly one reminder editor on the screen")
        XCTAssertEqual(
            scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "cancelled",
            "nothing is scheduled before the person saves")

        onMain { await editor.save() }
        XCTAssertEqual(editor.message, .saved)
        settle("the saved reminder is installed") { self.center.pendingIdentifiers.count == 1 }
        XCTAssertEqual(center.pendingIdentifiers.count, 1, "exactly one reminder is scheduled")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminders WHERE item_id = '\(itemID)'"), "1")
        XCTAssertEqual(scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "resolved")
        XCTAssertEqual(drawnDatePickers(), 1, "the save left one reminder editor on the screen")
    }

    // MARK: helpers

    private func drawnDatePickers() -> Int {
        func count(_ view: UIView) -> Int { (view is UIDatePicker ? 1 : 0) + view.subviews.map(count).reduce(0, +) }
        return retainedWindow.map(count) ?? 0
    }

    private func host(_ view: AnyView) {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: view)
        window.makeKeyAndVisible()
        retainedWindow = window
        pauseRunLoop()
    }

    private func pauseRunLoop() {
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
    }

    /// Runs the main loop until the condition holds; fails the test naming it if ten seconds pass first.
    private func settle(
        _ condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) {
        pauseRunLoop()
        waitUntilPumping(condition, timeout: 10, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    private func onMain(_ operation: @escaping @MainActor () async -> Void) {
        var finished = false
        Task { @MainActor in
            await operation()
            finished = true
        }
        settle("the operation completed") { finished }
        XCTAssertTrue(finished, "the operation completed")
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws {
        var finished = false
        var failure: Error?
        Task {
            do { _ = try await operation() } catch { failure = error }
            finished = true
        }
        settle("the call completed") { finished }
        XCTAssertTrue(finished)
        if let failure { throw failure }
    }

    private func saveCapture() throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.captureText, capturedAt: Date()) {
            outcome = $0
        }
        settle("the save completes") { outcome != nil }
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
    }

    /// Gives the item a scheduled-but-not-yet-installed reminder, as the core stores one before a pass; no export
    /// creates a reminder for a plain capture.
    private func seedLiveReminder(for itemID: String) {
        let reminderID = UUID().uuidString.lowercased()
        let notificationID = "\(reminderID)#1"
        execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                + "'not_acknowledged', '\(Self.formerInstant)', 'UTC', 1, '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
        execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                + "effect_identity, external_id, scheduled_for, created_at) VALUES "
                + "('schedule:\(notificationID)', '\(reminderID)', 'schedule', 'pending', "
                + "'schedule:\(notificationID)', '\(notificationID)', '\(Self.formerInstant)', '2026-10-08T09:30:02Z')")
    }

    private func openDatabase() -> OpaquePointer? {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        sqlite3_busy_timeout(database, 5000)
        return database
    }

    private func execute(_ sql: String) {
        let database = openDatabase()
        defer { sqlite3_close(database) }
        XCTAssertEqual(sqlite3_exec(database, sql, nil, nil, nil), SQLITE_OK, sql)
    }

    private func scalar(_ sql: String) -> String {
        let database = openDatabase()
        defer { sqlite3_close(database) }
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
