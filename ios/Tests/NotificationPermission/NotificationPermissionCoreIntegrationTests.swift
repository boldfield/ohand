import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Runs the permission reconciler over the real Rust core on the simulator, through the production
/// `CoreReminderReconciler` and `NotificationBridge`. Only the OS notification center is a stand-in
/// (`FakeNotificationCenter`), so the core's own capacity pass, its re-arm of rejected schedules and its
/// reminder state decide every install, cancel and fact asserted here.
///
/// Reminder rows are seeded into the closed store file because no export creates reminders yet; every
/// later state (rejected, scheduled, held back for capacity) is produced by a real pass.
final class NotificationPermissionCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    private struct SeededReminder {
        let captureID: String
        let itemID: String
        let reminderID: String
        let dueInstant: String
    }

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

        func status(_ itemID: String) throws -> ItemStatusReport {
            try run { try core.startItemStatus(operationID: $0, itemID: itemID) }.decode(ItemStatusReport.self)
        }
    }

    private func reminders(count: Int) -> [SeededReminder] {
        (0..<count).map { index in
            SeededReminder(
                captureID: "capture-permission-\(index)",
                itemID: String(format: "7d1c5a1e-0000-4000-8000-%012d", index),
                reminderID: String(format: "4a2b8c1e-5d4a-4e0b-9a77-%012d", index),
                dueInstant: String(format: "2090-01-%02dT%02d:00:00Z", 1 + index / 24, index % 24))
        }
    }

    private func prepareStore(_ seeded: [SeededReminder]) throws {
        let first = try Session(path: storePath)
        for reminder in seeded {
            let capture = CaptureRecord(
                captureID: reminder.captureID, text: "synthetic capture \(reminder.captureID)",
                captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0,
                locale: "en_US", calendar: "gregorian", itemScope: "personal", routeID: "route-default",
                entryLocked: false, createdAt: "2026-10-08T09:30:01Z")
            _ = try first.run { try first.core.startSaveCapture(operationID: $0, capture: capture) }
        }
        first.close()
        var statements: [String] = []
        for reminder in seeded {
            let notificationID = "\(reminder.reminderID)#1"
            statements.append(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
                    + "processing_state, transcription_state, created_at, updated_at) VALUES "
                    + "('\(reminder.itemID)', '\(reminder.captureID)', 0, 'active', 'saved_local', 'not_configured', "
                    + "'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')")
            statements.append(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                    + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, "
                    + "updated_at) VALUES ('\(reminder.reminderID)', '\(reminder.itemID)', 'resolved', "
                    + "'pending_schedule', 'unknown', 'not_acknowledged', '\(reminder.dueInstant)', 'UTC', 1, "
                    + "'2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
            statements.append(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                    + "effect_identity, external_id, scheduled_for, created_at) VALUES "
                    + "('schedule:\(notificationID)', '\(reminder.reminderID)', 'schedule', 'pending', "
                    + "'schedule:\(notificationID)', '\(notificationID)', '\(reminder.dueInstant)', "
                    + "'2026-10-08T09:30:02Z')")
        }
        try seed(statements)
    }

    private func storedFacts(_ session: Session, _ seeded: [SeededReminder]) throws -> [ReminderFacts] {
        try seeded.map { reminder in
            try XCTUnwrap(ReminderFacts(try session.status(reminder.itemID)), "the core reports a reminder")
        }
    }

    private struct Rig {
        let session: Session
        let center: FakeNotificationCenter
        let core: CoreReminderReconciler
        let observations: InMemoryNotificationPermissionObservationStore
        let reconciler: NotificationPermissionReconciler
    }

    private func makeRig(
        authorization: NotificationAuthorization,
        prompts: [OptionalPromptCandidate] = []
    ) throws -> Rig {
        let session = try Session(path: storePath)
        let center = FakeNotificationCenter()
        center.authorizationStatus = authorization
        let bridge = NotificationBridge(center: center, ingestor: NotificationEventIngestor(handler: { _ in }))
        let core = try CoreReminderReconciler(core: session.core, bridge: bridge)
        session.consumer = { core.handle($0) }
        let observations = InMemoryNotificationPermissionObservationStore()
        let reconciler = NotificationPermissionReconciler(
            center: center, reconciliation: core, observations: observations, promptCandidates: { prompts })
        return Rig(session: session, center: center, core: core, observations: observations, reconciler: reconciler)
    }

    private func finish(_ rig: Rig) {
        rig.core.invalidate()
        rig.session.close()
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

    func testDenialRevocationAndRegainDriveTheRealCoreWithoutDuplicateInstallsOrDeliveryFacts() throws {
        let seeded = reminders(count: 2)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .denied)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        let denied = try runAsync { try await reconciler.refresh(trigger: .launch) }
        XCTAssertNil(denied.reconciliationFailure)
        XCTAssertEqual(denied.readiness.undeliverableCount, 2)
        XCTAssertEqual(denied.readiness.reminders.map(\.facts.scheduleState), ["schedule_failed", "schedule_failed"])
        XCTAssertTrue(rig.center.addedRequests.isEmpty, "nothing reaches the OS without permission")

        let stillDenied = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(stillDenied.transition, .unchanged)
        XCTAssertTrue(rig.center.addedRequests.isEmpty, "a rejected schedule is withheld, not retried, while denied")

        rig.center.authorizationStatus = .provisional
        let regained = try runAsync { try await reconciler.refresh(trigger: .settingsReturn) }
        XCTAssertEqual(regained.transition, .regained)
        XCTAssertEqual(regained.readiness.scheduledCount, 2)
        XCTAssertEqual(regained.readiness.undeliverableCount, 0)
        XCTAssertTrue(regained.converged)
        XCTAssertEqual(rig.center.pendingIdentifiers, seeded.map { "\($0.reminderID)#1" }.sorted())
        XCTAssertEqual(rig.center.addedRequests.count, 2)

        let repeated = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.readiness.scheduledCount, 2)
        XCTAssertEqual(rig.center.addedRequests.count, 2, "a repeated pass installs nothing twice")

        rig.center.authorizationStatus = .denied
        let revoked = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(revoked.transition, .lost)
        XCTAssertEqual(revoked.readiness.undeliverableCount, 2, "installed reminders cannot alert without permission")
        XCTAssertFalse(revoked.readiness.optionalPromptsAllowed)
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 2, "revocation removes nothing the core holds")
        XCTAssertEqual(rig.center.addedRequests.count, 2)

        for fact in try storedFacts(rig.session, seeded) {
            XCTAssertEqual(fact.requestState, "resolved")
            XCTAssertEqual(fact.scheduleState, "scheduled", "a permission change leaves the core's schedule state")
            XCTAssertEqual(fact.deliveryState, "unknown", "revocation and elapsed time are not delivery")
            XCTAssertEqual(fact.acknowledgmentState, "not_acknowledged", "and not attention")
        }
    }

    func testConcurrentRefreshesJoinOnePassAndInstallEachRequestOnce() throws {
        let seeded = reminders(count: 3)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        let reports = try runAsync {
            async let launch = reconciler.refresh(trigger: .launch)
            async let foreground = reconciler.refresh(trigger: .foreground)
            return try await [launch, foreground]
        }

        XCTAssertEqual(reports.map(\.readiness.scheduledCount), [3, 3])
        XCTAssertEqual(rig.center.addedRequests.count, 3)
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 3)
    }

    func testCapacityHoldsOverflowRemindersAndOptionalPromptsNeverTakeTheirSlots() throws {
        let seeded = reminders(count: 62)
        try prepareStore(seeded)
        let prompts = (0..<3).map { index in
            OptionalPromptCandidate(
                notificationID: "prompt-\(index)", fireAt: Date().addingTimeInterval(3600 * Double(index + 1)))
        }
        let rig = try makeRig(authorization: .authorized, prompts: prompts)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 60, "the usable budget is the OS limit less headroom")
        XCTAssertEqual(report.readiness.reminders.filter { $0.outlook == .unscheduled(.capacityExceeded) }.count, 2)
        XCTAssertEqual(report.promptsGranted, 0, "explicit reminders come before prompts")
        XCTAssertEqual(report.promptsDeferred, 3)
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)

        rig.center.authorizationStatus = .denied
        let revoked = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(revoked.readiness.undeliverableCount, 60)
        XCTAssertEqual(
            revoked.readiness.reminders.filter { $0.outlook == .unscheduled(.capacityExceeded) }.count, 2,
            "a capacity hold keeps the core's reason under a revoked permission")
        XCTAssertEqual(revoked.promptsWithheld, 3, "optional prompts are withheld while the permission cannot alert")
        XCTAssertEqual(revoked.promptsGranted, 0)
        XCTAssertEqual(rig.center.addedRequests.count, 60, "no request was installed twice")
    }

    func testOptionalPromptsUseTheRemainingSlotsWhenPermissionIsUsable() throws {
        let seeded = reminders(count: 4)
        try prepareStore(seeded)
        let prompts = (0..<9).map { index in
            OptionalPromptCandidate(
                notificationID: "prompt-\(index)", fireAt: Date().addingTimeInterval(3600 * Double(index + 1)))
        }
        let rig = try makeRig(authorization: .authorized, prompts: prompts)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        let report = try runAsync { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 4)
        XCTAssertEqual(report.promptsGranted, 7, "at most seven prompts within the horizon")
        XCTAssertEqual(report.promptsDeferred, 2)
        XCTAssertEqual(report.promptsWithheld, 0)
        XCTAssertEqual(rig.center.addedRequests.count, 4, "granting a prompt installs nothing; no prompt scheduler exists yet")
    }

    func testAnUnavailablePassStillReportsTheCoresStoredRemindersUnderRevocation() throws {
        let seeded = reminders(count: 2)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        let reconciler = rig.reconciler
        rig.core.invalidate()
        rig.center.authorizationStatus = .denied

        let report = try runAsync { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.reconciliationFailure?.code, "notification_effects_not_registered")
        XCTAssertFalse(report.converged)
        XCTAssertEqual(report.readiness.reminders.count, 2, "stored facts are read although the pass could not run")
        XCTAssertEqual(report.readiness.undeliverableCount, 2)
        XCTAssertNil(rig.observations.lastObserved(), "the permission is not recorded until a pass completes")
        XCTAssertTrue(rig.center.addedRequests.isEmpty)
    }

    func testARejectedScheduleReadsAsAnInstallFailureWhenUsableAndUndeliverableWhenDenied() throws {
        let seeded = reminders(count: 2)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .denied)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        _ = try runAsync { try await reconciler.refresh(trigger: .launch) }
        let facts = try storedFacts(rig.session, seeded)
        let usable = NotificationReadiness.evaluate(permission: .usable, reminders: facts)
        XCTAssertEqual(usable.reminders.map(\.outlook), [.unscheduled(.installFailed), .unscheduled(.installFailed)])
        let denied = NotificationReadiness.evaluate(permission: .denied, reminders: facts)
        XCTAssertEqual(denied.reminders.map(\.outlook), [.undeliverable(.permissionDenied), .undeliverable(.permissionDenied)])
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
