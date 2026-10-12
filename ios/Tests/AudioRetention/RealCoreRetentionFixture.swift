import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Raw SQL is confined to this file. Transcription results and deletion are written by other core flows that have no
/// export yet, so the fixture writes the rows those flows leave behind (a transcript attachment with a chosen
/// `transcribed_at`, a deleted lifecycle state). Capture import and every retention decision go through the real core.
final class RealCoreRetentionWorld {
    static let audioSHA256 = "0b5f1b0f6c2a4d0d9e8d6f6e0f7b8a1c2d3e4f5061728394a5b6c7d8e9f00112"

    let layout: ProtectedStorageLayout
    private var sessions: [RealCoreRetentionSession] = []

    var databasePath: String { layout.databaseURL.path }

    init() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-retention-core-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
        guard ProtectedStorageService(layout: layout).prepare().isFullyConfigured else { throw StorageSetupFailed() }
        try CoreHandle(path: layout.databaseURL.path).close()
        try execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-01T07:00:00Z')")
    }

    func tearDown() {
        sessions.forEach { $0.close() }
        sessions = []
        try? FileManager.default.removeItem(at: layout.rootDirectory)
    }

    /// Opens the core over the on-disk store: a process start. Closing it is a process end.
    func openCore() throws -> RealCoreRetentionSession {
        let session = try RealCoreRetentionSession(path: databasePath)
        sessions.append(session)
        return session
    }

    func makeService(
        on session: RealCoreRetentionSession,
        fileSystem: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem(),
        retentionPeriodSeconds: UInt64 = 0,
        core: AudioRetentionCore? = nil
    ) -> AudioRetentionService {
        let core = core ?? session.client
        return AudioRetentionService(
            core: core, layout: layout, fileSystem: fileSystem,
            pressureProbe: FixedPressureProbe(pressure: .normal), retentionPeriodSeconds: retentionPeriodSeconds)
    }

    // MARK: Files

    func audioURL(_ fileName: String) -> URL {
        layout.directory(for: .finalizedAudio).appendingPathComponent(fileName)
    }

    func audioExists(_ fileName: String) -> Bool {
        FileManager.default.fileExists(atPath: audioURL(fileName).path)
    }

    func ledgerEntries() throws -> [AudioRetentionLedger.Entry] {
        try AudioRetentionLedger(layout: layout, fileSystem: FileManagerAudioRetentionFileSystem()).allEntries()
    }

    // MARK: Seeding

    struct SeededVoice {
        let itemID: String
        let fileName: String
        var reference: String { RetentionFixtures.audioReference(fileName) }
    }

    /// Imports a voice capture through the real core, with its finalized audio in protected storage. A non-nil
    /// `transcribedDaysAgo` also records a successful transcription that long ago.
    func seedVoice(captureID: String, transcribedDaysAgo: Int?) async throws -> SeededVoice {
        let fileName = "\(captureID).m4a"
        try Data("synthetic audio".utf8).write(to: audioURL(fileName))
        let reference = RetentionFixtures.audioReference(fileName)

        let importing = try openCore()
        let acknowledgment = try await importing.importCapture(CaptureRecord(
            captureID: captureID, text: nil, audioReference: reference,
            captureInstant: "2026-10-01T08:00:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: "route-personal", entryLocked: false,
            createdAt: "2026-10-01T08:00:01Z", sessionTopic: nil))
        importing.close()

        if let days = transcribedDaysAgo {
            let transcribedAt = ISO8601DateFormatter().string(from: Date().addingTimeInterval(-Double(days) * 86_400))
            let itemID = acknowledgment.itemID
            try execute("UPDATE items SET transcription_state = 'transcribed' WHERE item_id = '\(itemID)'")
            try execute(
                "INSERT INTO transcript_attachments (item_id, source_job_id, transcript_record_id, text, "
                    + "attached_revision, audio_reference, audio_sha256, recognizer, detected_language, confidence, "
                    + "transcribed_at) VALUES ('\(itemID)', 'job-\(itemID)', 'record-\(itemID)', 'water the ferns', "
                    + "1, '\(reference)', '\(Self.audioSHA256)', 'on-device-recognizer/1', 'en-US', 0.9, '\(transcribedAt)')")
        }
        return SeededVoice(itemID: acknowledgment.itemID, fileName: fileName)
    }

    /// What deletion leaves for the item. Safe while a core is open: it is one short write on a second connection.
    func markDeleted(itemID: String) {
        XCTAssertNoThrow(try execute("UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(itemID)'"))
    }

    /// The write that makes the item's audio protected, attempted on a second connection that waits only briefly. Returns
    /// the raw SQLite status, so a test can tell "blocked by the store-wide fence" from "done".
    func tryMarkDeleted(itemID: String) -> Int32 {
        var database: OpaquePointer?
        guard sqlite3_open(databasePath, &database) == SQLITE_OK else { return SQLITE_CANTOPEN }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 100)
        return sqlite3_exec(
            database, "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(itemID)'", nil, nil, nil)
    }

    func setCaptureAudioReference(captureID: String, to reference: String) {
        XCTAssertNoThrow(
            try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'"))
    }

    private func execute(_ statement: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5_000)
        let status = sqlite3_exec(database, statement, nil, nil, nil)
        XCTAssertEqual(status, SQLITE_OK, "\(String(cString: sqlite3_errmsg(database))): \(statement)")
        if status != SQLITE_OK { throw StorageSetupFailed() }
    }
}

