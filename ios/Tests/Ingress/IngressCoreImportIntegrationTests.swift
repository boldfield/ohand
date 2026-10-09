import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives `ForegroundIngressService` against the real Rust core on the simulator, over the protected store locations.
/// A process interruption is simulated by discarding the service and closing the core, then reopening both from disk:
/// nothing but the files survives, exactly as after a kill. The simulator does not lock, so lock-state behavior is
/// device evidence; protection classes are verified as requests made to the file system.
final class IngressCoreImportIntegrationTests: IngressStorageTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var runtimes: [Runtime] = []

    /// One "process": a core over the protected database, the importer bridging to it, and the service on top.
    private final class Runtime {
        let core: CoreHandle
        private(set) var service: ForegroundIngressService?

        init(layout: ProtectedStorageLayout, fileSystem: IngressFileSystem, protection: ProtectedStorageService?) throws {
            core = try CoreHandle(path: layout.databaseURL.path)
            do {
                let importer = try CoreIngressImporter(core: core)
                service = try ForegroundIngressService(
                    layout: layout, importer: importer, fileSystem: fileSystem, protection: protection)
            } catch {
                core.close()
                throw error
            }
        }

        /// Ends the process: the service (and its writer lock) goes away, then the core closes.
        func terminate() {
            service = nil
            core.close()
        }
    }

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        try createStoreWithPersonalRoute()
    }

    override func tearDown() {
        runtimes.forEach { $0.terminate() }
        runtimes = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: Helpers

    private func createStoreWithPersonalRoute() throws {
        let core = try CoreHandle(path: layout.databaseURL.path)
        core.close()
        try withDatabase { database in
            let statement = "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')"
            XCTAssertEqual(sqlite3_exec(database, statement, nil, nil, nil), SQLITE_OK)
        }
    }

    private func withDatabase(_ body: (OpaquePointer?) throws -> Void) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        try body(database)
    }

    private func count(_ table: String, whereCaptureID captureID: String) throws -> Int {
        var total = -1
        try withDatabase { database in
            var statement: OpaquePointer?
            XCTAssertEqual(sqlite3_prepare_v2(
                database, "SELECT COUNT(*) FROM \(table) WHERE capture_id = ?", -1, &statement, nil), SQLITE_OK)
            defer { sqlite3_finalize(statement) }
            sqlite3_bind_text(statement, 1, captureID, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
            XCTAssertEqual(sqlite3_step(statement), SQLITE_ROW)
            total = Int(sqlite3_column_int(statement, 0))
        }
        return total
    }

    private func storedText(captureID: String) throws -> String? {
        var text: String?
        try withDatabase { database in
            var statement: OpaquePointer?
            XCTAssertEqual(sqlite3_prepare_v2(
                database, "SELECT text FROM captures WHERE capture_id = ?", -1, &statement, nil), SQLITE_OK)
            defer { sqlite3_finalize(statement) }
            sqlite3_bind_text(statement, 1, captureID, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
            if sqlite3_step(statement) == SQLITE_ROW, let raw = sqlite3_column_text(statement, 0) {
                text = String(cString: raw)
            }
        }
        return text
    }

    private func storedAudioReference(captureID: String) throws -> String? {
        var reference: String?
        try withDatabase { database in
            var statement: OpaquePointer?
            XCTAssertEqual(sqlite3_prepare_v2(
                database, "SELECT audio_reference FROM captures WHERE capture_id = ?", -1, &statement, nil), SQLITE_OK)
            defer { sqlite3_finalize(statement) }
            sqlite3_bind_text(statement, 1, captureID, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
            if sqlite3_step(statement) == SQLITE_ROW, let raw = sqlite3_column_text(statement, 0) {
                reference = String(cString: raw)
            }
        }
        return reference
    }

    private func launch(
        fileSystem: IngressFileSystem = FileManagerIngressFileSystem(),
        protection: ProtectedStorageService? = nil
    ) throws -> Runtime {
        let runtime = try Runtime(layout: layout, fileSystem: fileSystem, protection: protection)
        runtimes.append(runtime)
        return runtime
    }

    private func waitFor<Value>(_ start: (@escaping (Value) -> Void) -> Void) throws -> Value {
        var result: Value?
        start { result = $0 }
        let deadline = Date().addingTimeInterval(30)
        while result == nil && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return try XCTUnwrap(result, "the operation never completed")
    }

    private func submit(_ runtime: Runtime, _ record: IngressRecord) throws -> IngressOutcome {
        let service = try XCTUnwrap(runtime.service)
        return try waitFor { service.submit(record, completion: $0) }
    }

    private func recover(_ runtime: Runtime) throws -> IngressRecoveryReport {
        let service = try XCTUnwrap(runtime.service)
        return try waitFor { service.recover(completion: $0) }
    }

    private func savedAcknowledgment(_ outcome: IngressOutcome, file: StaticString = #filePath, line: UInt = #line) throws -> IngressSaveAcknowledgment {
        guard case .saved(let acknowledgment) = outcome else {
            XCTFail("expected saved, got \(outcome)", file: file, line: line)
            throw CancellationError()
        }
        return acknowledgment
    }

    // MARK: Text

    func testTextIsSavedOnlyAfterTheCoreCommitAndTheStagingRecordIsThenRemoved() throws {
        let runtime = try launch()

        let acknowledgment = try savedAcknowledgment(try submit(runtime, textRecord("capture-1")))

        XCTAssertFalse(acknowledgment.itemID.isEmpty)
        XCTAssertFalse(acknowledgment.alreadyImported)
        XCTAssertTrue(acknowledgment.stagingCleanedUp)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1, "the import committed an item")
        XCTAssertEqual(try storedText(captureID: "capture-1"), "buy oat milk")
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testRepeatedHandoffAfterCleanupConvergesOnTheSameItem() throws {
        let runtime = try launch()
        let first = try savedAcknowledgment(try submit(runtime, textRecord("capture-1")))

        let second = try savedAcknowledgment(try submit(runtime, textRecord("capture-1")))

        XCTAssertEqual(second.itemID, first.itemID)
        XCTAssertTrue(second.alreadyImported)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1, "no duplicate item")
        XCTAssertEqual(try count("captures", whereCaptureID: "capture-1"), 1, "no duplicate capture")
    }

    func testDifferentContentForASavedCaptureIDIsRefusedAndTheStoredCaptureIsUntouched() throws {
        let runtime = try launch()
        _ = try savedAcknowledgment(try submit(runtime, textRecord("capture-1", text: "original words")))

        let outcome = try submit(runtime, textRecord("capture-1", text: "different words"))

        guard case .keptForRetry(_, .conflictingReuse) = outcome else { return XCTFail("got \(outcome)") }
        XCTAssertEqual(try storedText(captureID: "capture-1"), "original words")
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1)
        XCTAssertTrue(exists(recordURL("capture-1")), "the refused input stays recoverable, never silently dropped")
    }

    func testCoreRejectionKeepsTheInputAndNeverReportsSaved() throws {
        let runtime = try launch()
        var unknownRoute = textRecord("capture-1")
        unknownRoute.context.routeID = "route-missing"

        let outcome = try submit(runtime, unknownRoute)

        guard case .keptForRetry(_, .rejected(let code)) = outcome else { return XCTFail("got \(outcome)") }
        XCTAssertEqual(code, "ingress_unknown_route")
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 0)
        XCTAssertEqual(try count("captures", whereCaptureID: "capture-1"), 0)
        XCTAssertTrue(exists(recordURL("capture-1")))
    }

    // MARK: Process interruption

    func testInterruptionWhileTheCoreIsUnavailableIsRecoveredAfterRelaunch() throws {
        let beforeInterruption = try launch()
        beforeInterruption.core.close()

        let outcome = try submit(beforeInterruption, textRecord("capture-1"))
        guard case .keptForRetry(_, .coreUnavailable) = outcome else { return XCTFail("got \(outcome)") }
        XCTAssertTrue(exists(recordURL("capture-1")), "the input is durable although nothing was imported")
        beforeInterruption.terminate()

        let relaunched = try launch()
        let report = try recover(relaunched)

        XCTAssertEqual(report.entries.map { $0.captureID }, ["capture-1"])
        _ = try savedAcknowledgment(try XCTUnwrap(report.entries.first).outcome)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1)
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testInterruptionAfterCommitButBeforeCleanupDoesNotDuplicateTheItem() throws {
        let failingFileSystem = FailingIngressFileSystem()
        failingFileSystem.failing = [.remove]
        let beforeInterruption = try launch(fileSystem: failingFileSystem)

        let acknowledgment = try savedAcknowledgment(try submit(beforeInterruption, textRecord("capture-1")))
        XCTAssertFalse(acknowledgment.stagingCleanedUp)
        XCTAssertTrue(exists(recordURL("capture-1")))
        beforeInterruption.terminate()

        let relaunched = try launch()
        let report = try recover(relaunched)

        let recovered = try savedAcknowledgment(try XCTUnwrap(report.entries.first).outcome)
        XCTAssertEqual(recovered.itemID, acknowledgment.itemID)
        XCTAssertTrue(recovered.alreadyImported)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1)
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testRecoveryIsRepeatable() throws {
        let runtime = try launch()
        XCTAssertTrue(try recover(runtime).entries.isEmpty)
        _ = try savedAcknowledgment(try submit(runtime, textRecord("capture-1")))
        XCTAssertTrue(try recover(runtime).entries.isEmpty)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-1"), 1)
    }

    // MARK: Audio

    func testAudioReferenceStaysInsideTheProtectedFinalizedStoreAndValidAfterCleanup() throws {
        let boundary = RecordingFileAttributeBoundary(wrapping: FileManagerAttributeBoundary())
        let protection = ProtectedStorageService(layout: layout, boundary: boundary)
        let runtime = try launch(protection: protection)
        let record = try audioRecord("capture-audio", bytes: Data("synthetic recording".utf8))

        let acknowledgment = try savedAcknowledgment(try submit(runtime, record))

        XCTAssertTrue(acknowledgment.audioProtectionFailures.isEmpty)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-audio"), 1)
        let reference = try XCTUnwrap(try storedAudioReference(captureID: "capture-audio"))
        XCTAssertEqual(reference, "FinalizedAudio/capture-audio.m4a")
        XCTAssertFalse(exists(recordURL("capture-audio")), "the staging record is gone")
        XCTAssertFalse(exists(inProgressAudioURL("capture-audio.m4a")), "the audio was moved, not copied")
        let resolved = layout.rootDirectory.appendingPathComponent(reference)
        XCTAssertEqual(try Data(contentsOf: resolved), Data("synthetic recording".utf8))

        let policy = StoreProtectionPolicy.policy(for: .finalizedAudio)
        XCTAssertEqual(boundary.protectionByPath[resolved.path], policy.fileProtection, "the moved file received the finalized class")
        XCTAssertEqual(boundary.exclusionByPath[layout.directory(for: .finalizedAudio).path], policy.backup == .excluded)
    }

    func testAudioReferenceSurvivesMovingTheStorageRoot() throws {
        let runtime = try launch()
        _ = try savedAcknowledgment(try submit(runtime, try audioRecord("capture-audio")))
        let reference = try XCTUnwrap(try storedAudioReference(captureID: "capture-audio"))
        runtime.terminate()

        let movedRoot = layout.rootDirectory.deletingLastPathComponent()
            .appendingPathComponent("ohand-ingress-moved-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.moveItem(at: layout.rootDirectory, to: movedRoot)
        defer { try? FileManager.default.moveItem(at: movedRoot, to: layout.rootDirectory) }

        XCTAssertTrue(exists(movedRoot.appendingPathComponent(reference)), "a store-relative reference follows the container")
    }

    func testInterruptionBetweenAudioMoveAndImportKeepsTheAudioAndRecoversOnce() throws {
        let beforeInterruption = try launch()
        beforeInterruption.core.close()
        let record = try audioRecord("capture-audio")

        guard case .keptForRetry(_, .coreUnavailable) = try submit(beforeInterruption, record) else { return XCTFail("expected kept") }
        XCTAssertTrue(exists(finalizedAudioURL("capture-audio.m4a")))
        XCTAssertFalse(exists(inProgressAudioURL("capture-audio.m4a")))
        beforeInterruption.terminate()

        let relaunched = try launch()
        let report = try recover(relaunched)

        _ = try savedAcknowledgment(try XCTUnwrap(report.entries.first).outcome)
        XCTAssertEqual(try count("items", whereCaptureID: "capture-audio"), 1)
        XCTAssertEqual(try storedAudioReference(captureID: "capture-audio"), "FinalizedAudio/capture-audio.m4a")
        XCTAssertTrue(exists(finalizedAudioURL("capture-audio.m4a")))
    }

    // MARK: One writer

    func testOnlyOneForegroundWriterExistsAndNoOtherProcessKindCanBecomeOne() throws {
        let runtime = try launch()

        XCTAssertThrowsError(try Runtime(layout: layout, fileSystem: FileManagerIngressFileSystem(), protection: nil)) { error in
            XCTAssertEqual(error as? IngressWriterLock.AcquisitionError, .anotherWriterActive)
        }
        withExtendedLifetime(runtime) {}
    }
}
