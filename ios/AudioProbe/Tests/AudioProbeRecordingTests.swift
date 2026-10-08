import XCTest
import AVFoundation
@testable import AudioProbe

class AudioProbeRecordingTests: XCTestCase {
    var recorder: AudioRecorder!
    var testRecordingURL: URL!

    override func setUp() {
        super.setUp()
        recorder = AudioRecorder()
        let tempDirectory = FileManager.default.temporaryDirectory
        testRecordingURL = tempDirectory.appendingPathComponent("test-\(UUID().uuidString).wav")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: testRecordingURL)
        super.tearDown()
    }

    func testRecordingStartsSuccessfully() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started, "Recording should start successfully")
        XCTAssertNotNil(recorder.recordingStartTime, "Recording start time should be set")
        _ = recorder.stopRecording()
    }

    func testRecordingStopsAndReturnsResult() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let result = recorder.stopRecording()
        XCTAssertTrue(result.success, "Recording should complete successfully")
        XCTAssertGreaterThan(result.durationSeconds, 0.4, "Recording duration should be at least 0.4 seconds")
        XCTAssertNotNil(result.filePath, "Recording file path should be set")
        XCTAssertGreaterThan(result.fileSize, 0, "Recording file should have non-zero size")
    }

    func testCancellingRecordingReturnsPartialResult() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let result = recorder.cancelRecording()
        XCTAssertFalse(result.success, "Cancelled recording should not mark as successful")
        XCTAssertNotNil(result.filePath, "Cancelled recording should preserve partial file path")
        XCTAssertEqual(result.interruption, "Cancelled", "Interruption reason should be 'Cancelled'")
    }

    func testRecordingWithoutStartReturnsFailure() {
        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "Stopping without starting should fail")
        XCTAssertNil(result.filePath, "No file path should be available")
        XCTAssertEqual(result.durationSeconds, 0, "Duration should be zero")
    }

    func testMultipleRecordingSequences() {
        let result1 = recorder.stopRecording()
        XCTAssertFalse(result1.success, "First stop without start should fail")

        let started1 = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started1)
        let stopped1 = recorder.stopRecording()
        XCTAssertTrue(stopped1.success)

        let tempFile2 = FileManager.default.temporaryDirectory.appendingPathComponent("test-2-\(UUID().uuidString).wav")
        let started2 = recorder.startRecording(to: tempFile2)
        XCTAssertTrue(started2)
        let stopped2 = recorder.stopRecording()
        XCTAssertTrue(stopped2.success)

        try? FileManager.default.removeItem(at: tempFile2)
    }

    func testAudioSessionSetup() {
        let session = AVAudioSession.sharedInstance()
        XCTAssertEqual(session.category, .record, "Audio session should be in record category")
    }

    func testRecordingFileCreation() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        recorder.stopRecording()

        let fileExists = FileManager.default.fileExists(atPath: testRecordingURL.path)
        XCTAssertTrue(fileExists, "Recording file should exist after stop")
    }

    func testRecordingResultDuration() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let expectedDuration = 0.5
        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + expectedDuration) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let result = recorder.stopRecording()
        XCTAssertGreaterThan(result.durationSeconds, expectedDuration - 0.1, "Duration should match recorded time")
        XCTAssertLessThan(result.durationSeconds, expectedDuration + 0.3, "Duration should not exceed recorded time by much")
    }

    func testInterruptionHandling() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Record for interruption")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let interruptionNotification = Notification(
            name: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            userInfo: [
                AVAudioSession.interruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue
            ]
        )
        NotificationCenter.default.post(interruptionNotification)

        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "Recording interrupted should not be marked as successful")
        let filePath = XCTUnwrap(result.filePath, "Interrupted recording should have a partial file")
        XCTAssertEqual(result.interruption, "Audio interrupted", "Interruption reason should be recorded")

        let audioFileURL = URL(fileURLWithPath: filePath)
        let audioFile = try XCTUnwrap(try? AVAudioFile(forReading: audioFileURL), "Partial file should be readable")
        XCTAssertGreaterThan(audioFile.length, 0, "Interrupted partial file should contain audio")
    }

    func testInterruptionWithResume() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation1 = XCTestExpectation(description: "Record before interruption")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            waitExpectation1.fulfill()
        }
        wait(for: [waitExpectation1], timeout: 2)

        let beganNotification = Notification(
            name: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            userInfo: [
                AVAudioSession.interruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue
            ]
        )
        NotificationCenter.default.post(beganNotification)

        let waitExpectation2 = XCTestExpectation(description: "Interrupted")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            waitExpectation2.fulfill()
        }
        wait(for: [waitExpectation2], timeout: 2)

        let endedNotification = Notification(
            name: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            userInfo: [
                AVAudioSession.interruptionTypeKey: AVAudioSession.InterruptionType.ended.rawValue,
                AVAudioSession.interruptionOptionKey: AVAudioSession.InterruptionOptions.shouldResume.rawValue
            ]
        )
        NotificationCenter.default.post(endedNotification)

        let waitExpectation3 = XCTestExpectation(description: "Resume and record")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            waitExpectation3.fulfill()
        }
        wait(for: [waitExpectation3], timeout: 2)

        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "Recording with interruption and resume should not report clean success")
        XCTAssertEqual(result.interruption, "Interrupted and resumed", "Should track that interruption occurred despite resume")

        if let filePath = result.filePath {
            let audioFileURL = URL(fileURLWithPath: filePath)
            let audioFile = try XCTUnwrap(try? AVAudioFile(forReading: audioFileURL), "Interrupted+resumed file should be readable")
            XCTAssertGreaterThan(audioFile.length, 0, "File should contain audio despite interruption")
        }
    }

    func testCancelledRecovery() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let result = recorder.cancelRecording()
        XCTAssertFalse(result.success, "Cancelled recording should not be successful")
        let filePath = XCTUnwrap(result.filePath, "Cancelled recording should preserve partial file path")
        XCTAssertEqual(result.interruption, "Cancelled", "Interruption reason should be 'Cancelled'")

        let fileExists = FileManager.default.fileExists(atPath: filePath)
        XCTAssertTrue(fileExists, "Cancelled partial file should exist")

        let audioFileURL = URL(fileURLWithPath: filePath)
        let audioFile = try XCTUnwrap(try? AVAudioFile(forReading: audioFileURL), "Partial file should be readable")
        XCTAssertGreaterThan(audioFile.length, 0, "Partial file should contain audio data")
    }

    func testDoubleStopDoesNotReportSuccessTwice() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Recording duration")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let result1 = recorder.stopRecording()
        XCTAssertTrue(result1.success, "First stop should succeed")

        let result2 = recorder.stopRecording()
        XCTAssertFalse(result2.success, "Second stop should fail because recording is no longer active")
        XCTAssertEqual(result2.interruption, "No active recording", "Second stop should report no active recording")
    }

    func testFailedStartClearsState() {
        let tempURL1 = FileManager.default.temporaryDirectory.appendingPathComponent("test-invalid-\(UUID().uuidString).wav")
        let invalidParentURL = tempURL1.deletingLastPathComponent().appendingPathComponent("nonexistent").appendingPathComponent("test.wav")

        let started = recorder.startRecording(to: invalidParentURL)
        XCTAssertFalse(started, "Recording should fail with invalid destination")

        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "Stop after failed start should fail")
        XCTAssertNil(result.filePath, "No file path should be available after failed start")

        let result2 = recorder.stopRecording()
        XCTAssertFalse(result2.success, "Second stop should also fail")
    }

    func testUnreadableInterruptedFileFails() {
        let started = recorder.startRecording(to: testRecordingURL)
        XCTAssertTrue(started)

        let waitExpectation = XCTestExpectation(description: "Record very briefly")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            waitExpectation.fulfill()
        }
        wait(for: [waitExpectation], timeout: 2)

        let interruptionNotification = Notification(
            name: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            userInfo: [
                AVAudioSession.interruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue
            ]
        )
        NotificationCenter.default.post(interruptionNotification)

        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "Interrupted recording should fail")
        if result.filePath != nil {
            XCTAssertNil(result.filePath, "Unrecoverable interrupted file should not return a path")
        }
    }

    func testMicrophonePermissionDenial() {
        let session = AVAudioSession.sharedInstance()
        if session.recordPermission == .denied {
            let started = recorder.startRecording(to: testRecordingURL)
            XCTAssertFalse(started, "Recording should fail when microphone is denied")
            XCTAssertEqual(recorder.lastInterruptionReason, "Microphone permission denied")
        } else {
            XCTSkip("Microphone permission is not denied; skipping denial test")
        }
    }
}
