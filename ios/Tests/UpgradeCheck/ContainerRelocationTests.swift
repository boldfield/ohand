import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
#if DEBUG
@testable import OhAnd
#endif

private final class RefusingProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// iOS does not promise the app's container keeps its absolute path across an install, so the app must resolve its
/// store, audio and evidence locations from the container at run time and never persist the path.
final class ContainerRelocationTests: XCTestCase {
    private var parent: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        parent = FileManager.default.temporaryDirectory
            .appendingPathComponent("container-relocation-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: parent)
        try super.tearDownWithError()
    }

    func testAMovedContainerKeepsItsDataAndHoldsNoAbsolutePathOfItsOldLocation() throws {
        let oldRoot = parent.appendingPathComponent("old-container", isDirectory: true)
        let newRoot = parent.appendingPathComponent("new-container-elsewhere", isDirectory: true)
        try FileManager.default.createDirectory(at: oldRoot, withIntermediateDirectories: true)

        let itemID: String
        do {
            let root = try makeRoot(at: oldRoot)
            defer { root.shutDown() }
            var outcome: CompositionSaveOutcome?
            root.saveText(captureID: UUID().uuidString.lowercased(), text: "synthetic relocation capture", capturedAt: Date()) {
                outcome = $0
            }
            XCTAssertTrue(pump { outcome != nil }, "the save completes")
            guard case .saved(_, let savedID, _)? = outcome else { return XCTFail("expected a confirmed save") }
            itemID = savedID
        }
        // Production staging store: a staged record that names recorded audio, finalized into the audio store.
        let audioBytes = Data("synthetic relocation audio".utf8)
        let audioRecord = try stageFinalizedAudio(captureID: "relocation-audio", bytes: audioBytes, at: oldRoot)

        try FileManager.default.moveItem(at: oldRoot, to: newRoot)

        // Nothing under the moved tree records where it used to live (a path under the temporary directory).
        let oldPathBytes = Data(oldRoot.path.utf8)
        let enumerator = try XCTUnwrap(FileManager.default.enumerator(at: newRoot, includingPropertiesForKeys: [.isRegularFileKey]))
        for case let file as URL in enumerator {
            guard (try? file.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true else { continue }
            let contents = try Data(contentsOf: file)
            XCTAssertNil(contents.range(of: oldPathBytes), "\(file.lastPathComponent) holds the old container path")
        }

        // The audio reference the core keeps is relative, and resolves, loads and plays from the moved root.
        let movedStore = IngressStagingStore(
            layout: ProtectedStorageLayout(rootDirectory: newRoot), fileSystem: FileManagerIngressFileSystem())
        let reference = try XCTUnwrap(audioRecord.coreAudioReference)
        XCTAssertFalse(reference.hasPrefix("/"), "the audio reference is not an absolute path")
        let resolved = try XCTUnwrap(movedStore.finalizedAudioURL(forReference: reference))
        XCTAssertTrue(resolved.path.hasPrefix(newRoot.path), "the audio resolves under the moved root")
        XCTAssertEqual(try Data(contentsOf: resolved), audioBytes)
        XCTAssertEqual(try movedStore.load(captureID: audioRecord.captureID), audioRecord, "the staged record loads there")

        let reopened = try makeRoot(at: newRoot)
        defer { reopened.shutDown() }
        XCTAssertEqual(try count(of: itemID, in: newRoot), "1", "the capture is still there at the new location")
    }

    #if DEBUG
    /// The evidence file (the upgrade report) is written where the app's own run-time lookup says its container is, so
    /// moving the container moves the evidence location with it and nothing records the old one.
    func testTheEvidenceFileIsResolvedFromTheContainerAtRunTime() throws {
        let oldRoot = parent.appendingPathComponent("old-evidence-container", isDirectory: true)
        let newRoot = parent.appendingPathComponent("new-evidence-container-elsewhere", isDirectory: true)
        let report = UpgradeReport(
            schema: UpgradeReport.schema, generatedAt: "2026-10-27T10:00:00Z", build: nil, recentItemCount: 0, items: [],
            probes: [], providers: [], pendingReminderRequests: [], otherPendingRequestCount: 0)
        let written = try UpgradeReportRequest.reportURL(fileManager: ContainerRootFileManager(root: oldRoot))
        try report.encoded().write(to: written, options: .atomic)
        try FileManager.default.moveItem(at: oldRoot, to: newRoot)

        let contents = try Data(contentsOf: written.relocated(from: oldRoot, to: newRoot))
        XCTAssertNil(contents.range(of: Data(oldRoot.path.utf8)), "the evidence file holds the old container path")
        let resolved = try UpgradeReportRequest.reportURL(fileManager: ContainerRootFileManager(root: newRoot))
        XCTAssertTrue(resolved.path.hasPrefix(newRoot.path))
        XCTAssertEqual(try Data(contentsOf: resolved), contents, "the evidence resolves and loads from the moved root")
    }
    #endif

    private func stageFinalizedAudio(captureID: String, bytes: Data, at storageRoot: URL) throws -> IngressRecord {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        let store = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        let handoff = IngressAudioHandoff(inProgressFileName: "\(captureID).m4a", finalizedFileName: "\(captureID).m4a")
        try bytes.write(to: store.inProgressAudioURL(fileName: handoff.inProgressFileName))
        let record = IngressRecord(
            captureID: captureID, text: nil, audio: handoff,
            context: IngressCaptureContext(
                captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
                calendar: "gregorian", itemScope: "personal", routeID: "route-container-relocation", entryLocked: false,
                createdAt: "2026-10-08T09:30:01Z", sessionTopic: nil))
        XCTAssertEqual(try store.stage(record), .created)
        XCTAssertEqual(try store.finalizeAudio(handoff), .ready(moved: true))
        return record
    }

    private func makeRoot(at storageRoot: URL) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-container-relocation", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: RefusingProviderSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)))
    }

    private func count(of itemID: String, in storageRoot: URL) throws -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(
            sqlite3_prepare_v2(database, "SELECT COUNT(*) FROM items WHERE item_id = '\(itemID)'", -1, &statement, nil),
            SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }
}

#if DEBUG
/// A file manager whose Application Support directory lives under a chosen root, standing in for an app container.
private final class ContainerRootFileManager: FileManager {
    private let root: URL

    init(root: URL) {
        self.root = root
        super.init()
    }

    override func url(
        for directory: FileManager.SearchPathDirectory, in domain: FileManager.SearchPathDomainMask, appropriateFor url: URL?,
        create shouldCreate: Bool
    ) throws -> URL {
        let support = root.appendingPathComponent("Library/Application Support", isDirectory: true)
        if shouldCreate { try createDirectory(at: support, withIntermediateDirectories: true) }
        return support
    }
}

private extension URL {
    func relocated(from oldRoot: URL, to newRoot: URL) -> URL {
        URL(fileURLWithPath: newRoot.path + path.dropFirst(oldRoot.path.count))
    }
}
#endif
