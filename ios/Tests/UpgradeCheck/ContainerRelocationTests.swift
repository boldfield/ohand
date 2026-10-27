import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

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

        try FileManager.default.moveItem(at: oldRoot, to: newRoot)

        // Nothing under the moved tree records where it used to live (a path under the temporary directory).
        let oldPathBytes = Data(oldRoot.path.utf8)
        let enumerator = try XCTUnwrap(FileManager.default.enumerator(at: newRoot, includingPropertiesForKeys: [.isRegularFileKey]))
        for case let file as URL in enumerator {
            guard (try? file.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true else { continue }
            let contents = try Data(contentsOf: file)
            XCTAssertNil(contents.range(of: oldPathBytes), "\(file.lastPathComponent) holds the old container path")
        }

        let reopened = try makeRoot(at: newRoot)
        defer { reopened.shutDown() }
        XCTAssertEqual(try count(of: itemID, in: newRoot), "1", "the capture is still there at the new location")
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
