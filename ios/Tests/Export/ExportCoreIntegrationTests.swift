import CryptoKit
import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// One "process": a real core over the on-disk store with the production export client attached to its single event
/// handler. Events arrive on the main queue, where XCTest runs synchronous tests, so waiting pumps the run loop.
final class RealCoreExportSession {
    let core: CoreHandle
    let client: CoreExportClient
    private(set) var otherEvents: [CoreEvent] = []
    private var nextOperationID: UInt64 = 1
    private var isClosed = false

    init(path: String) throws {
        core = try CoreHandle(path: path)
        client = CoreExportClient(core: core)
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
        let box = ExportAnswerBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    func importCapture(_ record: CaptureRecord) throws -> String {
        let operationID = nextOperationID
        nextOperationID += 1
        try core.startImportForegroundIngress(operationID: operationID, record: record)
        XCTAssertTrue(pump(until: { self.otherEvents.contains { $0.operationID == operationID } }))
        let event = try XCTUnwrap(otherEvents.first { $0.operationID == operationID })
        return try event.decode(IngressImportAcknowledgment.self).itemID
    }

    /// Ends the process: nothing but the files survives.
    func close() {
        guard !isClosed else { return }
        isClosed = true
        client.shutDown()
        core.close()
    }
}

private final class ExportAnswerBox<Value>: @unchecked Sendable {
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

/// Drives `ExportService` through the production `CoreExportClient` into the real Rust core over a file store, with real
/// protected-storage files. Everything is synthetic. The deletion lifecycle state and the provider profile row (a
/// sentinel endpoint and credential reference that must never appear in an export) are seeded with SQL because the core
/// has no native entry point for either yet.
final class ExportCoreIntegrationTests: XCTestCase {
    private static let personalText = "synthetic personal note"
    private static let workText = "synthetic work note"
    private static let deletedText = "synthetic deleted-secret-text"
    private static let endpointSentinel = "https://private-endpoint.invalid/v1"
    private static let credentialSentinel = "keychain:credential-sentinel"

    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var worlds: [RealCoreDeletionWorld] = []
    private var sessions: [RealCoreExportSession] = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDown() {
        sessions.forEach { $0.close() }
        sessions = []
        worlds.forEach { $0.tearDown() }
        worlds = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: Helpers

    private struct Seeded {
        let world: RealCoreDeletionWorld
        let session: RealCoreExportSession
        let readSession: ReadAuthenticationSession
    }

    /// Personal: a text note, a voice note with audio on disk, and a voice note whose audio file is gone. Work: a text
    /// note. Deleted: a text note whose text is still in the store and a voice note with audio still on disk.
    private func seed() throws -> Seeded {
        let world = try RealCoreDeletionWorld()
        worlds.append(world)
        try Self.execute(
            world.databasePath,
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-work', 'route-work', 'work', '[]', '2026-10-08T09:00:00Z')")
        try Self.execute(
            world.databasePath,
            "INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, credential_ref, "
                + "timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at) "
                + "VALUES ('profile-v1', 'profile', 'self_hosted', '\(Self.endpointSentinel)', 'model-x', "
                + "'\(Self.credentialSentinel)', 30, '{}', '[]', '[]', '2026-10-08T09:00:00Z')")

        _ = try world.importVoiceCapture(captureID: "cap-voice")
        _ = try world.importVoiceCapture(captureID: "cap-lost")
        let deletedVoiceItem = try world.importVoiceCapture(captureID: "cap-gone")

        let importing = try RealCoreExportSession(path: world.databasePath)
        _ = try importing.importCapture(textCapture("note-1", Self.personalText, scope: "personal"))
        _ = try importing.importCapture(textCapture("work-1", Self.workText, scope: "work"))
        let deletedTextItem = try importing.importCapture(textCapture("gone-1", Self.deletedText, scope: "personal"))
        importing.close()

        try Self.execute(world.databasePath, "UPDATE items SET lifecycle_state = 'deleted' WHERE item_id = '\(deletedTextItem)'")
        try world.seedDeletedState(itemID: deletedVoiceItem, captureID: "cap-gone", reminderID: "rem-gone")
        try FileManager.default.removeItem(at: world.url(.finalizedAudio, "cap-lost.m4a"))

        let session = try RealCoreExportSession(path: world.databasePath)
        sessions.append(session)
        let readSession = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        readSession.authenticate(reason: "test") { _ in }
        XCTAssertEqual(readSession.scope, .authenticated)
        return Seeded(world: world, session: session, readSession: readSession)
    }

    private func textCapture(_ captureID: String, _ text: String, scope: String) -> CaptureRecord {
        CaptureRecord(
            captureID: captureID, text: text, captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC",
            utcOffsetMinutes: 0, locale: "en_US", calendar: "gregorian", itemScope: scope, routeID: "route-\(scope)",
            entryLocked: false, createdAt: "2026-10-08T09:31:00Z", sessionTopic: nil)
    }

    private func makeService(_ seeded: Seeded) -> ExportService {
        ExportService(core: seeded.session.client, session: seeded.readSession, layout: seeded.world.layout)
    }

    private func prepare(
        _ seeded: Seeded, _ service: ExportService, scopes: [CoreExportScope], includeAudio: Bool = false
    ) throws -> PreparedExport {
        let selection = ExportSelection(scopes: scopes, includeAudio: includeAudio)
        return try seeded.session.run { try await service.prepareExport(selection) }
    }

    private struct ParsedExport {
        let text: String
        let records: [[String: Any]]

        var items: [[String: Any]] { records.filter { ($0["record"] as? String) == "item" } }
    }

    private func parse(_ url: URL) throws -> ParsedExport {
        let text = try String(contentsOf: url, encoding: .utf8)
        let records = try text.split(separator: "\n").map { line -> [String: Any] in
            try XCTUnwrap(try JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])
        }
        return ParsedExport(text: text, records: records)
    }

    private func exportRemnants(_ world: RealCoreDeletionWorld) -> [String] {
        let subpaths = (try? FileManager.default.subpathsOfDirectory(atPath: world.layout.rootDirectory.path)) ?? []
        return subpaths.filter { $0.hasPrefix("Temporary/" + ExportService.exportsDirectoryName + "/") }.sorted()
    }

    // MARK: Scope and secrets

    func testExportStaysWithinThePersonalScopeAndCarriesNoSecrets() throws {
        let seeded = try seed()
        let prepared = try prepare(seeded, makeService(seeded), scopes: [.personal])

        let export = try parse(prepared.exportFile)
        XCTAssertEqual(export.records.first?["record"] as? String, "header")
        XCTAssertEqual(export.records.last?["record"] as? String, "footer")
        XCTAssertEqual(export.items.count, 3, "a text note and two voice notes")
        XCTAssertEqual(prepared.itemCount, 3)
        XCTAssertTrue(export.items.allSatisfy { ($0["scope"] as? String) == "personal" })
        XCTAssertTrue(export.text.contains(Self.personalText))
        for forbidden in [
            Self.workText, Self.deletedText, "gone-1", "cap-gone", Self.endpointSentinel, Self.credentialSentinel,
            "FinalizedAudio/",
        ] {
            XCTAssertFalse(export.text.contains(forbidden), "the export must not contain \(forbidden)")
        }
        XCTAssertEqual(prepared.shareItems, [prepared.exportFile], "audio was not requested")
        XCTAssertEqual(prepared.audio, ExportAudioReport(requested: false, includedCaptureIDs: [], unavailable: []))
    }

    func testWorkScopeAndBothScopesAreExactlyWhatWasSelected() throws {
        let seeded = try seed()
        let service = makeService(seeded)

        let work = try parse(try prepare(seeded, service, scopes: [.work]).exportFile)
        XCTAssertEqual(work.items.count, 1)
        XCTAssertTrue(work.text.contains(Self.workText))
        XCTAssertFalse(work.text.contains(Self.personalText))

        let both = try parse(try prepare(seeded, service, scopes: [.personal, .work]).exportFile)
        XCTAssertEqual(both.items.count, 4)
        XCTAssertTrue(both.text.contains(Self.personalText) && both.text.contains(Self.workText))
        XCTAssertFalse(both.text.contains(Self.deletedText))
        XCTAssertFalse(both.text.contains(Self.credentialSentinel) || both.text.contains(Self.endpointSentinel))
    }

    func testFooterDigestCoversEverythingBeforeTheFooter() throws {
        let seeded = try seed()
        let prepared = try prepare(seeded, makeService(seeded), scopes: [.personal, .work])

        let export = try parse(prepared.exportFile)
        let footerRange = try XCTUnwrap(export.text.range(of: "{\"record\":\"footer\"", options: .backwards))
        let digest = SHA256.hash(data: Data(export.text[..<footerRange.lowerBound].utf8))
            .map { String(format: "%02x", $0) }.joined()
        XCTAssertEqual(export.records.last?["sha256"] as? String, digest)
    }

    // MARK: Audio

    func testAudioIsIncludedOnlyWhenRequestedAndMissingAudioIsReported() throws {
        let seeded = try seed()
        let world = seeded.world
        let prepared = try prepare(seeded, makeService(seeded), scopes: [.personal], includeAudio: true)

        XCTAssertEqual(prepared.audio.requested, true)
        XCTAssertEqual(prepared.audio.includedCaptureIDs, ["cap-voice"])
        XCTAssertEqual(prepared.audioFiles.map { $0.lastPathComponent }, ["cap-voice.m4a"])
        XCTAssertEqual(try String(contentsOf: try XCTUnwrap(prepared.audioFiles.first)), "synthetic audio")
        XCTAssertEqual(prepared.audio.unavailable.map { $0.captureID }, ["cap-lost"])
        XCTAssertEqual(prepared.audio.unavailable.map { $0.reason }, [.fileMissing])
        let export = try parse(prepared.exportFile)
        XCTAssertTrue(export.text.contains("FinalizedAudio/cap-voice.m4a"))
        XCTAssertFalse(export.text.contains("cap-gone"), "a deleted item's audio is not named")
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-gone.m4a"), "the fixture left the deleted audio on disk")
        XCTAssertFalse(
            prepared.audioFiles.contains { $0.lastPathComponent.hasPrefix("cap-gone") },
            "a deleted item's audio is not copied")
    }

    // MARK: Share lifecycle against the real core

    func testSharingNeverDeletesSourcesAndLeavesNoTemporaries() throws {
        let seeded = try seed()
        let world = seeded.world
        let service = makeService(seeded)
        let prepared = try prepare(seeded, service, scopes: [.personal], includeAudio: true)
        XCTAssertFalse(exportRemnants(world).isEmpty)

        let outcome = service.finishShare(exportID: prepared.id, ending: .completed)

        XCTAssertEqual(outcome, ExportShareOutcome(result: .shared, temporariesRemoved: true))
        XCTAssertEqual(exportRemnants(world), [])
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-voice.m4a"))
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-gone.m4a"))
        XCTAssertTrue(FileManager.default.fileExists(atPath: world.databasePath))
    }

    func testRelockAfterARealExportRemovesTheTemporaries() throws {
        let seeded = try seed()
        let service = makeService(seeded)
        let prepared = try prepare(seeded, service, scopes: [.personal, .work], includeAudio: true)

        seeded.readSession.relock(.enteredBackground)

        XCTAssertEqual(exportRemnants(seeded.world), [])
        XCTAssertEqual(service.finishShare(exportID: prepared.id, ending: .completed).result, .interrupted)
        XCTAssertTrue(seeded.world.exists(.finalizedAudio, "cap-voice.m4a"))
    }

    func testNextLaunchSweepRemovesWhatAnInterruptedShareLeft() throws {
        let seeded = try seed()
        _ = try prepare(seeded, makeService(seeded), scopes: [.personal], includeAudio: true)
        XCTAssertFalse(exportRemnants(seeded.world).isEmpty)

        let nextLaunch = makeService(seeded)
        let report = nextLaunch.sweepAtLaunch()

        XCTAssertEqual(report.failedCount, 0)
        XCTAssertGreaterThanOrEqual(report.removedCount, 1)
        XCTAssertEqual(exportRemnants(seeded.world), [])
        XCTAssertTrue(seeded.world.exists(.finalizedAudio, "cap-voice.m4a"))
    }

    // MARK: Core-side enforcement

    func testTheCoreRefusesACaptureOnlyExportAndCreatesNoFile() throws {
        let seeded = try seed()
        let output = FileManager.default.temporaryDirectory.appendingPathComponent("ohand-export-refused-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: output) }
        let request = CoreExportRequest(
            readAuthentication: .captureOnly, scopes: [.personal], audio: .excluded, outputPath: output.path)
        let client = seeded.session.client

        XCTAssertThrowsError(try seeded.session.run { try await client.export(request) }) { error in
            let failure = error as? CoreFailure
            XCTAssertEqual(failure?.code, "export_not_authorized")
            XCTAssertEqual(failure?.errorClass, .unauthorized)
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: output.path))
    }

    func testTheCoreNeverReplacesAnExistingFile() throws {
        let seeded = try seed()
        let output = FileManager.default.temporaryDirectory.appendingPathComponent("ohand-export-existing-\(UUID().uuidString)")
        try Data("keep me".utf8).write(to: output)
        defer { try? FileManager.default.removeItem(at: output) }
        let request = CoreExportRequest(
            readAuthentication: .authenticated, scopes: [.personal], audio: .excluded, outputPath: output.path)
        let client = seeded.session.client

        XCTAssertThrowsError(try seeded.session.run { try await client.export(request) }) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "export_write_failed")
        }
        XCTAssertEqual(try String(contentsOf: output), "keep me")
    }

    func testShutDownFailsLaterExports() throws {
        let seeded = try seed()
        seeded.session.client.shutDown()
        let service = makeService(seeded)

        XCTAssertThrowsError(try prepare(seeded, service, scopes: [.personal])) { error in
            guard case .core(let failure)? = error as? ExportFailure else { return XCTFail("expected a core failure") }
            XCTAssertEqual(failure.code, "client_shut_down")
        }
        XCTAssertEqual(exportRemnants(seeded.world), [])
    }

    // MARK: SQLite

    private static func execute(_ databasePath: String, _ statement: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(databasePath, &database) == SQLITE_OK else { throw StorageSetupFailed() }
        defer { sqlite3_close(database) }
        let status = sqlite3_exec(database, statement, nil, nil, nil)
        XCTAssertEqual(status, SQLITE_OK, "\(String(cString: sqlite3_errmsg(database))): \(statement)")
        if status != SQLITE_OK { throw StorageSetupFailed() }
    }
}
