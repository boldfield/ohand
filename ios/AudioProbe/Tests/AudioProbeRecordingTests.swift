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
        recorder.stopRecording()
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
        XCTAssertNotNil(result.filePath, "Interrupted recording should have a partial file")
        XCTAssertEqual(result.interruption, "Audio interrupted", "Interruption reason should be recorded")
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
        XCTAssertNotNil(result.filePath, "Cancelled recording should preserve partial file path")
        XCTAssertEqual(result.interruption, "Cancelled", "Interruption reason should be 'Cancelled'")

        if let filePath = result.filePath {
            let fileExists = FileManager.default.fileExists(atPath: filePath)
            XCTAssertTrue(fileExists, "Cancelled partial file should exist")

            if fileExists {
                let audioFileURL = URL(fileURLWithPath: filePath)
                if let audioFile = try? AVAudioFile(forReading: audioFileURL) {
                    XCTAssertGreaterThan(audioFile.length, 0, "Partial file should be readable and contain audio data")
                }
            }
        }
    }
}
