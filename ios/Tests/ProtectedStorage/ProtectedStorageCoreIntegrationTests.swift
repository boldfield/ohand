import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Persists captures through the real Rust core inside the protected store locations on the simulator and checks the
/// backup treatment the operating system reports for them. The simulator does not lock, so protection classes are
/// verified as requests made to the file system (and read back only on a device); lock behavior is device evidence.
final class ProtectedStorageCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var layout: ProtectedStorageLayout!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-protected-storage-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        layout = ProtectedStorageLayout(rootDirectory: root)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: layout.rootDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func makeCapture(id: String, text: String) -> CaptureRecord {
        CaptureRecord(
            captureID: id,
            text: text,
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

    /// Opens the core over the database inside the protected `Database` directory and records its events.
    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in
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

        func save(_ capture: CaptureRecord) throws -> SaveCaptureAcknowledgment {
            try run { try core.startSaveCapture(operationID: $0, capture: capture) }
                .decode(SaveCaptureAcknowledgment.self)
        }

        func read(_ captureID: String) throws -> CaptureRecord {
            try run { try core.startGetCapture(operationID: $0, captureID: captureID) }
                .decode(CaptureReadout.self).capture
        }
    }

    private func isExcludedFromBackup(_ url: URL) throws -> Bool? {
        try url.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup
    }

    // MARK: Persistence and backup treatment

    func testCapturePersistsInTheProtectedDatabaseDirectoryWithTheContractedPolicy() throws {
        let boundary = RecordingFileAttributeBoundary(wrapping: FileManagerAttributeBoundary())
        let service = ProtectedStorageService(layout: layout, boundary: boundary)

        let preparation = service.prepare()
        XCTAssertTrue(preparation.isFullyConfigured, "failures: \(preparation.failures)")

        let capture = makeCapture(id: "capture-protected-1", text: "buy oat milk")
        let first = try Session(path: layout.databaseURL.path)
        XCTAssertFalse(try first.save(capture).alreadySaved)
        XCTAssertEqual(try first.read(capture.captureID), capture)
        first.close()

        XCTAssertTrue(FileManager.default.fileExists(atPath: layout.databaseURL.path))
        XCTAssertEqual(layout.databaseURL.deletingLastPathComponent().path, layout.directory(for: .database).path)

        let reapplied = service.applyPolicy(to: .database)
        XCTAssertTrue(reapplied.isFullyConfigured, "failures: \(reapplied.failures)")
        XCTAssertEqual(
            boundary.protectionByPath[layout.databaseURL.path], .completeUntilFirstUserAuthentication,
            "the database file itself received the contracted class")

        let second = try Session(path: layout.databaseURL.path)
        defer { second.close() }
        XCTAssertEqual(try second.read(capture.captureID), capture, "the capture survived reopening the protected store")
    }

    func testEveryStoreDirectoryReportsTheContractedBackupTreatment() throws {
        let report = ProtectedStorageService(layout: layout).prepare()
        XCTAssertTrue(report.isFullyConfigured, "failures: \(report.failures)")

        for policy in StoreProtectionPolicy.all {
            let directory = layout.directory(for: policy.store)
            var isDirectory: ObjCBool = false
            XCTAssertTrue(FileManager.default.fileExists(atPath: directory.path, isDirectory: &isDirectory), "\(policy.store)")
            XCTAssertTrue(isDirectory.boolValue, "\(policy.store)")
            XCTAssertEqual(
                try isExcludedFromBackup(directory), policy.backup == .excluded,
                "\(policy.store) backup exclusion as reported by the file system")
        }
    }

    func testFilesCreatedInsideAnExcludedStoreAreCoveredByTheDirectoryExclusion() throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        let audioDirectory = layout.directory(for: .finalizedAudio)
        let audioFile = audioDirectory.appendingPathComponent("synthetic.m4a")
        try Data("synthetic audio bytes".utf8).write(to: audioFile)

        XCTAssertEqual(try isExcludedFromBackup(audioDirectory), true)
        let service = ProtectedStorageService(layout: layout)
        XCTAssertTrue(service.applyPolicy(to: .finalizedAudio).isFullyConfigured)
        XCTAssertEqual(try Data(contentsOf: audioFile), Data("synthetic audio bytes".utf8))
        XCTAssertEqual(try isExcludedFromBackup(audioDirectory), true)
    }

    #if !targetEnvironment(simulator)
    func testDeviceReportsTheContractedProtectionClassOnEveryStoreDirectory() throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        for policy in StoreProtectionPolicy.all {
            let values = try layout.directory(for: policy.store).resourceValues(forKeys: [.fileProtectionKey])
            XCTAssertEqual(values.fileProtection, policy.fileProtection, "\(policy.store)")
        }
    }
    #endif

    // MARK: Failure leaves data intact

    func testProtectionAndBackupFailuresLeaveStoredCapturesIntact() throws {
        let setup = ProtectedStorageService(layout: layout).prepare()
        XCTAssertTrue(setup.isFullyConfigured, "failures: \(setup.failures)")

        let capture = makeCapture(id: "capture-before-failure", text: "call the dentist tomorrow")
        let beforeFailure = try Session(path: layout.databaseURL.path)
        _ = try beforeFailure.save(capture)
        beforeFailure.close()

        let failing = RecordingFileAttributeBoundary(wrapping: FileManagerAttributeBoundary())
        failing.shouldFail = { call in
            switch call {
            case .setProtection, .setExcludedFromBackup:
                return true
            case .createDirectory, .descendants:
                return false
            }
        }
        let report = ProtectedStorageService(layout: layout, boundary: failing).prepare()

        XCTAssertFalse(report.isFullyConfigured)
        XCTAssertEqual(report.outcome(for: .database)?.directoryAvailable, true)
        XCTAssertFalse(report.outcome(for: .database)?.failures.isEmpty ?? true)
        XCTAssertTrue(FileManager.default.fileExists(atPath: layout.databaseURL.path), "the database was not removed")

        let afterFailure = try Session(path: layout.databaseURL.path)
        defer { afterFailure.close() }
        XCTAssertEqual(try afterFailure.read(capture.captureID), capture, "the capture is readable after the failed setup")

        let later = makeCapture(id: "capture-after-failure", text: "capture continues after a setup failure")
        XCTAssertFalse(try afterFailure.save(later).alreadySaved, "capture stays durable when protection setup failed")
        XCTAssertEqual(try afterFailure.read(later.captureID), later)
    }

    func testBlockedStoreDirectoryIsReportedWithoutTouchingTheBlockingFileOrOtherStores() throws {
        let blockingFile = URL(fileURLWithPath: layout.directory(for: .database).path)
        let blockingBytes = Data("not a directory; an earlier file that must survive".utf8)
        try blockingBytes.write(to: blockingFile)

        let report = ProtectedStorageService(layout: layout).prepare()

        let database = report.outcome(for: .database)
        XCTAssertEqual(database?.directoryAvailable, false)
        XCTAssertEqual(database?.failures.map { $0.step }, [.createDirectory])
        XCTAssertEqual(try Data(contentsOf: blockingFile), blockingBytes, "existing content was not overwritten or removed")
        for store in ProtectedStore.allCases where store != .database {
            XCTAssertEqual(report.outcome(for: store)?.isFullyConfigured, true, "\(store)")
        }
    }
}
