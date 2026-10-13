import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Wait at least as long as the core's own 5 s busy timeout (core/src/store/schema/mod.rs) for its write lock.
private let coreStoreBusyTimeoutMilliseconds: Int32 = 5_000

/// Raw SQL is confined to this file. The deletion intent itself (tombstone, search removal, cancel operations) is
/// recorded by `mark_deletion_intent` in the Rust core, which has no export yet; `seedDeletedState` writes the rows that
/// call leaves behind for one item with a reminder whose second schedule generation has an unrecorded cancel. The core's
/// own tests cover the tombstone; these tests cover how the native cleanup drives the queue that tombstone leaves.
final class RealCoreDeletionWorld {
    let layout: ProtectedStorageLayout
    let center = FakeNotificationCenter()
    let faults = FaultPlan()
    let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    private var sessions: [RealCoreSession] = []

    var databasePath: String { layout.databaseURL.path }

    init() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-deletion-core-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        guard ProtectedStorageService(layout: layout).prepare().isFullyConfigured else { throw StorageSetupFailed() }
        try CoreHandle(path: layout.databaseURL.path).close()
        try execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')")
    }

    func tearDown() {
        sessions.forEach { $0.close() }
        sessions = []
        try? FileManager.default.removeItem(at: layout.rootDirectory)
    }

    /// Opens the core over the on-disk store: a process start. Closing it is a process end.
    func openCore() throws -> RealCoreSession {
        let session = try RealCoreSession(path: databasePath)
        sessions.append(session)
        return session
    }

    func makeBridge() -> NotificationBridge {
        let now = currentTime
        return NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { _ in }),
            now: { now },
            removalPollInterval: 0,
            maximumRemovalPolls: 5)
    }

    func makeService(on session: RealCoreSession) -> DeletionCleanupService {
        DeletionCleanupService(
            core: FaultingDeletionCleanupCore(wrapped: session.client, faults: faults),
            notifications: FaultingNotificationCanceller(bridge: makeBridge(), faults: faults),
            localCleanup: DeletionLocalCleanup(layout: layout, fileSystem: FaultingDeletionFileSystem(faults: faults)))
    }

    // MARK: Files

    func url(_ store: ProtectedStore, _ name: String) -> URL {
        layout.directory(for: store).appendingPathComponent(name)
    }

    func exists(_ store: ProtectedStore, _ name: String) -> Bool {
        FileManager.default.fileExists(atPath: url(store, name).path)
    }

    /// Paths anywhere under the storage root, other than the core database files, whose name mentions `captureID`.
    func remnants(of captureID: String) -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: layout.rootDirectory.path)) ?? []
        return subpaths.filter { ($0 as NSString).lastPathComponent.contains(captureID) }.sorted()
    }

    func pendingNotificationIdentifiers(ofReminder reminderID: String) -> [String] {
        center.pendingIdentifiers.filter { $0.hasPrefix(reminderID + "#") }.sorted()
    }

    private var captureContext: IngressCaptureContext {
        IngressCaptureContext(
            captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: "route-personal", entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z", sessionTopic: nil)
    }

    // MARK: Seeding

    /// Imports a voice capture through the real core and puts its files in protected storage: the finalized audio, the
    /// staged record and the in-progress recording, and optionally a record write that was interrupted midway.
    /// Returns the item the core created.
    func importVoiceCapture(captureID: String, interruptedRecordWrite: Bool = false) throws -> String {
        let finalizedName = "\(captureID).m4a"
        try Data("synthetic audio".utf8).write(to: url(.finalizedAudio, finalizedName))
        let handoff = IngressAudioHandoff(inProgressFileName: "\(captureID)-recording.m4a", finalizedFileName: finalizedName)
        try Data("synthetic audio".utf8).write(to: url(.ingressInProgressAudio, handoff.inProgressFileName))
        let record = IngressRecord(captureID: captureID, text: nil, audio: handoff, context: captureContext)
        _ = try IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem()).stage(record)
        if interruptedRecordWrite {
            try Data("{\"text\":\"buy oat".utf8).write(to: url(.ingressStagingRecords, "\(captureID).json.incomplete"))
        }

        let importing = try openCore()
        defer { importing.close() }
        let acknowledgment = try importing.importCapture(CaptureRecord(
            captureID: captureID,
            text: nil,
            audioReference: IngressRecord.audioReference(finalizedFileName: finalizedName),
            captureInstant: captureContext.captureInstant,
            timezoneID: captureContext.timezoneID,
            utcOffsetMinutes: captureContext.utcOffsetMinutes,
            locale: captureContext.locale,
            calendar: captureContext.calendar,
            itemScope: captureContext.itemScope,
            routeID: captureContext.routeID,
            entryLocked: captureContext.entryLocked,
            createdAt: captureContext.createdAt,
            sessionTopic: nil))
        return acknowledgment.itemID
    }

    /// Installs a request in the fake OS center for each generation, as the reminder schedule did before the delete.
    func installNotifications(
        itemID: String, reminderID: String, generations: [Int64], on session: RealCoreSession
    ) throws {
        let bridge = makeBridge()
        let target = try XCTUnwrap(OpaqueIdentifier(itemID))
        let due = currentTime.addingTimeInterval(3600)
        for generation in generations {
            let identifier = try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: generation))
            _ = try session.run {
                try await bridge.schedule(.generic(identifier: identifier, dueInstant: due, opaqueTargetID: target))
            }
        }
    }

    /// The rows `mark_deletion_intent` leaves for an item with audio and a reminder that reached generation 2: the
    /// first generation's cancel acknowledged, the second generation's cancel still pending. No core may be open.
    func seedDeletedState(itemID: String, captureID: String, reminderID: String) throws {
        try execute("UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(itemID)'")
        try execute("UPDATE captures SET text = '', session_topic = '' WHERE capture_id = '\(captureID)'")
        try execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'cancelled', 'scheduled', 'unknown', 'not_acknowledged', "
                + "'2026-10-12T09:00:00Z', 'UTC', 2, '2026-10-10T09:31:00Z', '2026-10-10T09:32:00Z')")
        let operations: [(String, String, Int, String)] = [
            ("schedule", "acknowledged", 1, "2026-10-10T09:31:00Z"),
            ("cancel", "acknowledged", 1, "2026-10-10T09:32:00Z"),
            ("schedule", "acknowledged", 2, "2026-10-10T09:32:00Z"),
            ("cancel", "pending", 2, "2026-10-10T09:33:00Z"),
        ]
        for (index, (type, state, generation, createdAt)) in operations.enumerated() {
            try execute(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                    + "effect_identity, external_id, created_at) VALUES ('\(reminderID)-op-\(index)', '\(reminderID)', "
                    + "'\(type)', '\(state)', '\(type):\(reminderID)#\(generation)', '\(reminderID)#\(generation)', "
                    + "'\(createdAt)')")
        }
        for kind in DeletionCleanupKind.allCases {
            try execute(
                "INSERT INTO deletion_work (deletion_work_id, item_id, work_type, status, created_at) "
                    + "VALUES ('work-\(itemID)-\(kind.rawValue)', '\(itemID)', '\(kind.rawValue)', 'pending', "
                    + "'2026-10-10T09:33:00Z')")
        }
    }

    /// Replaces the audio reference the core holds for a capture. No core may be open.
    func setAudioReference(captureID: String, to reference: String) throws {
        try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'")
    }

    // MARK: Inspection of the durable core state

    func workStatuses(itemID: String) throws -> [String: String] {
        var statuses: [String: String] = [:]
        try query("SELECT work_type, status FROM deletion_work WHERE item_id = '\(itemID)'") { statement in
            statuses[Self.text(statement, 0)] = Self.text(statement, 1)
        }
        return statuses
    }

    func audioReference(captureID: String) throws -> String? {
        var reference: String?
        try query("SELECT audio_reference FROM captures WHERE capture_id = '\(captureID)'") { statement in
            reference = sqlite3_column_type(statement, 0) == SQLITE_NULL ? nil : Self.text(statement, 0)
        }
        return reference
    }

    func cancelOperationStates(reminderID: String) throws -> [String: String] {
        var states: [String: String] = [:]
        try query(
            "SELECT external_id, operation_state FROM reminder_operations "
                + "WHERE reminder_id = '\(reminderID)' AND operation_type = 'cancel'"
        ) { statement in
            states[Self.text(statement, 0)] = Self.text(statement, 1)
        }
        return states
    }

    // MARK: SQLite

    private func execute(_ statement: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        sqlite3_busy_timeout(database, coreStoreBusyTimeoutMilliseconds)
        defer { sqlite3_close(database) }
        let status = sqlite3_exec(database, statement, nil, nil, nil)
        XCTAssertEqual(status, SQLITE_OK, "\(String(cString: sqlite3_errmsg(database))): \(statement)")
        if status != SQLITE_OK { throw StorageSetupFailed() }
    }

    private func query(_ sql: String, row: (OpaquePointer?) -> Void) throws {
        var database: OpaquePointer?
        guard sqlite3_open(databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        sqlite3_busy_timeout(database, coreStoreBusyTimeoutMilliseconds)
        defer { sqlite3_close(database) }
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_finalize(statement) }
        while sqlite3_step(statement) == SQLITE_ROW { row(statement) }
    }

    private static func text(_ statement: OpaquePointer?, _ column: Int32) -> String {
        sqlite3_column_text(statement, column).map { String(cString: $0) } ?? ""
    }
}

