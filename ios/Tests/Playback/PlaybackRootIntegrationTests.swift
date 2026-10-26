import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class PlaybackRootUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class PlaybackRootResultBox<Value>: @unchecked Sendable {
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

/// Drives `ServiceCompositionRoot.playback` over the real Rust core and the real `AVAudioPlayer` with the real
/// `LifecycleCoordinator`: that the root returns a playable handle, that the root's deletion service ends playback
/// only once an intent is recorded, and that the lifecycle's retention and deletion cleanup share the root's fence.
final class PlaybackRootIntegrationTests: XCTestCase {
    private static let routeID = "route-playback"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("playback-root-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        // A core read issued before the root shut down finishes on its own queue; wait for its buffer to be freed.
        waitUntilPumping("every core result buffer is freed", timeout: 10) {
            CoreHandle.liveResultBufferCount == self.baselineBuffers
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testTheRootOpensRetainedAudioAsAPlayableHandle() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)

        let opened = try run { try await root.playback.open(itemID: itemID) }

        XCTAssertTrue(opened.isOpen)
        XCTAssertEqual(opened.itemID, itemID)
        XCTAssertTrue(root.playback.fence.isHeld(audioURL))
        opened.close()
        XCTAssertEqual(opened.endReason, .closed)
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
    }

    func testARecordedDeletionThroughTheRootEndsPlaybackBeforeCleanupRemovesTheAudio() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let start = try revision(of: itemID)
        let opened = try run { try await root.playback.open(itemID: itemID) }

        let receipt = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }

        XCTAssertEqual(opened.endReason, .deletionRequested)
        XCTAssertEqual(receipt.progress, .complete)
        XCTAssertTrue(receipt.cleanupSettled)
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path), "the audio was removed after playback ended")
    }

    func testARefusedDeletionThroughTheRootLeavesPlaybackOpen() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let start = try revision(of: itemID)
        let opened = try run { try await root.playback.open(itemID: itemID) }
        defer { opened.close() }

        XCTAssertThrowsError(try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start + 3) }) {
            XCTAssertEqual($0 as? ItemDeletionError, .staleRevision(currentRevision: start))
        }

        XCTAssertTrue(opened.isOpen, "a deletion the core refused does not stop playback")
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
    }

    func testTheLifecycleRetentionServiceUsesTheRootsFence() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let reference = IngressRecord.audioReference(finalizedFileName: audioURL.lastPathComponent)
        let retention = try XCTUnwrap(coordinator.audioRetention)
        let opened = try run { try await root.playback.open(itemID: itemID) }
        defer { opened.close() }

        let outcome = try run { await retention.removeAudioForUser(itemID: itemID, audioReference: reference) }

        XCTAssertEqual(outcome, .retryLater(.audioInPlayback))
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
    }

    func testTheLifecycleDeletionCleanupUsesTheRootsFence() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(authenticated: true, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let start = try revision(of: itemID)
        let opened = try run { try await root.playback.open(itemID: itemID) }
        // Take the root's intent observer out so the deletion cannot end playback: only the fence stands in the way.
        root.itemDeletion.setIntentObserver(nil)

        let receipt = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }

        XCTAssertFalse(receipt.cleanupSettled)
        XCTAssertTrue(opened.isOpen)
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path), "cleanup leaves open audio in place")

        opened.close()
        root.setAppActiveForTests(true)
        let pass = try run { await coordinator.reconcile(.foreground) }
        XCTAssertEqual(pass.outcome(of: .deletion), .completed, "the pass reported \(pass)")
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path), "cleanup finishes once playback closed")
    }

    func testRelockingThroughTheRootsSessionEndsPlayback() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let opened = try run { try await root.playback.open(itemID: itemID) }

        session.relock(.explicit)

        XCTAssertEqual(opened.endReason, .relocked)
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
    }

    // MARK: helpers

    private var layout: ProtectedStorageLayout { ProtectedStorageLayout(rootDirectory: storageRoot) }

    private var databasePath: String { layout.databaseURL.path }

    private func makeRoot(authenticated: Bool, modules: [CompositionModule]) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return try makeRoot(session: session, modules: modules)
    }

    private func makeRoot(
        session: ReadAuthenticationSession, modules: [CompositionModule]
    ) throws -> ServiceCompositionRoot {
        try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: PlaybackRootUnreachableSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)),
            modules: modules)
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> (itemID: String, captureID: String) {
        let captureID = UUID().uuidString.lowercased()
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: captureID, text: "synthetic capture to play", capturedAt: Date()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return (itemID, captureID)
    }

    /// Gives the saved item retained audio: a tiny synthetic WAV the real `AVAudioPlayer` can open.
    private func seedAudio(captureID: String) throws -> URL {
        let finalizedName = "\(captureID).wav"
        let audioURL = layout.directory(for: .finalizedAudio).appendingPathComponent(finalizedName)
        try FileManager.default.createDirectory(
            at: audioURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Self.silentWave().write(to: audioURL)
        let reference = IngressRecord.audioReference(finalizedFileName: finalizedName)
        try execute("UPDATE captures SET audio_reference = '\(reference)' WHERE capture_id = '\(captureID)'")
        return audioURL
    }

    /// Half a second of 8 kHz mono 16-bit silence.
    private static func silentWave() -> Data {
        let sampleRate: UInt32 = 8000
        let dataSize: UInt32 = sampleRate  // 4000 samples * 2 bytes
        func le32(_ value: UInt32) -> [UInt8] { (0..<4).map { UInt8((value >> (8 * UInt32($0))) & 0xff) } }
        func le16(_ value: UInt16) -> [UInt8] { [UInt8(value & 0xff), UInt8(value >> 8)] }
        var bytes: [UInt8] = Array("RIFF".utf8) + le32(36 + dataSize) + Array("WAVEfmt ".utf8)
        bytes += le32(16) + le16(1) + le16(1) + le32(sampleRate) + le32(sampleRate * 2) + le16(2) + le16(16)
        bytes += Array("data".utf8) + le32(dataSize)
        return Data(bytes) + Data(count: Int(dataSize))
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = PlaybackRootResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func execute(_ sql: String) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        if code != SQLITE_OK { throw CompositionError.storageUnavailable }
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}
