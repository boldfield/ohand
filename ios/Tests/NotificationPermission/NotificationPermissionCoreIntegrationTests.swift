import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Runs the permission reconciler over reminder facts read from the real Rust core on the
/// simulator.
///
/// The core does not yet export its capacity pass or `retry_rejected_schedule` across the C
/// boundary, so reminder rows are seeded into the closed store file (as in
/// `NotificationCoreIntegrationTests`) and the pass itself is a recording stand-in that returns
/// the facts the real core reports. What these tests prove with the real core: the facts the
/// module reads are the core's own status strings, a permission change alone leaves every
/// durable reminder, delivery and acknowledgment fact in the core unchanged, and capacity-held
/// reminders keep the core's reason under every permission. They do not prove that a real pass
/// re-arms or refills; that needs the export.
final class NotificationPermissionCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    private struct SeededReminder {
        let captureID: String
        let itemID: String
        let reminderID: String
        let requestState: String
        let scheduleState: String
        let unschedulableReason: String?
    }

    private let scheduled = SeededReminder(
        captureID: "capture-permission-1", itemID: "7d1c5a1e-0000-4000-8000-0000000000b1",
        reminderID: "4a2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4a01", requestState: "resolved",
        scheduleState: "scheduled", unschedulableReason: nil)
    private let rejected = SeededReminder(
        captureID: "capture-permission-2", itemID: "7d1c5a1e-0000-4000-8000-0000000000b2",
        reminderID: "4a2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4a02", requestState: "resolved",
        scheduleState: "schedule_failed", unschedulableReason: nil)
    private let heldBack = SeededReminder(
        captureID: "capture-permission-3", itemID: "7d1c5a1e-0000-4000-8000-0000000000b3",
        reminderID: "4a2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4a03", requestState: "unschedulable",
        scheduleState: "not_scheduled", unschedulableReason: "capacity_exceeded")

    private var seededReminders: [SeededReminder] { [scheduled, rejected, heldBack] }

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-permission-integration-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: storeDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { storeDirectory.appendingPathComponent("core.sqlite").path }

    private func seed(_ statements: [String]) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(storePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        }
    }

    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in self.events.append(event) }
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

        func status(_ itemID: String) throws -> ItemStatusReport {
            try run { try core.startItemStatus(operationID: $0, itemID: itemID) }.decode(ItemStatusReport.self)
        }
    }

    private func prepareStore() throws {
        let first = try Session(path: storePath)
        for reminder in seededReminders {
            let capture = CaptureRecord(
                captureID: reminder.captureID, text: "synthetic capture \(reminder.captureID)",
                captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0,
                locale: "en_US", calendar: "gregorian", itemScope: "personal", routeID: "route-default",
                entryLocked: false, createdAt: "2026-10-08T09:30:01Z")
            _ = try first.run { try first.core.startSaveCapture(operationID: $0, capture: capture) }
        }
        first.close()
        var statements: [String] = []
        for reminder in seededReminders {
            let reason = reminder.unschedulableReason.map { "'\($0)'" } ?? "NULL"
            statements.append(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
                    + "processing_state, transcription_state, created_at, updated_at) VALUES "
                    + "('\(reminder.itemID)', '\(reminder.captureID)', 0, 'active', 'saved_local', 'not_configured', "
                    + "'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')")
            statements.append(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                    + "acknowledgment_state, unschedulable_reason, resolved_instant, timezone_id, "
                    + "schedule_generation, created_at, updated_at) VALUES ('\(reminder.reminderID)', "
                    + "'\(reminder.itemID)', '\(reminder.requestState)', '\(reminder.scheduleState)', 'unknown', "
                    + "'not_acknowledged', \(reason), '2027-02-14T08:00:00Z', 'UTC', 1, "
                    + "'2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
        }
        try seed(statements)
    }

    private func storedFacts(_ session: Session) throws -> [ReminderFacts] {
        try seededReminders.map { reminder in
            try XCTUnwrap(ReminderFacts(try session.status(reminder.itemID)), "the core reports a reminder")
        }
    }

    private final class AsyncResultBox<Value>: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: Result<Value, Error>?

        var result: Result<Value, Error>? {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }

        func set(_ outcome: Result<Value, Error>) {
            lock.lock()
            defer { lock.unlock() }
            stored = outcome
        }
    }

    private func runAsync<Value>(timeout: TimeInterval = 20, _ body: @escaping () async throws -> Value) throws -> Value {
        let box = AsyncResultBox<Value>()
        Task {
            do { box.set(.success(try await body())) } catch { box.set(.failure(error)) }
        }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let outcome = box.result { return try outcome.get() }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "async work did not finish")
    }

    func testRevocationAndRegainOverRealCoreFactsChangeNoDurableReminderState() throws {
        try prepareStore()
        let session = try Session(path: storePath)
        defer { session.close() }

        let before = try storedFacts(session)
        XCTAssertEqual(before.map(\.requestState), ["resolved", "resolved", "unschedulable"])
        XCTAssertEqual(before.map(\.scheduleState), ["scheduled", "schedule_failed", "not_scheduled"])
        XCTAssertEqual(before.map(\.deliveryState), ["unknown", "unknown", "unknown"])

        let center = FakeNotificationCenter()
        let core = RecordingReconciliation(
            facts: before, promptsInstalled: 3, promptsDeferred: 4)
        let reconciler = NotificationPermissionReconciler(
            center: center, reconciliation: core, observations: InMemoryNotificationPermissionObservationStore())

        let launch = try runAsync { try await reconciler.refresh(trigger: .launch) }
        XCTAssertEqual(launch.readiness.scheduledCount, 1)
        XCTAssertEqual(launch.readiness.unscheduledCount, 2, "install failure and capacity hold both stay visible")
        XCTAssertEqual(launch.readiness.undeliverableCount, 0)
        XCTAssertEqual(launch.promptsInstalled, 3)
        XCTAssertEqual(launch.promptsDeferred, 4)

        center.authorizationStatus = .denied
        let revoked = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(revoked.transition, .lost)
        XCTAssertEqual(revoked.readiness.undeliverableCount, 2, "the installed and the rejected reminder cannot alert")
        XCTAssertEqual(
            revoked.readiness.reminders.last?.outlook, .unscheduled(.capacityExceeded),
            "a capacity hold keeps the core's reason under a revoked permission")
        XCTAssertFalse(revoked.readiness.optionalPromptsAllowed)

        let afterRevocation = try storedFacts(session)
        XCTAssertEqual(afterRevocation, before, "a permission change alone leaves the core's reminder facts unchanged")

        center.authorizationStatus = .provisional
        let regained = try runAsync { try await reconciler.refresh(trigger: .settingsReturn) }
        XCTAssertEqual(regained.transition, .regained)
        XCTAssertEqual(regained.readiness.undeliverableCount, 0)
        XCTAssertEqual(core.requests.map(\.rearmRejectedSchedules), [true, false, true])
        XCTAssertEqual(try storedFacts(session), before)
    }

    func testEveryItemStatusTheCoreReportsForAReminderMapsToAnOutlook() throws {
        try prepareStore()
        let session = try Session(path: storePath)
        defer { session.close() }

        let facts = try storedFacts(session)
        let usable = NotificationReadiness.evaluate(permission: .usable, reminders: facts)
        XCTAssertEqual(
            usable.reminders.map(\.outlook),
            [.scheduled, .unscheduled(.installFailed), .unscheduled(.capacityExceeded)])
        let denied = NotificationReadiness.evaluate(permission: .denied, reminders: facts)
        XCTAssertEqual(
            denied.reminders.map(\.outlook),
            [.undeliverable(.permissionDenied), .undeliverable(.permissionDenied), .unscheduled(.capacityExceeded)])
    }

    func testRealCenterPermissionIsReadAndMappedWithoutChangingIt() async throws {
        let center = SystemNotificationCenter()
        let observed = NotificationPermissionState(await center.authorization())
        let facts = [ReminderFixtures.resolved("item-1")]
        let reconciler = NotificationPermissionReconciler(
            center: center, reconciliation: RecordingReconciliation(facts: facts),
            observations: InMemoryNotificationPermissionObservationStore())

        let report = try await reconciler.refresh(trigger: .launch)

        XCTAssertEqual(report.permission, observed)
        let expected: ReminderOutlook = observed.canAlert
            ? .scheduled
            : .undeliverable(observed == .denied ? .permissionDenied : .permissionNotRequested)
        XCTAssertEqual(report.readiness.reminders.first?.outlook, expected)
        let again = NotificationPermissionState(await center.authorization())
        XCTAssertEqual(again, observed, "reading the permission does not change it")
    }
}
