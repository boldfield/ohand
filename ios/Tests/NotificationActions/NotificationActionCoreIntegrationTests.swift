import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Wait at least as long as the core's own 5 s busy timeout (core/src/store/schema/mod.rs) for its write lock.
private let coreStoreBusyTimeoutMilliseconds: Int32 = 5_000

/// Runs the action handler over the real Rust core on the simulator, through the production
/// `CoreNotificationActionClient`, the durable file journal and the real read-authentication session. Only the
/// authentication prompt is a stand-in (`FakeReadAuthenticationBoundary`); every state asserted here is read back from
/// the core's own SQLite store, so the core's rules decide what a tap or action changes.
///
/// The item and its two schedule generations are seeded into the closed store file because no export creates reminders
/// yet. Generation 1 was superseded by generation 2, and both were handed to the OS.
final class NotificationActionCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var rootDirectory: URL!

    private let itemID = "7d1c5a1e-0000-4000-8000-000000000001"
    private let reminderID = "4a2b8c1e-5d4a-4e0b-9a77-000000000001"
    private var currentNotification: NotificationIdentifier {
        NotificationIdentifier(reminderID: reminderID, scheduleGeneration: 2)!
    }
    private var supersededNotification: NotificationIdentifier {
        NotificationIdentifier(reminderID: reminderID, scheduleGeneration: 1)!
    }

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        rootDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-action-integration-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: rootDirectory, withIntermediateDirectories: true)
        try prepareStore()
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: rootDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { rootDirectory.appendingPathComponent("core.sqlite").path }
    private var journalDirectory: URL { rootDirectory.appendingPathComponent("action-journal", isDirectory: true) }

    // MARK: store access

    private func withDatabase<Value>(_ body: (OpaquePointer?) throws -> Value) rethrows -> Value {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(storePath, &database), SQLITE_OK)
        sqlite3_busy_timeout(database, coreStoreBusyTimeoutMilliseconds)
        defer { sqlite3_close(database) }
        return try body(database)
    }

    private func execute(_ statements: [String]) {
        withDatabase { database in
            for statement in statements {
                var message: UnsafeMutablePointer<CChar>?
                let code = sqlite3_exec(database, statement, nil, nil, &message)
                let detail = message.map { String(cString: $0) } ?? ""
                sqlite3_free(message)
                XCTAssertEqual(code, SQLITE_OK, "statement failed: \(detail)")
            }
        }
    }

    private func scalar(_ sql: String) -> String {
        withDatabase { database in
            var statement: OpaquePointer?
            XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
            defer { sqlite3_finalize(statement) }
            guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
            return String(cString: text)
        }
    }

    private var lifecycle: String { scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'") }
    private var revision: String { scalar("SELECT CAST(revision AS TEXT) FROM items WHERE item_id = '\(itemID)'") }
    private var acknowledgment: String {
        scalar("SELECT acknowledgment_state FROM reminders WHERE reminder_id = '\(reminderID)'")
    }
    private var deliveryState: String {
        scalar("SELECT delivery_state FROM reminders WHERE reminder_id = '\(reminderID)'")
    }
    private var completionEventCount: String {
        scalar("SELECT CAST(COUNT(*) AS TEXT) FROM events WHERE event_type = 'completion'")
    }

    private func prepareStore() throws {
        let session = try Session(path: storePath)
        let capture = CaptureRecord(
            captureID: "capture-action-1", text: "synthetic capture", captureInstant: "2026-10-08T09:30:00Z",
            timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US", calendar: "gregorian", itemScope: "personal",
            routeID: "route-default", entryLocked: false, createdAt: "2026-10-08T09:30:01Z")
        _ = try session.run { try session.core.startSaveCapture(operationID: $0, capture: capture) }
        session.close()
        var statements = [
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, item_type, save_state, sync_state, "
                + "processing_state, transcription_state, created_at, updated_at) VALUES "
                + "('\(itemID)', 'capture-action-1', 0, 'active', 'action', 'saved_local', 'not_configured', "
                + "'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')",
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'scheduled', 'unknown', 'not_acknowledged', "
                + "'2090-01-01T09:00:00Z', 'UTC', 2, '2026-10-08T09:31:00Z', '2026-10-08T09:32:00Z')",
        ]
        for generation in 1...2 {
            let notificationID = "\(reminderID)#\(generation)"
            statements.append(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                    + "effect_identity, external_id, created_at) VALUES ('schedule:\(notificationID)', "
                    + "'\(reminderID)', 'schedule', 'acknowledged', 'schedule:\(notificationID)', '\(notificationID)', "
                    + "'2026-10-08T09:32:00Z')")
        }
        execute(statements)
    }

    // MARK: running pieces

    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1
        var consumer: ((CoreEvent) -> Bool)?

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in
                if self.consumer?(event) == true { return }
                self.events.append(event)
            }
        }

        deinit { core.close() }

        func close() { core.close() }

        func run(_ start: (UInt64) throws -> Void, timeout: TimeInterval = 20) throws -> CoreEvent {
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
    }

    private final class OpenedItems: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [String] = []

        func add(_ item: String) {
            lock.lock()
            items.append(item)
            lock.unlock()
        }

        var all: [String] {
            lock.lock()
            defer { lock.unlock() }
            return items
        }
    }

    /// One app launch: a core, the action client over it, a read-authentication session and a handler over the
    /// journal in `journalDirectory`.
    private final class Launch {
        let session: Session
        let client: CoreNotificationActionClient
        let authentication: ReadAuthenticationSession
        let handler: ActionHandler
        let opened: OpenedItems

        init(storePath: String, journalDirectory: URL) throws {
            let session = try Session(path: storePath)
            let client = CoreNotificationActionClient(core: session.core)
            session.consumer = { client.handle($0) }
            let authentication = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
            let opened = OpenedItems()
            self.session = session
            self.client = client
            self.authentication = authentication
            self.opened = opened
            handler = ActionHandler(
                core: client, journal: FileNotificationActionJournal(directory: journalDirectory),
                authorization: authentication, openItem: { opened.add($0) })
        }

        func shutDown() {
            client.shutDown()
            session.close()
        }
    }

    private func startLaunch() throws -> Launch {
        try Launch(storePath: storePath, journalDirectory: journalDirectory)
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 20) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private final class ResultBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: NotificationActionReconciliation?

        var value: NotificationActionReconciliation? {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }

        func set(_ value: NotificationActionReconciliation) {
            lock.lock()
            stored = value
            lock.unlock()
        }
    }

    /// Runs one reconcile pass, pumping the main run loop where the core delivers events.
    private func reconcile(_ launch: Launch, file: StaticString = #filePath, line: UInt = #line)
        -> NotificationActionReconciliation
    {
        let box = ResultBox()
        let handler = launch.handler
        Task { box.set(await handler.reconcile()) }
        XCTAssertTrue(pump(until: { box.value != nil }), "the pass finished", file: file, line: line)
        return box.value ?? NotificationActionReconciliation()
    }

    private func unlock(_ launch: Launch) {
        XCTAssertEqual(authenticateAndWait(launch.authentication), .authenticated)
    }

    private func event(
        _ kind: NotificationEventKind, identifier: NotificationIdentifier? = nil
    ) -> NotificationEvent {
        NotificationEvent(
            identifier: identifier ?? currentNotification, kind: kind, opaqueTargetID: OpaqueIdentifier(itemID),
            occurredAt: Date())
    }

    private func action(_ raw: String) -> NotificationEventKind { .action(OpaqueIdentifier(raw)!) }

    /// Lets the reconcile pass that `ingest` started finish, so a test sees a settled store.
    private func settle(_ launch: Launch) { _ = reconcile(launch) }

    // MARK: tests

    func testCompleteWaitsForUnlockThenMutatesOnceAndRepeatsChangeNothing() throws {
        let launch = try startLaunch()
        defer { launch.shutDown() }

        XCTAssertEqual(launch.handler.ingest(event(action(NotificationActionIdentifier.complete))), .persisted)
        let locked = reconcile(launch)
        XCTAssertEqual(locked.deferred, 1)
        XCTAssertEqual(lifecycle, "active", "no mutation while the session is capture-only")
        XCTAssertEqual(revision, "0")

        unlock(launch)
        let unlocked = reconcile(launch)
        XCTAssertEqual(unlocked.settled, 1)
        XCTAssertEqual(lifecycle, "completed")
        XCTAssertEqual(revision, "1", "one revisioned completion event")
        XCTAssertEqual(completionEventCount, "1")
        XCTAssertEqual(acknowledgment, "not_acknowledged", "completing never acknowledges")

        launch.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        let duplicate = reconcile(launch)
        XCTAssertEqual(duplicate.deferred, 0)
        XCTAssertEqual(lifecycle, "completed")
        XCTAssertEqual(revision, "1", "a duplicate callback does not mutate again")
        XCTAssertEqual(completionEventCount, "1")
        XCTAssertEqual(try FileNotificationActionJournal(directory: journalDirectory).pending(), [])
    }

    func testAcknowledgeNeverCompletesAndAnOlderGenerationCannotComplete() throws {
        let launch = try startLaunch()
        defer { launch.shutDown() }
        unlock(launch)

        launch.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        settle(launch)
        XCTAssertEqual(acknowledgment, "acknowledged")
        XCTAssertEqual(lifecycle, "active", "acknowledging never completes")
        XCTAssertEqual(revision, "0")

        launch.handler.ingest(
            event(action(NotificationActionIdentifier.complete), identifier: supersededNotification))
        settle(launch)
        XCTAssertEqual(lifecycle, "active", "a superseded schedule generation cannot complete the item")
        XCTAssertEqual(revision, "0")
        XCTAssertEqual(try FileNotificationActionJournal(directory: journalDirectory).pending(), [])
    }

    func testDeletedItemIsNotResurrectedOrMutated() throws {
        execute(["UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(itemID)'"])
        let launch = try startLaunch()
        defer { launch.shutDown() }
        unlock(launch)

        launch.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        launch.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        launch.handler.ingest(event(.opened))
        settle(launch)
        XCTAssertEqual(lifecycle, "deleted")
        XCTAssertEqual(revision, "0")
        XCTAssertEqual(acknowledgment, "not_acknowledged")
        XCTAssertTrue(launch.opened.all.isEmpty, "a tap on a deleted item opens nothing")
        XCTAssertEqual(try FileNotificationActionJournal(directory: journalDirectory).pending(), [])
    }

    func testTapRecordsEvidenceWhileLockedAndRoutesToTheCoreItemAfterUnlock() throws {
        let launch = try startLaunch()
        defer { launch.shutDown() }

        launch.handler.ingest(event(.opened))
        let locked = reconcile(launch)
        XCTAssertEqual(locked.deferred, 1)
        XCTAssertNotEqual(deliveryState, "unknown", "opening evidence needs no private read")
        XCTAssertEqual(acknowledgment, "not_acknowledged", "opening is evidence, not acknowledgment")
        XCTAssertTrue(launch.opened.all.isEmpty)

        unlock(launch)
        let unlocked = reconcile(launch)
        XCTAssertEqual(unlocked.openedItemIDs, [itemID])
        XCTAssertEqual(launch.opened.all, [itemID])
    }

    func testCallbackStoredDuringACoreOutageIsReconciledByTheNextLaunch() throws {
        let first = try startLaunch()
        first.client.shutDown()
        XCTAssertEqual(first.handler.ingest(event(action(NotificationActionIdentifier.acknowledge))), .persisted)
        XCTAssertEqual(reconcile(first).retained, 1, "the core could not answer, so the entry stays")
        first.shutDown()
        XCTAssertEqual(acknowledgment, "not_acknowledged")
        XCTAssertEqual(try FileNotificationActionJournal(directory: journalDirectory).pending().count, 1)

        let second = try startLaunch()
        defer { second.shutDown() }
        XCTAssertEqual(reconcile(second).settled, 1)
        XCTAssertEqual(acknowledgment, "acknowledged")
        XCTAssertEqual(try FileNotificationActionJournal(directory: journalDirectory).pending(), [])
    }

    func testCompleteStoredBeforeRelaunchRunsAfterTheNextUnlock() throws {
        let first = try startLaunch()
        first.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        XCTAssertEqual(reconcile(first).deferred, 1)
        first.shutDown()
        XCTAssertEqual(lifecycle, "active")

        let second = try startLaunch()
        defer { second.shutDown() }
        second.handler.attach(to: second.authentication)
        unlock(second)
        XCTAssertTrue(pump(until: { self.lifecycle == "completed" }), "authenticating reconciles stored actions")
        XCTAssertEqual(revision, "1")
        XCTAssertEqual(completionEventCount, "1")
    }
}