/// One "process": a real core over the on-disk store with the production deletion client attached to its single event
/// handler. Events arrive on the main queue, where XCTest runs synchronous tests, so waiting pumps the run loop.
final class RealCoreSession {
    let core: CoreHandle
    let client: CoreDeletionCleanupClient
    private(set) var otherEvents: [CoreEvent] = []
    private var nextOperationID: UInt64 = 1
    private var isClosed = false

    init(path: String) throws {
        core = try CoreHandle(path: path)
        client = CoreDeletionCleanupClient(core: core)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            if !self.client.handle(event) { self.otherEvents.append(event) }
        }
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Runs one async call to completion while the main run loop delivers the core's events.
    func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = AnswerBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    func importCapture(_ record: CaptureRecord) throws -> IngressImportAcknowledgment {
        let operationID = nextOperationID
        nextOperationID += 1
        try core.startImportForegroundIngress(operationID: operationID, record: record)
        XCTAssertTrue(pump(until: { self.otherEvents.contains { $0.operationID == operationID } }))
        let event = try XCTUnwrap(otherEvents.first { $0.operationID == operationID })
        return try event.decode(IngressImportAcknowledgment.self)
    }

    /// Ends the process: nothing but the files survives.
    func close() {
        guard !isClosed else { return }
        isClosed = true
        client.shutDown()
        core.close()
    }
}

