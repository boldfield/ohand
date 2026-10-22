import Foundation
import SQLite3
import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class SourceDetailRootUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class SourceDetailRootResultBox<Value>: @unchecked Sendable {
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

/// Drives `SourceDetailModel.assembled` over the real composition root, the real Rust core and the real
/// `AVAudioPlayer`: synthetic text and retained audio are read and played through the authenticated services, and
/// deletion and relock end playback.
final class SourceDetailRootIntegrationTests: XCTestCase {
    private static let routeID = "route-source-detail"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("source-detail-root-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testTheStoredTextAndRetainedAudioAreReadAndPlayedThroughTheRoot() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)

        try runOnMain { await model.load() }

        XCTAssertEqual(model.content?.sections.map(\.text), ["synthetic capture to play"])
        XCTAssertEqual(model.audio, .available)
        try runOnMain { await model.play() }
        XCTAssertEqual(model.audio, .playing)
        XCTAssertTrue(root.playback.fence.isHeld(audioURL))
        try runOnMain { model.close() }
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
    }

    func testARecordedDeletionTurnsPlaybackOffAndALaterReadFindsNothing() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        _ = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.play() }
        XCTAssertEqual(model.audio, .playing)
        let start = try revision(of: itemID)

        _ = try run { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: start) }
        try runOnMain { model.checkSession() }

        XCTAssertEqual(model.audio, .removed)
        try runOnMain { await model.load() }
        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .notFound)
    }

    func testRelockingEndsPlaybackAndAReloadShowsNoText() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        _ = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.play() }

        session.relock(.explicit)
        try runOnMain { model.checkSession() }

        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .locked)
        XCTAssertEqual(model.audio, .locked)
        try runOnMain { await model.load() }
        XCTAssertNil(model.content, "the core is not asked while locked")
        XCTAssertEqual(model.failure, .locked)
    }

    func testRelockingDropsTextThatIsShownWithoutAnyPlayback() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, _) = try saveCapture(root)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        XCTAssertEqual(model.content?.sections.map(\.text), ["synthetic capture to play"])

        session.relock(.explicit)
        try runOnMain { model.checkSession() }

        XCTAssertNil(model.content, "no text stays once the read session is locked")
        XCTAssertEqual(model.failure, .locked)
    }

    func testAClipThatPlaysToItsEndReleasesTheFileAndCanBePlayedAgain() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.play() }
        XCTAssertEqual(model.audio, .playing)
        XCTAssertTrue(root.playback.fence.isHeld(audioURL))

        XCTAssertTrue(pump(until: {
            MainActor.assumeIsolated { model.checkSession() }
            return model.audio != .playing
        }), "the half-second clip finishes")

        XCTAssertEqual(model.audio, .available, "the control offers play again, not pause")
        XCTAssertFalse(root.playback.fence.isHeld(audioURL), "the file is no longer held in place")
        try runOnMain { await model.play() }
        XCTAssertEqual(model.audio, .playing, "replay opens the clip afresh")
        XCTAssertTrue(root.playback.fence.isHeld(audioURL))
        try runOnMain { model.close() }
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
    }

    func testWithoutAuthenticationNeitherTextNorAudioIsOffered() throws {
        let root = try makeRoot(session: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()), modules: [])
        defer { root.shutDown() }
        let model = SourceDetailModel.assembled(itemID: "item-never-read", root: root)

        try runOnMain { await model.load() }
        try runOnMain { await model.play() }

        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .locked)
        XCTAssertFalse(model.canPlay)
    }

    func testAudioThatExpiredOrWasRemovedIsHonestlyUnavailableWithoutTouchingTheOldFile() throws {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [])
        defer { root.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        XCTAssertEqual(model.audio, .available)
        try execute("UPDATE captures SET audio_reference = NULL WHERE capture_id = '\(captureID)'")
        try FileManager.default.removeItem(at: audioURL)

        try runOnMain { await model.play() }

        XCTAssertEqual(model.audio, .removed)
        XCTAssertFalse(model.canPlay)
    }

    func testAudioRemovedByTheRealRetentionServiceFirstRendersAsUnavailableWithNoPlayControl() throws {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        let root = try makeRoot(session: session, modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let reference = IngressRecord.audioReference(finalizedFileName: audioURL.lastPathComponent)
        let retention = try XCTUnwrap(coordinator.audioRetention)

        let outcome = try run { await retention.removeAudioForUser(itemID: itemID, audioReference: reference) }

        XCTAssertEqual(outcome, .removed)
        XCTAssertFalse(FileManager.default.fileExists(atPath: audioURL.path))
        // Host the real screen and let it load itself, recording every identified element it ever puts on screen,
        // so a Play button that showed up even briefly before the notice would be caught.
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        var onScreen: Set<String> = []
        var everShown: Set<String> = []
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: NavigationStack {
            SourceDetailView(model: model, renderReporter: { identifier, shown in
                if shown {
                    onScreen.insert(identifier)
                    everShown.insert(identifier)
                } else {
                    onScreen.remove(identifier)
                }
            })
        })
        window.makeKeyAndVisible()
        defer {
            window.isHidden = true
            _ = pump(until: { false }, timeout: 0.2)
        }

        XCTAssertTrue(
            pump(until: {
                window.rootViewController?.view.layoutIfNeeded()
                return onScreen.contains(SourceDetailView.sectionIdentifier(.original))
                    && onScreen.contains(SourceDetailView.audioNoticeIdentifier)
            }),
            "the loaded screen shows the text and the disabled audio notice")
        XCTAssertEqual(model.content?.sections.map(\.text), ["synthetic capture to play"])
        XCTAssertEqual(model.audio, .removed, "the first render is already the disabled state")
        XCTAssertFalse(model.canPlay)
        XCTAssertTrue(onScreen.contains(SourceDetailView.audioNoticeIdentifier), "the disabled notice is visible")
        XCTAssertFalse(onScreen.contains(SourceDetailView.playButtonIdentifier), "no Play button is shown")
        XCTAssertFalse(everShown.contains(SourceDetailView.playButtonIdentifier), "Play was never rendered")
        XCTAssertFalse(everShown.contains(SourceDetailView.pauseButtonIdentifier), "Pause was never rendered")
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
    }

    // MARK: reminder section follows the core after a change on detail

    func testChangingTheTypeToActionMakesTheReminderSectionAppear() throws {
        let root = try makeRoot(authenticated: true, modules: [])
        defer { root.shutDown() }
        let (itemID, _) = try saveCapture(root)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.itemControls?.load() }
        try runOnMain { await model.reminderEditor?.load() }
        XCTAssertEqual(model.reminderEditor?.isShown, false, "a note carries no reminder")

        try runOnMain {
            model.itemControls?.pendingType = .action
            await model.itemControls?.applyType()
        }

        XCTAssertTrue(
            pump(until: { model.reminderEditor?.isShown == true && model.reminderEditor?.isLoading == false }),
            "the section follows the new type")
        XCTAssertEqual(model.reminderEditor?.canSave, true)
    }

    func testChangingAnActionToAnotherTypeRemovesTheEditableReminderSection() throws {
        let root = try makeRoot(authenticated: true, modules: [])
        defer { root.shutDown() }
        let (itemID, _) = try saveActionItem(root)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.itemControls?.load() }
        try runOnMain { await model.reminderEditor?.load() }
        XCTAssertEqual(model.reminderEditor?.isShown, true)

        try runOnMain {
            model.itemControls?.pendingType = .note
            await model.itemControls?.applyType()
        }

        XCTAssertTrue(pump(until: { model.reminderEditor?.isShown == false }), "the section is no longer offered")
        XCTAssertEqual(model.reminderEditor?.canSave, false)
        XCTAssertEqual(model.reminderEditor?.canCancel, false)
    }

    func testCompletingTheItemClosesTheReminderSection() throws {
        try assertLifecycleClosesTheReminderSection { await $0.complete() }
    }

    func testCancellingTheItemClosesTheReminderSection() throws {
        try assertLifecycleClosesTheReminderSection { await $0.cancel() }
    }

    private func assertLifecycleClosesTheReminderSection(
        _ act: @escaping @MainActor @Sendable (ItemControlsModel) async -> Void
    ) throws {
        let root = try makeRoot(authenticated: true, modules: [])
        defer { root.shutDown() }
        let (itemID, _) = try saveActionItem(root)
        let model = SourceDetailModel.assembled(itemID: itemID, root: root)
        try runOnMain { await model.load() }
        try runOnMain { await model.itemControls?.load() }
        try runOnMain { await model.reminderEditor?.load() }
        XCTAssertEqual(model.reminderEditor?.canSave, true)
        let controls = try XCTUnwrap(model.itemControls)

        try runOnMain { await act(controls) }

        XCTAssertTrue(pump(until: { model.reminderEditor?.isClosed == true }), "the section learns the item ended")
        XCTAssertEqual(model.reminderEditor?.isShown, true)
        XCTAssertEqual(model.reminderEditor?.canSave, false)
        XCTAssertEqual(model.reminderEditor?.canCancel, false)
    }

    private func saveActionItem(_ root: ServiceCompositionRoot) throws -> (itemID: String, captureID: String) {
        let saved = try saveCapture(root)
        let itemID = saved.itemID
        let expected = try revision(of: itemID)
        _ = try run {
            try await root.itemCorrections.correctType(
                commandID: "cmd-action-\(itemID)", itemID: itemID, expectedRevision: expected, itemType: .action)
        }
        return saved
    }

    private func runOnMain(_ operation: @escaping @MainActor @Sendable () async -> Void) throws {
        let box = SourceDetailRootResultBox<Void>()
        Task { @MainActor in
            await operation()
            box.set(.success(()))
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
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
                sender: SourceDetailRootUnreachableSender(), readSession: session,
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
        let box = SourceDetailRootResultBox<Value>()
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
