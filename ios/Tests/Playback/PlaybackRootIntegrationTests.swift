import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class PlaybackRootUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Records playback's audio-session calls in order, so a test can see whether the session was released.
private final class PlaybackRootAudioSessionLog: PlaybackAudioSessionControlling, @unchecked Sendable {
    static let deactivation = "setActive(false, .notifyOthersOnDeactivation)"

    private let lock = NSLock()
    private var recorded: [String] = []

    var calls: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    private func record(_ call: String) {
        lock.lock()
        recorded.append(call)
        lock.unlock()
    }

    func setPlaybackCategory() throws { record("setCategory(.playback, .spokenAudio)") }
    func activate() throws { record("setActive(true)") }
    func deactivate() { record(Self.deactivation) }
}

/// The synthetic recorder engine with a probe that runs as the engine starts, before it writes anything.
private final class ProbedVoiceEngine: VoiceCaptureEngine {
    let synthetic = SyntheticVoiceEngine()
    var atStart: () -> Void = {}

    var onEvent: ((VoiceEngineEvent) -> Void)? {
        get { synthetic.onEvent }
        set { synthetic.onEvent = newValue }
    }

    func start(writingTo destination: URL, maxDuration: TimeInterval) throws {
        atStart()
        try synthetic.start(writingTo: destination, maxDuration: maxDuration)
    }

    func stopAndClose() -> Bool { synthetic.stopAndClose() }
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
/// only once an intent is recorded, that the lifecycle's retention and deletion cleanup share the root's fence, and
/// that the app assembly keeps playback and recording off the shared audio session at the same time.
final class PlaybackRootIntegrationTests: XCTestCase {
    private static let routeID = "route-playback"

    private var storageRoot: URL!
    private var coreLeakBaseline = CoreLeakBaseline.empty

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("playback-root-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        coreLeakBaseline = CoreLeakBaseline.capture()
    }

    override func tearDownWithError() throws {
        coreLeakBaseline.check()
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

    // MARK: recording through the app assembly

    /// `AppServices` connects the recorder to the root's playback. A recording started from the capture screen ends an
    /// open, playing session and releases its audio session before the engine takes the session for recording.
    func testANewRecordingThroughTheAppAssemblyEndsPlayingAudioBeforeTheEngineStarts() throws {
        let audio = PlaybackRootAudioSessionLog()
        let engine = ProbedVoiceEngine()
        let (services, root) = try makeAppServices(audio: audio, engine: engine)
        defer { services.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        let audioURL = try seedAudio(captureID: captureID)
        let opened = try run { try await root.playback.open(itemID: itemID) }
        XCTAssertTrue(opened.play())
        var stateAtStart: (isOpen: Bool, calls: [String])?
        engine.atStart = { stateAtStart = (opened.isOpen, audio.calls) }

        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        services.voice.recorder.start { started = $0 }
        XCTAssertTrue(pump(until: { started != nil }), "the recording start completes")
        defer { services.voice.recorder.cancel() }

        _ = try XCTUnwrap(started).get()
        let observed = try XCTUnwrap(stateAtStart, "the engine started")
        XCTAssertFalse(observed.isOpen, "playback had ended when the engine started")
        XCTAssertEqual(
            observed.calls,
            ["setCategory(.playback, .spokenAudio)", "setActive(true)", PlaybackRootAudioSessionLog.deactivation])
        XCTAssertEqual(opened.endReason, .closed)
        XCTAssertFalse(root.playback.fence.isHeld(audioURL))
        XCTAssertEqual(engine.synthetic.startCount, 1)
    }

    /// The assembly also tells playback a recording is in progress: an open while one is running is refused.
    func testAnOpenWhileRecordingThroughTheAppAssemblyIsRefused() throws {
        let audio = PlaybackRootAudioSessionLog()
        let engine = ProbedVoiceEngine()
        let (services, root) = try makeAppServices(audio: audio, engine: engine)
        defer { services.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        _ = try seedAudio(captureID: captureID)
        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        services.voice.recorder.start { started = $0 }
        XCTAssertTrue(pump(until: { started != nil }), "the recording start completes")
        _ = try XCTUnwrap(started).get()

        XCTAssertThrowsError(try run { try await root.playback.open(itemID: itemID) }) {
            XCTAssertEqual($0 as? PlaybackError, .recordingInProgress)
        }

        services.voice.recorder.cancel()
        XCTAssertTrue(pump(until: { services.voice.controller.state == .idle }), "the recording ends")
        let reopened = try run { try await root.playback.open(itemID: itemID) }
        XCTAssertTrue(reopened.isOpen, "once the recording ends, playback opens again")
        reopened.close()
        XCTAssertEqual(audio.calls, [], "no session was configured for a refused open")
    }

    /// Continuing a recording from recovery starts the engine through the same assembly, so it ends playback too.
    func testAContinuedRecordingThroughTheAppAssemblyEndsPlayingAudioBeforeTheEngineStarts() throws {
        let audio = PlaybackRootAudioSessionLog()
        let engine = ProbedVoiceEngine()
        let (services, root) = try makeAppServices(audio: audio, engine: engine)
        defer { services.shutDown() }
        let (itemID, captureID) = try saveCapture(root)
        _ = try seedAudio(captureID: captureID)
        try FileManager.default.createDirectory(at: root.inProgressAudioDirectory, withIntermediateDirectories: true)
        try SyntheticVoiceEngine.writeClosedRecording(
            at: root.inProgressAudioDirectory.appendingPathComponent("voice-left-1.wav"), frames: 8000)
        var listing: VoiceRecoveryListing?
        services.voice.recovery.refresh { listing = $0 }
        XCTAssertTrue(pump(until: { listing != nil }), "the recovery listing completes")
        let leftRecording = try XCTUnwrap(listing?.recordings.first, "the left recording is offered")
        let opened = try run { try await root.playback.open(itemID: itemID) }
        XCTAssertTrue(opened.play())
        var stateAtStart: (isOpen: Bool, calls: [String])?
        engine.atStart = { stateAtStart = (opened.isOpen, audio.calls) }

        var continued: Result<VoiceRecordingStarted, VoiceRecoveryFailure>?
        services.voice.recovery.continueRecording(leftRecording) { continued = $0 }
        XCTAssertTrue(pump(until: { continued != nil }), "the continuation completes")
        defer { services.voice.recorder.cancel() }

        _ = try XCTUnwrap(continued).get()
        let observed = try XCTUnwrap(stateAtStart, "the engine started")
        XCTAssertFalse(observed.isOpen, "playback had ended when the engine started")
        XCTAssertEqual(observed.calls.last, PlaybackRootAudioSessionLog.deactivation)
        XCTAssertEqual(observed.calls.filter { $0 == PlaybackRootAudioSessionLog.deactivation }.count, 1)
        XCTAssertEqual(opened.endReason, .closed)
    }

    // MARK: helpers

    /// The app's assembly over an authenticated root whose playback records its audio-session calls, with the
    /// microphone and the recorder engine replaced.
    private func makeAppServices(
        audio: PlaybackRootAudioSessionLog, engine: ProbedVoiceEngine
    ) throws -> (AppServices, ServiceCompositionRoot) {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        let lifecycle = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: PlaybackRootUnreachableSender(), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                playbackAudioSession: audio),
            modules: [lifecycle])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        environment.schedulePoll = { _, _ in {} }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: FakeMicrophonePermission(), environment: environment,
            notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: NotificationCenter(), isProtectedDataAvailable: { true })
        return (services, root)
    }


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