private final class AnswerBox<Value>: @unchecked Sendable {
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

/// The production client behind fault points before and after each call, so an interruption can land with the core
/// already changed and the caller not yet told. Complete points are named by the work row's kind.
final class FaultingDeletionCleanupCore: DeletionCleanupCore, @unchecked Sendable {
    private let wrapped: DeletionCleanupCore
    private let faults: FaultPlan
    private let lock = NSLock()
    private var kindsByWorkID: [String: DeletionCleanupKind] = [:]

    init(wrapped: DeletionCleanupCore, faults: FaultPlan) {
        self.wrapped = wrapped
        self.faults = faults
    }

    func itemsWithPendingCleanup() async throws -> [String] {
        try faults.check("core.itemsWithPendingCleanup")
        return try await wrapped.itemsWithPendingCleanup()
    }

    func pendingCleanup(itemID: String) async throws -> [DeletionCleanupWork] {
        try faults.check("core.pendingCleanup")
        let work = try await wrapped.pendingCleanup(itemID: itemID)
        lock.lock()
        for row in work { kindsByWorkID[row.workID] = row.kind }
        lock.unlock()
        return work
    }

    func terminallyFailedCleanup() async throws -> [DeletionCleanupWork] {
        try faults.check("core.terminalFailures")
        return try await wrapped.terminallyFailedCleanup()
    }

    func cleanupTarget(itemID: String) async throws -> DeletionCleanupTarget? {
        try faults.check("core.cleanupTarget")
        return try await wrapped.cleanupTarget(itemID: itemID)
    }

    func reminderNotificationIdentifiers(itemID: String) async throws -> [NotificationIdentifier] {
        try faults.check("core.reminderIdentifiers")
        return try await wrapped.reminderNotificationIdentifiers(itemID: itemID)
    }

    func recordNotificationCancelled(_ identifier: NotificationIdentifier) async throws {
        try faults.check("core.recordCancelled")
        try await wrapped.recordNotificationCancelled(identifier)
        try faults.check("core.recordCancelled.afterEffect")
    }

    func completeCleanup(workID: String) async throws {
        let name = kind(of: workID).map { "core.complete.\($0.rawValue)" } ?? "core.complete"
        try faults.check(name)
        try await wrapped.completeCleanup(workID: workID)
        try faults.check("\(name).afterEffect")
    }

    func failCleanup(workID: String) async throws {
        try faults.check("core.fail")
        try await wrapped.failCleanup(workID: workID)
    }

    private func kind(of workID: String) -> DeletionCleanupKind? {
        lock.lock()
        defer { lock.unlock() }
        return kindsByWorkID[workID]
    }
}
