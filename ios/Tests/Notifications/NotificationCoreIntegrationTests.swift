import Foundation
import SQLite3
import UserNotifications
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives the notification bridge with identifiers read from the real Rust core on the simulator,
/// and runs the real OS request builder and permission check.
///
/// The core does not yet export reminder operations across the C boundary, so reminder rows are
/// seeded into the closed store file the way `CaptureStatusTests` seeds items. What these tests
/// prove with the real core: the opaque identifiers the bridge accepts are the ones the core
/// returns, the stored capture text never reaches a payload, and OS evidence handed onward by the
/// bridge changes no core state.
final class NotificationCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    private let captureID = "capture-notification-1"
    private let itemID = "7d1c5a1e-0000-4000-8000-0000000000a1"
    private let reminderID = "3f2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4a5b"
    private let canaryText = "synthetic canary: pay the roof deposit"
    private let currentTime = Date(timeIntervalSince1970: 1_800_000_000)

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-notification-integration-\(UUID().uuidString)")
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

    /// Opens a core over `path` and records its events in arrival order.
    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in
                XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
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

        func save(_ capture: CaptureRecord) throws -> CoreEvent {
            try run { try core.startSaveCapture(operationID: $0, capture: capture) }
        }

        func read(_ captureID: String) throws -> CoreEvent {
            try run { try core.startGetCapture(operationID: $0, captureID: captureID) }
        }

        func status(_ itemID: String) throws -> CoreEvent {
            try run { try core.startItemStatus(operationID: $0, itemID: itemID) }
        }
    }

    private func makeCapture() -> CaptureRecord {
        CaptureRecord(
            captureID: captureID,
            text: canaryText,
            captureInstant: "2026-10-08T09:30:00Z",
            timezoneID: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeID: "route-default",
            entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z"
        )
    }

    /// Saves a capture through the real core, then stands in for the import and the reminder
    /// derivation by inserting the item and a resolved reminder (schedule generation 2).
    private func prepareStore() throws {
        let first = try Session(path: storePath)
        _ = try first.save(makeCapture()).decode(SaveCaptureAcknowledgment.self)
        first.close()
        try seed([
            "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
                + "processing_state, transcription_state, created_at, updated_at) VALUES "
                + "('\(itemID)', '\(captureID)', 0, 'active', 'saved_local', 'not_configured', 'unprocessed', "
                + "'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')",
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                + "'not_acknowledged', '2027-01-15T08:00:00Z', 'UTC', 2, '2026-10-08T09:30:02Z', "
                + "'2026-10-08T09:30:02Z')",
        ])
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

    /// Runs async bridge work while the main thread keeps servicing its run loop, which is where
    /// the core delivers events, so core reads and bridge calls can share one test.
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

    func testBridgeSchedulesTheCoreItemWithoutCaptureTextAndEvidenceChangesNoCoreState() throws {
        try prepareStore()
        let session = try Session(path: storePath)
        defer { session.close() }

        let before = try session.status(itemID).decode(ItemStatusReport.self)
        XCTAssertEqual(before.reminderRequestState, "resolved")
        XCTAssertEqual(before.reminderScheduleState, "pending_schedule")
        XCTAssertEqual(before.reminderDeliveryState, "unknown")
        let readout = try session.read(captureID).decode(CaptureReadout.self)
        XCTAssertEqual(readout.capture.text, canaryText, "the real core holds the canary text")

        let coreItemID = try XCTUnwrap(OpaqueIdentifier(before.itemID), "the core's item id is a valid opaque identifier")
        let notificationID = try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: 2))
        XCTAssertEqual(notificationID.rawValue, "\(reminderID)#2")

        let center = FakeNotificationCenter()
        let recorder = EventRecorder()
        let now = currentTime
        let bridge = NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { recorder.record($0) }),
            now: { now },
            removalPollInterval: 0
        )

        let due = try XCTUnwrap(ISO8601DateFormatter().date(from: "2027-01-15T08:00:00Z"))
        let scheduleRequest = NotificationScheduleRequest.generic(
            identifier: notificationID, dueInstant: due, opaqueTargetID: coreItemID)
        let installed = try runAsync { try await bridge.schedule(scheduleRequest) }
        XCTAssertEqual(installed.identifier, notificationID)

        let sent = try XCTUnwrap(center.addedRequests.first)
        var everyString: [String] = [sent.identifier, sent.content.title, sent.content.body]
        everyString.append(contentsOf: Array(sent.content.userInfo.keys))
        everyString.append(contentsOf: Array(sent.content.userInfo.values))
        XCTAssertEqual(Set(everyString), Set([
            "\(reminderID)#2", GenericNotificationWording.title, GenericNotificationWording.body,
            NotificationContent.targetKey, NotificationContent.kindKey, before.itemID, "generic",
        ]))
        XCTAssertFalse(everyString.contains { $0.contains("canary") || $0.contains("roof") })

        let pending = try runAsync { try await bridge.pendingNotifications() }
        XCTAssertEqual(pending.map(\.identifier), [notificationID])
        XCTAssertEqual(pending.first?.opaqueTargetID?.rawValue, before.itemID)

        center.delivered = [NotificationCenterDeliveredNotification(
            identifier: sent.identifier, deliveredAt: due, userInfo: sent.content.userInfo)]
        let forwarded = try runAsync { await bridge.ingestDeliveredNotifications() }
        XCTAssertEqual(forwarded, 1)
        XCTAssertEqual(recorder.events.first?.identifier, notificationID)
        XCTAssertEqual(recorder.events.first?.kind, .delivered)
        XCTAssertEqual(recorder.events.first?.opaqueTargetID?.rawValue, before.itemID)

        let after = try session.status(itemID).decode(ItemStatusReport.self)
        XCTAssertEqual(Self.facts(of: after), Self.facts(of: before),
                       "evidence handed to the receiver changes nothing in the core by itself")

        try runAsync { try await bridge.cancel(notificationID) }
        XCTAssertTrue(center.pendingIdentifiers.isEmpty)
    }

    private static func facts(of report: ItemStatusReport) -> [String?] {
        [report.itemID, report.saveState, report.syncState, report.processingState, report.transcriptionState,
         report.processingJobStatus, report.reminderRequestState, report.reminderScheduleState,
         report.reminderDeliveryState, report.reminderAcknowledgmentState, report.unschedulableReason]
    }

    func testSystemRequestBuilderMakesAnAbsoluteNonRepeatingUTCTrigger() throws {
        let due = Date(timeIntervalSince1970: 1_900_000_000)
        let content = NotificationContent.generic(opaqueTargetID: try XCTUnwrap(OpaqueIdentifier("item-1")))
        let request = SystemNotificationCenter.makeRequest(
            NotificationCenterRequest(identifier: "\(reminderID)#2", dueInstant: due, content: content))

        XCTAssertEqual(request.identifier, "\(reminderID)#2")
        XCTAssertEqual(request.content.title, GenericNotificationWording.title)
        XCTAssertEqual(request.content.body, GenericNotificationWording.body)
        XCTAssertEqual(request.content.userInfo as? [String: String], content.userInfo)
        let trigger = try XCTUnwrap(request.trigger as? UNCalendarNotificationTrigger)
        XCTAssertFalse(trigger.repeats)
        XCTAssertEqual(trigger.dateComponents.timeZone?.secondsFromGMT(), 0)
        let next = try XCTUnwrap(trigger.nextTriggerDate())
        XCTAssertEqual(next.timeIntervalSince1970, due.timeIntervalSince1970, accuracy: 1)
    }

    func testRealCenterWithoutGrantedPermissionIsUnauthorizedAndKeepsNothing() async throws {
        let center = SystemNotificationCenter()
        let status = await center.authorization()
        try XCTSkipIf(
            [.authorized, .provisional, .ephemeral].contains(status),
            "notification permission is already granted on this simulator; the denied path cannot be observed"
        )

        let bridge = NotificationBridge(center: center, ingestor: NotificationEventIngestor(handler: { _ in }))
        let identifier = try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: 1))
        let request = NotificationScheduleRequest.generic(
            identifier: identifier,
            dueInstant: Date().addingTimeInterval(3600),
            opaqueTargetID: try XCTUnwrap(OpaqueIdentifier("item-1"))
        )

        do {
            _ = try await bridge.schedule(request)
            XCTFail("scheduling without permission must fail")
        } catch let error as NotificationBridgeError {
            XCTAssertEqual(error, .permissionDenied)
            XCTAssertEqual(error.errorClass, .unauthorized)
        }
        let pending = try await bridge.pendingNotifications()
        XCTAssertFalse(pending.contains { $0.identifier == identifier })
    }
}
