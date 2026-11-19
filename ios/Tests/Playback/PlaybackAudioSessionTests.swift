import XCTest
@testable import OhAndServices

private struct SessionRefused: Error {}

private final class RecordingAudioSession: PlaybackAudioSessionControlling, @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [String] = []
    var failCategory = false
    var failActivation = false

    var calls: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(_ call: String) {
        lock.lock()
        recorded.append(call)
        lock.unlock()
    }

    func setPlaybackCategory() throws {
        record("setCategory(.playback, .spokenAudio)")
        if failCategory { throw SessionRefused() }
    }

    func activate() throws {
        record("setActive(true)")
        if failActivation { throw SessionRefused() }
    }

    func deactivate() { record("setActive(false, .notifyOthersOnDeactivation)") }
}

private final class OrderedPlayer: FinishNotifyingPlayer, @unchecked Sendable {
    private let log: RecordingAudioSession
    private var handler: (() -> Void)?

    init(log: RecordingAudioSession) { self.log = log }

    func setFinishHandler(_ handler: @escaping () -> Void) { self.handler = handler }
    func finish() { handler?() }

    func play() -> Bool {
        log.record("player.play()")
        return true
    }

    func pause() { log.record("player.pause()") }
    func stop() { log.record("player.stop()") }
}

final class PlaybackAudioSessionTests: XCTestCase {
    private let deactivation = "setActive(false, .notifyOthersOnDeactivation)"
    private var audio: RecordingAudioSession!
    private var player: OrderedPlayer!
    private var notifications: NotificationCenter!
    private var recording = false

    override func setUp() {
        super.setUp()
        audio = RecordingAudioSession()
        player = OrderedPlayer(log: audio)
        notifications = NotificationCenter()
        recording = false
    }

    private func makeSession() -> PlaybackSession {
        PlaybackSession(
            itemID: "item-1", player: player, audioSession: audio, isRecordingActive: { [unowned self] in recording },
            notificationCenter: notifications, release: {})
    }

    private func deactivations() -> Int { audio.calls.filter { $0 == deactivation }.count }

    func testFirstPlayConfiguresThenActivatesThenStartsThePlayer() {
        let session = makeSession()

        XCTAssertTrue(session.play())

        XCTAssertEqual(audio.calls, ["setCategory(.playback, .spokenAudio)", "setActive(true)", "player.play()"])
    }

    func testResumeAfterPauseDoesNotConfigureAgain() {
        let session = makeSession()
        XCTAssertTrue(session.play())
        session.pause()
        XCTAssertTrue(session.play())

        XCTAssertEqual(audio.calls.filter { $0.hasPrefix("setCategory") }.count, 1)
        XCTAssertEqual(audio.calls.filter { $0 == "setActive(true)" }.count, 1)
        XCTAssertEqual(audio.calls.filter { $0 == "player.play()" }.count, 2)
        XCTAssertEqual(deactivations(), 0, "pausing keeps the session")
    }

    func testDeactivatesExactlyOnceWhenClosed() {
        let session = makeSession()
        session.play()
        session.close()
        session.close()
        XCTAssertEqual(deactivations(), 1)
    }

    func testDeactivatesExactlyOnceWhenTheClipFinishes() {
        let session = makeSession()
        session.play()
        player.finish()
        session.close()
        XCTAssertEqual(session.endReason, .finished)
        XCTAssertEqual(deactivations(), 1)
    }

    func testDeactivatesExactlyOnceWhenDeletionRelockOrShutdownEndsIt() {
        for reason in [PlaybackEndReason.deletionRequested, .relocked] {
            setUp()
            let session = makeSession()
            session.play()
            session.end(reason)
            session.end(.relocked)
            XCTAssertEqual(deactivations(), 1, "\(reason)")
        }
    }

    func testASessionThatNeverPlayedDoesNotDeactivate() {
        let session = makeSession()
        session.close()
        XCTAssertEqual(deactivations(), 0)
    }

    func testPlayReturnsFalseAndNeverStartsThePlayerWhenTheCategoryCannotBeSet() {
        audio.failCategory = true
        let session = makeSession()

        XCTAssertFalse(session.play())

        XCTAssertFalse(audio.calls.contains("player.play()"))
        XCTAssertFalse(audio.calls.contains("setActive(true)"))
    }

    func testPlayReturnsFalseAndNeverStartsThePlayerWhenActivationFails() {
        audio.failActivation = true
        let session = makeSession()

        XCTAssertFalse(session.play())

        XCTAssertFalse(audio.calls.contains("player.play()"))
        session.close()
        XCTAssertEqual(deactivations(), 0, "nothing was activated")
    }

    func testPlayIsRefusedWhileARecordingIsInProgressAndDoesNotTouchTheSession() {
        recording = true
        let session = makeSession()

        XCTAssertFalse(session.play())

        XCTAssertEqual(audio.calls, [])
        recording = false
        XCTAssertTrue(session.play())
    }

    func testAnInterruptionPausesThePlayerAndResumingReactivatesTheSession() {
        let session = makeSession()
        var interruptions = 0
        session.setInterruptionHandler { interruptions += 1 }
        session.play()

        notifications.post(
            name: .playbackAudioInterruption, object: nil, userInfo: ["AVAudioSessionInterruptionTypeKey": UInt(1)])

        XCTAssertEqual(interruptions, 1)
        XCTAssertTrue(audio.calls.contains("player.pause()"))
        XCTAssertTrue(session.isOpen)
        XCTAssertTrue(session.play())
        XCTAssertEqual(audio.calls.filter { $0 == "setActive(true)" }.count, 2)
    }

    func testTheEndOfAnInterruptionDoesNotPause() {
        let session = makeSession()
        session.play()
        notifications.post(
            name: .playbackAudioInterruption, object: nil, userInfo: ["AVAudioSessionInterruptionTypeKey": UInt(0)])
        XCTAssertFalse(audio.calls.contains("player.pause()"))
    }
}