/// One "process": a real core over the on-disk store with the production retention client attached to its single event
/// handler. Events arrive on the main queue; awaiting leaves the main queue free to deliver them.
final class RealCoreRetentionSession {
    let core: CoreHandle
    let client: CoreAudioRetentionClient
    private let lock = NSLock()
    private var otherEvents: [CoreEvent] = []
    private var nextOperationID: UInt64 = 1
    private var isClosed = false

    init(path: String) throws {
        core = try CoreHandle(path: path)
        client = CoreAudioRetentionClient(core: core)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            if self.client.handle(event) { return }
            self.lock.lock()
            self.otherEvents.append(event)
            self.lock.unlock()
        }
    }

    func importCapture(_ record: CaptureRecord) async throws -> IngressImportAcknowledgment {
        lock.lock()
        let operationID = nextOperationID
        nextOperationID += 1
        lock.unlock()
        try core.startImportForegroundIngress(operationID: operationID, record: record)
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline {
            lock.lock()
            let event = otherEvents.first { $0.operationID == operationID }
            lock.unlock()
            if let event { return try event.decode(IngressImportAcknowledgment.self) }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw RetentionFixtureError()
    }

    /// Ends the process: nothing but the files survives.
    func close() {
        guard !isClosed else { return }
        isClosed = true
        client.shutDown()
        core.close()
    }
}

/// The production client with test hooks around the removal: `beforeRemoval` runs just before the removal request is
/// sent, so a test can change the core's state after the sweep listed the audio and before the core acts on it,
/// `duringRemoval` runs inside the file effect, while the core holds its store-wide fence, and `interruptNextRemoval`
/// makes the next removal fail before it reaches the core, as a process that dies there would.
final class HookedRetentionCore: AudioRetentionCore, @unchecked Sendable {
    private let wrapped: AudioRetentionCore
    private let beforeRemoval: (@Sendable (String) -> Void)?
    private let duringRemoval: (@Sendable (String) -> Void)?
    private let lock = NSLock()
    private var interruptions = 0

    init(
        wrapped: AudioRetentionCore, beforeRemoval: (@Sendable (String) -> Void)? = nil,
        duringRemoval: (@Sendable (String) -> Void)? = nil
    ) {
        self.wrapped = wrapped
        self.beforeRemoval = beforeRemoval
        self.duringRemoval = duringRemoval
    }

    func interruptNextRemoval() {
        lock.lock()
        interruptions += 1
        lock.unlock()
    }

    func sweep(pressure: CoreRetentionPressure, periodSeconds: UInt64) async throws -> [AudioRetentionItem] {
        try await wrapped.sweep(pressure: pressure, periodSeconds: periodSeconds)
    }

    func authorizeRemoval(itemID: String, audioReference: String, periodSeconds: UInt64) async throws
        -> AudioRemovalDecision
    {
        try await wrapped.authorizeRemoval(
            itemID: itemID, audioReference: audioReference, periodSeconds: periodSeconds)
    }

    func removeAudio(itemID: String, audioReference: String, periodSeconds: UInt64, effect: () -> AudioRemovalEffect)
        async throws -> AudioRemovalResult
    {
        beforeRemoval?(itemID)
        lock.lock()
        let interrupted = interruptions > 0
        if interrupted { interruptions -= 1 }
        lock.unlock()
        if interrupted { throw CoreAudioRetentionClient.unreadableAnswer }
        let duringRemoval = duringRemoval
        return try await wrapped.removeAudio(
            itemID: itemID, audioReference: audioReference, periodSeconds: periodSeconds,
            effect: {
                duringRemoval?(itemID)
                return effect()
            })
    }
}

/// Records values from `@Sendable` hooks.
final class RecordedValues<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Value] = []

    func append(_ value: Value) {
        lock.lock()
        values.append(value)
        lock.unlock()
    }

    var all: [Value] {
        lock.lock()
        defer { lock.unlock() }
        return values
    }
}
