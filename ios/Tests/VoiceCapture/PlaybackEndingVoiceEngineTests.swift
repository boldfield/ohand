import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Playback and recording share one audio session, so an open playback must end before any recording starts writing,
/// whether the recording is new or continued from recovery. Both routes start through the engine.
final class PlaybackEndingVoiceEngineTests: VoiceCaptureTestCase {
    private var hookStartCounts: [Int] = []
    private var wrappedEngine: PlaybackEndingVoiceEngine!

    private func wireTheHook() {
        wrappedEngine = PlaybackEndingVoiceEngine(wrapping: engine)
        wrappedEngine.beforeStart = { [unowned self] in hookStartCounts.append(engine.startCount) }
        let wrappedController = VoiceRecordingController(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
            engine: wrappedEngine,
            permission: permission,
            handoff: handoff,
            environment: testEnvironment,
            notificationCenter: notificationCenter,
            makeCaptureID: { "voice-wired-1" })
        controller = wrappedController
        coordinator = makeCoordinator()
    }

    func testANewRecordingEndsPlaybackBeforeTheEngineStarts() throws {
        wireTheHook()

        _ = try startRecording()

        XCTAssertEqual(hookStartCounts, [0], "the hook runs once, before the engine started")
        XCTAssertEqual(engine.startCount, 1)
    }

    func testAContinuationFromRecoveryEndsPlaybackBeforeTheEngineStarts() throws {
        wireTheHook()
        try leaveRecording("voice-left-1")
        let recording = try XCTUnwrap(try refreshListing().recordings.first)

        var result: Result<VoiceRecordingStarted, VoiceRecoveryFailure>?
        coordinator.continueRecording(recording) { result = $0 }

        _ = try XCTUnwrap(result, "continue did not complete").get()
        XCTAssertEqual(hookStartCounts, [0], "the hook runs once, before the engine started")
        XCTAssertEqual(engine.startCount, 1)
    }

    func testARecordingThatIsNotStartedLeavesPlaybackAlone() throws {
        wireTheHook()
        isForeground = false

        controller.start { _ in }

        XCTAssertEqual(hookStartCounts, [])
    }
}
