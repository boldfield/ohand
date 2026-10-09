import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Starting is explicit, foreground-only and checked before any audio is written: a refused start leaves no session,
/// no file and no side effect, and says why.
final class VoiceRecordingStartTests: VoiceCaptureTestCase {
    private func inProgressFiles() throws -> [String] {
        try FileManager.default.contentsOfDirectory(atPath: layout.directory(for: .ingressInProgressAudio).path)
    }

    func testExplicitStartWritesIntoTheProtectedInProgressStoreAndNothingElse() throws {
        let started = try startRecording()

        XCTAssertEqual(started.captureID, "voice-test-1")
        XCTAssertEqual(controller.state, .recording(captureID: "voice-test-1", startedAt: started.startedAt))
        XCTAssertEqual(try inProgressFiles(), ["voice-test-1.wav"])
        XCTAssertEqual(protectedURLs, [recordingFile("voice-test-1")], "the store's protection policy is applied to the new file")
        XCTAssertEqual(engine.startCount, 1)
        XCTAssertEqual(engine.lastMaxDuration, VoiceRecordingLimits().maxDurationSeconds)
        XCTAssertTrue(poller.isPolling)
        XCTAssertEqual(poller.scheduledInterval, VoiceRecordingLimits().pollIntervalSeconds)
        XCTAssertFalse(exists(recordURL("voice-test-1")), "nothing is acknowledged or staged while recording")
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertTrue(outcomes.isEmpty)
    }

    func testDeniedMicrophoneRefusesWithoutRecordingAnything() throws {
        permission.status = .denied

        XCTAssertEqual(try startFailure(), .microphoneDenied)

        XCTAssertEqual(permission.requestCount, 0, "a denial is not re-prompted")
        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(try inProgressFiles(), [])
        XCTAssertEqual(controller.state, .idle)
        XCTAssertFalse(poller.isPolling)
    }

    func testUndeterminedMicrophoneStartsOnlyAfterTheUserGrants() throws {
        permission.status = .undetermined
        var firstResult: Result<VoiceRecordingStarted, VoiceStartFailure>?
        controller.start { firstResult = $0 }

        XCTAssertEqual(controller.state, .requestingPermission)
        XCTAssertNil(firstResult)
        XCTAssertEqual(engine.startCount, 0, "nothing records while the prompt is showing")
        XCTAssertEqual(try startFailure(), .alreadyActive)

        permission.answerPending(true)

        XCTAssertEqual(try XCTUnwrap(firstResult).get().captureID, "voice-test-1")
        XCTAssertEqual(engine.startCount, 1)
        guard case .recording = controller.state else { return XCTFail("expected recording, got \(controller.state)") }
    }

    func testUserRefusingThePromptLeavesNothingRecorded() throws {
        permission.status = .undetermined
        permission.promptAnswer = false

        XCTAssertEqual(try startFailure(), .microphoneDenied)

        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(try inProgressFiles(), [])
        XCTAssertEqual(controller.state, .idle)
    }

    func testRecordingNeverStartsOutsideTheForeground() throws {
        isForeground = false
        XCTAssertEqual(try startFailure(), .notInForeground)

        isForeground = true
        permission.status = .undetermined
        var result: Result<VoiceRecordingStarted, VoiceStartFailure>?
        controller.start { result = $0 }
        isForeground = false
        permission.answerPending(true)

        guard case .failure(.notInForeground)? = result else { return XCTFail("got \(String(describing: result))") }
        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(controller.state, .idle)
    }

    func testEngineRefusalsMapToHonestFailuresAndLeaveNoFile() throws {
        engine.createsEmptyFileBeforeFailingToStart = true
        engine.startError = VoiceEngineStartError(reason: .microphoneDenied)
        XCTAssertEqual(try startFailure(), .microphoneDenied)
        XCTAssertEqual(try inProgressFiles(), [], "an empty file that never held audio is not left behind")

        engine.startError = VoiceEngineStartError(reason: .unavailable)
        XCTAssertEqual(try startFailure(), .recorderUnavailable)
        XCTAssertEqual(try inProgressFiles(), [])
        XCTAssertEqual(controller.state, .idle)

        engine.startError = nil
        engine.createsEmptyFileBeforeFailingToStart = false
        XCTAssertNoThrow(try startRecording(), "a refused start does not block a later recording")
    }

    func testStorageIsCheckedBeforeRecording() throws {
        freeSpace = nil
        XCTAssertEqual(try startFailure(), .storageSpaceUnknown)

        freeSpace = VoiceRecordingLimits().minimumFreeBytesToStart - 1
        XCTAssertEqual(try startFailure(), .insufficientStorage)

        freeSpace = 1_000_000_000
        try FileManager.default.removeItem(at: layout.directory(for: .ingressInProgressAudio))
        XCTAssertEqual(try startFailure(), .storageUnavailable)

        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(controller.state, .idle)
    }

    func testASecondStartKeepsTheFirstRecordingRunning() throws {
        _ = try startRecording()

        XCTAssertEqual(try startFailure(), .alreadyActive)

        XCTAssertEqual(engine.startCount, 1)
        XCTAssertEqual(try inProgressFiles(), ["voice-test-1.wav"])
        guard case .recording(let captureID, _) = controller.state else { return XCTFail("expected recording") }
        XCTAssertEqual(captureID, "voice-test-1")
    }

    func testAnExistingFileIsNeverOverwritten() throws {
        let existing = recordingFile("voice-test-1")
        try Data("earlier audio".utf8).write(to: existing)

        XCTAssertEqual(try startFailure(), .storageUnavailable)

        XCTAssertEqual(try Data(contentsOf: existing), Data("earlier audio".utf8))
        XCTAssertEqual(engine.startCount, 0)
    }

    func testCaptureIDsCannotNameAPath() {
        XCTAssertTrue(VoiceRecordingController.isSafeCaptureID(VoiceRecordingController.generateCaptureID()))
        XCTAssertFalse(VoiceRecordingController.isSafeCaptureID("../escape"))
        XCTAssertFalse(VoiceRecordingController.isSafeCaptureID("a/b"))
        XCTAssertFalse(VoiceRecordingController.isSafeCaptureID(""))
    }
}
