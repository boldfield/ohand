import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Wait at least as long as the core's own 5 s busy timeout (core/src/store/schema/mod.rs) for its write lock.
private let coreStoreBusyTimeoutMilliseconds: Int32 = 5_000

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
        sqlite3_busy_timeout(database, coreStoreBusyTimeoutMilliseconds)
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
        /// The operation identifier of every event the core delivered, claimed by a consumer or not.
        private(set) var observedOperationIDs: [UInt64] = []
        private var nextOperationID: UInt64 = 1
        var consumer: ((CoreEvent) -> Bool)?

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in
                self.observedOperationIDs.append(event.operationID)
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

    /// The fake center with every pending-list read held at `gate` until it is released. The pass that asked
    /// stays inside the core, waiting on its native effect, for as long as a test needs.
    private final class GatedPendingListCenter: NotificationCenterProviding, @unchecked Sendable {
        private let base: FakeNotificationCenter
        private let gate: SuspensionGate

        init(base: FakeNotificationCenter, gate: SuspensionGate) {
            self.base = base
            self.gate = gate
        }

        func authorization() async -> NotificationAuthorization { await base.authorization() }

        func add(_ request: NotificationCenterRequest) async throws { try await base.add(request) }

        func pendingRequests() async throws -> [NotificationCenterPendingRequest] {
            await gate.wait()
            return try await base.pendingRequests()
        }

        func removePending(identifiers: [String]) async { await base.removePending(identifiers: identifiers) }

        func deliveredNotifications() async -> [NotificationCenterDeliveredNotification] {
            await base.deliveredNotifications()
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
        pendingListGate: SuspensionGate? = nil,
        metrics: MetricsRecording? = nil
    ) throws -> Rig {
        let session = try Session(path: storePath)
        let center = FakeNotificationCenter()
        center.authorizationStatus = authorization
        let effectsCenter: NotificationCenterProviding
        if let pendingListGate {
            effectsCenter = GatedPendingListCenter(base: center, gate: pendingListGate)
        } else {
            effectsCenter = center
        }
        let bridge = NotificationBridge(center: effectsCenter, ingestor: NotificationEventIngestor(handler: { _ in }))
        let core = try CoreReminderReconciler(core: session.core, bridge: bridge, center: effectsCenter, metrics: metrics)
        session.consumer = { core.handle($0) }
        let observations = InMemoryNotificationPermissionObservationStore()
        let reconciler = NotificationPermissionReconciler(
            center: center, reconciliation: core, observations: observations, promptCandidates: { [] })
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

    private func startAsync<Value>(_ body: @escaping () async throws -> Value) -> AsyncResultBox<Value> {
        let box = AsyncResultBox<Value>()
        Task {
            do { box.set(.success(try await body())) } catch { box.set(.failure(error)) }
        }
        return box
    }

    /// Pumps the main run loop, where the core delivers events, until `condition` holds or `timeout` passes.
    private func pump(until condition: () -> Bool, timeout: TimeInterval = 20) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func outcome<Value>(of box: AsyncResultBox<Value>, timeout: TimeInterval = 20) throws -> Value {
        guard pump(until: { box.result != nil }, timeout: timeout), let outcome = box.result else {
            throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "async work did not finish")
        }
        return try outcome.get()
    }

    private func runAsync<Value>(timeout: TimeInterval = 20, _ body: @escaping () async throws -> Value) throws -> Value {
        try outcome(of: startAsync(body), timeout: timeout)
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

    /// An assembly routes one core handle's events to every service on it, and the core accepts a duplicate
    /// operation identifier without reservation. The reconciler therefore takes its identifiers from a range
    /// no other client uses, so a reminder pass and a shadow review read that are in flight together each
    /// reach only their owner.
    func testAReminderPassOverlappingAShadowReviewReadOnOneCoreReachesOnlyItsOwner() throws {
        let seeded = reminders(count: 2)
        try prepareStore(seeded)
        let gate = SuspensionGate()
        let rig = try makeRig(authorization: .authorized, pendingListGate: gate)
        defer { finish(rig) }
        let reconciler = rig.reconciler
        let shadow = ShadowReviewService(core: rig.session.core, configuration: .off)
        var claims: [UInt64: (shadow: Bool, reminders: Bool)] = [:]
        rig.session.consumer = { event in
            // Both services are asked, so an event two of them would take is visible as a double claim.
            let claimedByShadow = shadow.handle(event)
            let claimedByReminders = rig.core.handle(event)
            claims[event.operationID] = (shadow: claimedByShadow, reminders: claimedByReminders)
            return claimedByShadow || claimedByReminders
        }

        // The pass is inside the core, waiting on the OS pending list, when the shadow read is submitted. The
        // read queues behind it on the core's single worker, so both answers are outstanding at once.
        let pass = startAsync { try await reconciler.refresh(trigger: .launch) }
        XCTAssertTrue(pump(until: { gate.arrivals >= 1 }), "the pass asked for the pending list")
        var shadowResult: ShadowReviewReadResult?
        shadow.record(jobID: "no-such-case") { shadowResult = $0 }
        XCTAssertNil(pass.result, "the pass is held while the read is submitted")
        gate.release()

        let report = try outcome(of: pass)
        XCTAssertTrue(pump(until: { shadowResult != nil }), "the shadow read answers its owner")
        XCTAssertEqual(shadowResult, .failed(code: "not_found"))
        XCTAssertNil(report.reconciliationFailure)
        XCTAssertEqual(report.readiness.scheduledCount, 2)
        XCTAssertTrue(report.converged)
        XCTAssertEqual(rig.center.addedRequests.count, 2)

        let repeated = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.readiness.scheduledCount, 2)
        XCTAssertEqual(rig.center.addedRequests.count, 2, "the overlap caused no second install")

        let observed = rig.session.observedOperationIDs
        XCTAssertEqual(observed.count, 3, "one shadow read and two reminder passes")
        XCTAssertEqual(Set(observed).count, observed.count, "no two operations shared an identifier")
        for operationID in observed {
            let claim = try XCTUnwrap(claims[operationID])
            XCTAssertNotEqual(claim.shadow, claim.reminders, "exactly one service owns operation \(operationID)")
        }
        let reminderIDs = observed.filter { claims[$0]?.reminders == true }
        let shadowIDs = observed.filter { claims[$0]?.shadow == true }
        XCTAssertEqual(reminderIDs.count, 2)
        XCTAssertEqual(shadowIDs.count, 1)
        XCTAssertTrue(reminderIDs.allSatisfy { $0 >= CoreReminderReconciler.firstOperationID })
        XCTAssertTrue(shadowIDs.allSatisfy { $0 < CoreReminderReconciler.firstOperationID })
        XCTAssertTrue(rig.session.events.isEmpty, "no event went unclaimed")
    }

    /// A planned prompt as the core names it: the pending-request identifier and its fire time.
    private struct PlannedPrompt {
        let identifier: String
        let fireAt: Date
    }

    /// Turns on a daily prompt in the core's store through the production bridge and returns the three occurrences the
    /// core plans from its own clock, soonest first. Prompts reach the reminder pass only this way.
    private func configurePromptSchedule(_ rig: Rig) throws -> [PlannedPrompt] {
        let session = rig.session
        let outcome = try session.run {
            try session.core.startConfigurePromptSchedule(
                operationID: $0, setting: .daily(hour: 9, minute: 0), timeZoneIdentifier: "UTC")
        }.decode(PromptScheduleOutcome.self)
        XCTAssertTrue(outcome.enabled)
        XCTAssertEqual(outcome.time, PromptScheduleOutcome.Time(hour: 9, minute: 0))
        let formatter = ISO8601DateFormatter()
        let dateFormatter = DateFormatter()
        dateFormatter.locale = Locale(identifier: "en_US_POSIX")
        dateFormatter.timeZone = TimeZone(identifier: "UTC")
        dateFormatter.dateFormat = "yyyy-MM-dd"
        let next = try XCTUnwrap(outcome.next)
        let first = try XCTUnwrap(formatter.date(from: next.fireAt))
        XCTAssertEqual(next.localDate, dateFormatter.string(from: first))
        return (0..<3).map { offset in
            let fireAt = first.addingTimeInterval(86_400 * Double(offset))
            return PlannedPrompt(identifier: "prompt:\(dateFormatter.string(from: fireAt))", fireAt: fireAt)
        }
    }

    func testCapacityHoldsOverflowRemindersAndOptionalPromptsNeverTakeTheirSlots() throws {
        let seeded = reminders(count: 62)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        _ = try configurePromptSchedule(rig)
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

    func testPendingPromptsYieldEverySlotToExplicitRemindersAndAreCancelledOnce() throws {
        let seeded = reminders(count: 60)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        let prompts = try configurePromptSchedule(rig)
        for prompt in prompts {
            rig.center.installForeignRequest(identifier: prompt.identifier, dueInstant: prompt.fireAt)
        }
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 3)
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 60)
        XCTAssertEqual(report.promptsGranted, 0, "explicit reminders take every usable slot")
        XCTAssertEqual(report.promptsDeferred, 3)
        XCTAssertEqual(
            rig.center.removedIdentifiers.sorted(), prompts.map(\.identifier).sorted(),
            "each pending prompt was cancelled through the production cancel path")
        XCTAssertEqual(rig.center.pendingIdentifiers, seeded.map { "\($0.reminderID)#1" }.sorted())
        XCTAssertEqual(rig.center.addedRequests.count, 60)

        let repeated = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.readiness.scheduledCount, 60)
        XCTAssertEqual(rig.center.removedIdentifiers.count, 3, "a repeated foreground cancels nothing again")
        XCTAssertEqual(rig.center.addedRequests.count, 60, "and installs nothing twice")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60, "the pending set stays within the planned budget")
    }

    func testPendingPromptsKeepOnlyTheSlotsTheRemindersLeave() throws {
        let seeded = reminders(count: 58)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        let prompts = try configurePromptSchedule(rig)
        for prompt in prompts {
            rig.center.installForeignRequest(identifier: prompt.identifier, dueInstant: prompt.fireAt)
        }
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 58)
        XCTAssertEqual(report.promptsGranted, 2)
        XCTAssertEqual(report.promptsDeferred, 1)
        XCTAssertEqual(rig.center.removedIdentifiers, [prompts[2].identifier], "only the latest prompt loses its slot")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)
        XCTAssertTrue(rig.center.pendingIdentifiers.contains(prompts[0].identifier))
        XCTAssertTrue(rig.center.pendingIdentifiers.contains(prompts[1].identifier))

        let repeated = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.promptsGranted, 2)
        XCTAssertEqual(rig.center.removedIdentifiers, [prompts[2].identifier])
        XCTAssertEqual(rig.center.addedRequests.count, 58)
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)
    }

    func testAPendingRequestOfUnknownKindCountsAgainstTheBudgetAndIsNeverCancelled() throws {
        let seeded = reminders(count: 60)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        rig.center.installForeignRequest(identifier: "stray-request", dueInstant: Date().addingTimeInterval(7200))
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 59, "the unknown request holds one of the 60 usable slots")
        XCTAssertEqual(report.readiness.reminders.filter { $0.outlook == .unscheduled(.capacityExceeded) }.count, 1)
        XCTAssertTrue(rig.center.removedIdentifiers.isEmpty, "the core does not cancel what it cannot attribute")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)
    }

    func testPendingRequestsTheCoreCannotNameStillHoldTheirSlotsAndAreNeverCancelled() throws {
        let seeded = reminders(count: 60)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        _ = try configurePromptSchedule(rig)
        // Five identifiers one byte over the core's bound and an empty one exhaust the four-slot headroom on their
        // own; a short foreign request is counted beside them.
        let overlong = (0..<5).map { index in String(index) + String(repeating: "x", count: 128) }
        let foreign = overlong + ["", "ohand.unattributable.0"]
        for identifier in foreign {
            rig.center.installForeignRequest(identifier: identifier, dueInstant: Date().addingTimeInterval(7200))
        }
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 7)
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertEqual(report.readiness.scheduledCount, 53, "each of the seven foreign requests holds a usable slot")
        XCTAssertEqual(report.readiness.reminders.filter { $0.outlook == .unscheduled(.capacityExceeded) }.count, 7)
        XCTAssertEqual(report.promptsGranted, 0)
        XCTAssertEqual(report.promptsDeferred, 3)
        XCTAssertTrue(rig.center.removedIdentifiers.isEmpty, "no foreign request is cancelled")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60, "the pending set stays within the usable budget")
        XCTAssertTrue(Set(foreign).isSubset(of: Set(rig.center.pendingIdentifiers)))
        XCTAssertEqual(rig.center.addedRequests.count, 53)

        let repeated = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.readiness.scheduledCount, 53)
        XCTAssertTrue(rig.center.removedIdentifiers.isEmpty, "a repeated foreground cancels nothing")
        XCTAssertEqual(rig.center.addedRequests.count, 53, "and installs nothing twice")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)
    }

    func testUnnameablePendingRequestsNeverAliasPromptsAndOnlyPendingPromptsYieldTheirSlots() throws {
        let seeded = reminders(count: 60)
        try prepareStore(seeded)
        let rig = try makeRig(authorization: .authorized)
        defer { finish(rig) }
        let prompts = try configurePromptSchedule(rig)
        // Foreign requests named like the stand-ins the core invents for unnameable ones are still only foreign.
        let unnameable = (0..<3).map { index in String(index) + String(repeating: "x", count: 128) } + [""]
        let lookalikes = ["ohand.unattributable.0", "ohand.unattributable.1"]
        for identifier in unnameable + lookalikes {
            rig.center.installForeignRequest(identifier: identifier, dueInstant: Date().addingTimeInterval(7200))
        }
        rig.center.installForeignRequest(identifier: prompts[0].identifier, dueInstant: prompts[0].fireAt)
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 7)
        let reconciler = rig.reconciler

        let report = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .launch) }

        XCTAssertNil(report.reconciliationFailure)
        XCTAssertEqual(report.readiness.scheduledCount, 54, "each of the six foreign requests holds a slot")
        XCTAssertEqual(report.readiness.reminders.filter { $0.outlook == .unscheduled(.capacityExceeded) }.count, 6)
        XCTAssertEqual(report.promptsGranted, 0, "no foreign request is mistaken for a planned prompt")
        XCTAssertEqual(report.promptsDeferred, 3)
        XCTAssertEqual(
            rig.center.removedIdentifiers, [prompts[0].identifier],
            "the pending prompt yields its slot to explicit reminders; nothing else is cancelled")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60, "the pending set stays within the usable budget")
        XCTAssertTrue(
            Set(unnameable + lookalikes).isSubset(of: Set(rig.center.pendingIdentifiers)), "no foreign request is cancelled")
        XCTAssertEqual(rig.center.addedRequests.count, 54)

        let repeated = try runAsync(timeout: 90) { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(repeated.readiness.scheduledCount, 54)
        XCTAssertEqual(repeated.promptsGranted, 0)
        XCTAssertEqual(rig.center.removedIdentifiers.count, 1, "a repeated foreground cancels nothing again")
        XCTAssertEqual(rig.center.addedRequests.count, 54, "and installs nothing twice")
        XCTAssertEqual(rig.center.pendingIdentifiers.count, 60)
    }

    func testOptionalPromptsUseTheRemainingSlotsWhenPermissionIsUsable() throws {
        let seeded = reminders(count: 4)
        try prepareStore(seeded)
        let metrics = RecordingMetrics()
        let rig = try makeRig(authorization: .authorized, metrics: metrics)
        defer { finish(rig) }
        let prompts = try configurePromptSchedule(rig)
        let reconciler = rig.reconciler

        let report = try runAsync { try await reconciler.refresh(trigger: .launch) }

        XCTAssertNil(report.reconciliationFailure)
        XCTAssertTrue(report.converged, "every install the core asked for was performed")
        XCTAssertEqual(report.readiness.scheduledCount, 4)
        XCTAssertEqual(report.promptsGranted, 3, "the planning horizon is three occurrences")
        XCTAssertEqual(report.promptsDeferred, 0)
        XCTAssertEqual(report.promptsWithheld, 0)
        XCTAssertEqual(rig.center.addedRequests.count, 7, "four reminders and the three granted prompts")
        let installedPrompts = rig.center.addedRequests.filter { $0.identifier.hasPrefix("prompt:") }
        XCTAssertEqual(installedPrompts.map(\.identifier).sorted(), prompts.map(\.identifier).sorted())
        for prompt in prompts {
            XCTAssertTrue(rig.center.pendingIdentifiers.contains(prompt.identifier), "\(prompt.identifier) is in the centre")
            let installed = try XCTUnwrap(installedPrompts.first { $0.identifier == prompt.identifier })
            XCTAssertEqual(installed.dueInstant.timeIntervalSince1970, prompt.fireAt.timeIntervalSince1970, accuracy: 1)
            XCTAssertEqual(installed.content.title, PromptNotificationWording.title)
            XCTAssertEqual(installed.content.body, PromptNotificationWording.body)
            XCTAssertNil(installed.content.categoryIdentifier, "a prompt offers no reminder actions")
        }
        XCTAssertTrue(metrics.records.isEmpty, "no scheduling failure is recorded")

        let repeated = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertTrue(repeated.converged)
        XCTAssertEqual(rig.center.addedRequests.count, 7, "a repeated pass installs nothing twice")
        XCTAssertTrue(rig.center.removedIdentifiers.isEmpty)
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

    func testSchedulingFailuresAreCountedByReasonAndCountNothingWhenSchedulingSucceeds() throws {
        let seeded = reminders(count: 2)
        try prepareStore(seeded)
        let metrics = RecordingMetrics()
        let rig = try makeRig(authorization: .denied, metrics: metrics)
        defer { finish(rig) }
        let reconciler = rig.reconciler

        _ = try runAsync { try await reconciler.refresh(trigger: .launch) }
        XCTAssertEqual(
            metrics.records, [.schedulingFailure(.permissionDenied), .schedulingFailure(.permissionDenied)])

        _ = try runAsync { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(metrics.records.count, 2, "a withheld schedule is not attempted again, so it is not counted again")

        rig.center.authorizationStatus = .authorized
        let regained = try runAsync { try await reconciler.refresh(trigger: .settingsReturn) }
        XCTAssertEqual(regained.readiness.scheduledCount, 2)
        XCTAssertEqual(metrics.records.count, 2, "installing the reminders adds no failure")
    }

    func testEachNativeScheduleFailureMapsToOneClosedLabel() {
        let mapped: [(Error, CoreMetricsSchedulingFailure?)] = [
            (NotificationBridgeError.permissionDenied, .permissionDenied),
            (NotificationBridgeError.dueTimeInPast, .unschedulable),
            (NotificationBridgeError.invalidPayload, .unschedulable),
            (NotificationBridgeError.installNotConfirmed, .nativeFailure),
            (NotificationBridgeError.timedOut, .nativeFailure),
            (NotificationBridgeError.centerFailed, .nativeFailure),
            (CancellationError(), nil),
            (NotificationBridgeError.cancelled, nil),
        ]
        for (error, expected) in mapped {
            XCTAssertEqual(CoreReminderReconciler.schedulingFailure(of: error), expected, "\(error)")
        }
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
