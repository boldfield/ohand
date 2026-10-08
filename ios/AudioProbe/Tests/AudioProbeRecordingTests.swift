import XCTest
import AVFoundation

/// Writes a real 16 kHz mono PCM WAV prefix on start so the recorder's read-back verification runs against genuine files.
final class SyntheticCaptureEngine: AudioCaptureEngine {
    enum CloseBehavior {
        case clean
        case reportFailure
        case corruptFile
        case truncateToEmpty
    }

    var startError: AudioCaptureError?
    var framesWrittenAtStart: AVAudioFrameCount = 8000
    var closeBehavior: CloseBehavior = .clean
    private(set) var startCount = 0
    private(set) var closeCount = 0
    private(set) var lastMaxDuration: TimeInterval?
    private var destination: URL?
    private var openFile: AVAudioFile?

    func start(to destination: URL, maxDuration: TimeInterval) throws {
        if let startError = startError {
            throw startError
        }
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false
        ]
        let audioFile = try AVAudioFile(
            forWriting: destination,
            settings: settings,
            commonFormat: .pcmFormatInt16,
            interleaved: true
        )
        if framesWrittenAtStart > 0 {
            let buffer = AVAudioPCMBuffer(pcmFormat: audioFile.processingFormat, frameCapacity: framesWrittenAtStart)!
            buffer.frameLength = framesWrittenAtStart
            let samples = buffer.int16ChannelData![0]
            for index in 0..<Int(framesWrittenAtStart) {
                samples[index] = Int16(truncatingIfNeeded: index % 100)
            }
            try audioFile.write(from: buffer)
        }
        startCount += 1
        lastMaxDuration = maxDuration
        self.destination = destination
        openFile = audioFile
    }

    func stopAndClose() -> Bool {
        closeCount += 1
        openFile = nil
        switch closeBehavior {
        case .clean:
            return true
        case .reportFailure:
            return false
        case .corruptFile:
            if let destination = destination {
                try? Data([0x00, 0x01, 0x02]).write(to: destination)
            }
            return true
        case .truncateToEmpty:
            if let destination = destination {
                try? Data().write(to: destination)
            }
            return true
        }
    }
}

final class AudioProbeRecordingTests: XCTestCase {
    private var engine: SyntheticCaptureEngine!
    private var notificationCenter: NotificationCenter!
    private var recorder: AudioRecorder!
    private var recordingURL: URL!
    private var endedEarlyResults: [AudioRecordingResult] = []

    override func setUp() {
        super.setUp()
        engine = SyntheticCaptureEngine()
        notificationCenter = NotificationCenter()
        recorder = makeRecorder()
        recordingURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-probe-\(UUID().uuidString).wav")
        endedEarlyResults = []
        recorder.onSessionEndedEarly = { [unowned self] result in
            self.endedEarlyResults.append(result)
        }
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: recordingURL)
        super.tearDown()
    }

    private func makeRecorder(
        limits: AudioRecordingLimits = AudioRecordingLimits(),
        freeBytes: Int64? = 1_000_000_000
    ) -> AudioRecorder {
        AudioRecorder(
            engine: engine,
            limits: limits,
            freeSpaceProvider: { _ in freeBytes },
            notificationCenter: notificationCenter
        )
    }

    private func postInterruption(
        _ type: AVAudioSession.InterruptionType,
        options: AVAudioSession.InterruptionOptions? = nil
    ) {
        var userInfo: [AnyHashable: Any] = [AVAudioSessionInterruptionTypeKey: type.rawValue]
        if let options = options {
            userInfo[AVAudioSessionInterruptionOptionKey] = options.rawValue
        }
        notificationCenter.post(name: AVAudioSession.interruptionNotification, object: nil, userInfo: userInfo)
    }

    private func assertReadablePrefix(_ result: AudioRecordingResult, frames: Int64 = 8000, file: StaticString = #filePath, line: UInt = #line) throws {
        let path = try XCTUnwrap(result.filePath, "A recoverable result must expose its path", file: file, line: line)
        let audioFile = try AVAudioFile(forReading: URL(fileURLWithPath: path))
        XCTAssertEqual(audioFile.length, frames, "Prefix should hold every frame written before close", file: file, line: line)
        XCTAssertEqual(result.durationSeconds, Double(frames) / 16000.0, accuracy: 0.0001, file: file, line: line)
        XCTAssertGreaterThan(result.fileSize, 0, file: file, line: line)
    }

    func testStopReportsSavedOnlyAfterFileIsClosedAndReadBack() throws {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))
        XCTAssertTrue(recorder.isRecording)
        XCTAssertNotNil(recorder.sessionStartTime)

        let result = recorder.stopRecording()

        XCTAssertEqual(result.outcome, .saved)
        XCTAssertTrue(result.success)
        XCTAssertEqual(engine.closeCount, 1, "The file must be closed before the result is reported")
        XCTAssertEqual(result.filePath, recordingURL.path)
        try assertReadablePrefix(result)
        XCTAssertFalse(recorder.isRecording)
    }

    func testEngineFailureAtCloseIsNotReportedAsSuccess() throws {
        engine.closeBehavior = .reportFailure
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.stopRecording()

        XCTAssertFalse(result.success)
        XCTAssertEqual(result.outcome, .partial(reason: AudioRecordingReason.finalizationFailed))
        try assertReadablePrefix(result)
    }

    func testCorruptFileAfterCloseIsHonestFailureWithoutPath() {
        engine.closeBehavior = .corruptFile
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.stopRecording()

        XCTAssertFalse(result.success)
        XCTAssertEqual(result.outcome, .failed(reason: AudioRecordingReason.noRecoverableAudio))
        XCTAssertNil(result.filePath)
        XCTAssertEqual(result.fileSize, 0)
    }

    func testEmptyFileAfterCloseIsHonestFailureWithoutPath() {
        engine.closeBehavior = .truncateToEmpty
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.stopRecording()

        XCTAssertEqual(result.outcome, .failed(reason: AudioRecordingReason.noRecoverableAudio))
        XCTAssertNil(result.filePath)
    }

    func testRecordingWithNoFramesIsHonestFailure() {
        engine.framesWrittenAtStart = 0
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.stopRecording()

        XCTAssertFalse(result.success)
        XCTAssertNil(result.filePath)
    }

    func testCancelKeepsRecoverablePrefixAsPartial() throws {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.cancelRecording()

        XCTAssertFalse(result.success)
        XCTAssertEqual(result.outcome, .partial(reason: AudioRecordingReason.cancelled))
        XCTAssertEqual(engine.closeCount, 1, "Cancel must close the file before inspecting it")
        try assertReadablePrefix(result)
    }

    func testCancelWithUnrecoverableAudioIsHonestFailure() {
        engine.closeBehavior = .corruptFile
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        let result = recorder.cancelRecording()

        XCTAssertEqual(
            result.outcome,
            .failed(reason: "\(AudioRecordingReason.cancelled); \(AudioRecordingReason.noRecoverableAudio)")
        )
        XCTAssertNil(result.filePath)
    }

    func testInterruptionFinalizesAndPreservesPrefixImmediately() throws {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        postInterruption(.began)

        let earlyResult = try XCTUnwrap(endedEarlyResults.first, "The interruption must be reported without waiting for Stop")
        XCTAssertEqual(endedEarlyResults.count, 1)
        XCTAssertEqual(earlyResult.outcome, .partial(reason: AudioRecordingReason.interrupted))
        XCTAssertFalse(earlyResult.success)
        XCTAssertEqual(engine.closeCount, 1)
        XCTAssertFalse(recorder.isRecording)
        try assertReadablePrefix(earlyResult)

        let stopResult = recorder.stopRecording()
        XCTAssertEqual(stopResult.outcome, earlyResult.outcome)
        XCTAssertEqual(stopResult.filePath, earlyResult.filePath)
        XCTAssertEqual(engine.closeCount, 1, "Stop must not close the engine a second time")
    }

    func testInterruptionWithUnreadablePrefixIsHonestFailure() {
        engine.closeBehavior = .corruptFile
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        postInterruption(.began)

        XCTAssertEqual(
            endedEarlyResults.first?.outcome,
            AudioRecordingOutcome.failed(reason: "\(AudioRecordingReason.interrupted); \(AudioRecordingReason.noRecoverableAudio)")
        )
        XCTAssertNil(endedEarlyResults.first?.filePath)

        let stopResult = recorder.stopRecording()
        XCTAssertFalse(stopResult.success)
        XCTAssertNil(stopResult.filePath)
    }

    func testInterruptionEndedWithShouldResumeDoesNotRestartOrOverwritePrefix() throws {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        postInterruption(.began)
        postInterruption(.ended, options: .shouldResume)

        XCTAssertEqual(engine.startCount, 1, "Resuming into the same URL would truncate the saved prefix")
        XCTAssertEqual(endedEarlyResults.count, 1)

        let result = recorder.stopRecording()
        XCTAssertFalse(result.success, "A capture with an interruption must never be reported as clean success")
        XCTAssertEqual(result.outcome, .partial(reason: AudioRecordingReason.interrupted))
        try assertReadablePrefix(result)
    }

    func testInterruptionWhileIdleIsIgnored() {
        postInterruption(.began)

        XCTAssertTrue(endedEarlyResults.isEmpty)
        XCTAssertEqual(engine.closeCount, 0)
    }

    func testStopWithoutStartFails() {
        let result = recorder.stopRecording()

        XCTAssertEqual(result.outcome, .failed(reason: AudioRecordingReason.noActiveRecording))
        XCTAssertNil(result.filePath)
        XCTAssertEqual(result.durationSeconds, 0)
    }

    func testDoubleStopDoesNotReportSuccessTwice() {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        XCTAssertTrue(recorder.stopRecording().success)
        let second = recorder.stopRecording()

        XCTAssertFalse(second.success)
        XCTAssertEqual(second.outcome, .failed(reason: AudioRecordingReason.noActiveRecording))
        XCTAssertEqual(engine.closeCount, 1)
    }

    func testMicrophonePermissionDenialLeavesNoActiveRecording() {
        engine.startError = AudioCaptureError(reason: "Microphone permission denied")

        XCTAssertFalse(recorder.startRecording(to: recordingURL))
        XCTAssertEqual(recorder.lastStartFailure, "Microphone permission denied")
        XCTAssertFalse(recorder.isRecording)
        XCTAssertNil(recorder.sessionStartTime)

        let result = recorder.stopRecording()
        XCTAssertEqual(result.outcome, .failed(reason: AudioRecordingReason.noActiveRecording))
        XCTAssertFalse(FileManager.default.fileExists(atPath: recordingURL.path))
    }

    func testFailedStartClearsStateAndAllowsLaterRecording() {
        engine.startError = AudioCaptureError(reason: "Recording start failed")
        XCTAssertFalse(recorder.startRecording(to: recordingURL))
        XCTAssertFalse(recorder.stopRecording().success)

        engine.startError = nil
        XCTAssertTrue(recorder.startRecording(to: recordingURL))
        XCTAssertNil(recorder.lastStartFailure)
        XCTAssertTrue(recorder.stopRecording().success)
    }

    func testStartWhileRecordingIsRefusedAndKeepsFirstSession() {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))
        let secondURL = recordingURL.deletingLastPathComponent()
            .appendingPathComponent("audio-probe-second-\(UUID().uuidString).wav")

        XCTAssertFalse(recorder.startRecording(to: secondURL))

        XCTAssertEqual(recorder.lastStartFailure, "Recording already active")
        XCTAssertEqual(engine.startCount, 1)
        XCTAssertFalse(FileManager.default.fileExists(atPath: secondURL.path))
        XCTAssertEqual(recorder.stopRecording().filePath, recordingURL.path)
    }

    func testSequentialSessionsAreIndependent() {
        XCTAssertTrue(recorder.startRecording(to: recordingURL))
        postInterruption(.began)
        _ = recorder.stopRecording()

        let secondURL = recordingURL.deletingLastPathComponent()
            .appendingPathComponent("audio-probe-next-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: secondURL) }
        XCTAssertTrue(recorder.startRecording(to: secondURL))

        let second = recorder.stopRecording()
        XCTAssertEqual(second.outcome, .saved)
        XCTAssertEqual(second.filePath, secondURL.path)
    }

    func testInsufficientFreeSpaceRefusesToStart() {
        recorder = makeRecorder(freeBytes: 10)

        XCTAssertFalse(recorder.startRecording(to: recordingURL))
        XCTAssertEqual(recorder.lastStartFailure, "Insufficient free space")
        XCTAssertEqual(engine.startCount, 0)
    }

    func testUnknownFreeSpaceFailsClosed() {
        recorder = makeRecorder(freeBytes: nil)

        XCTAssertFalse(recorder.startRecording(to: recordingURL))
        XCTAssertEqual(recorder.lastStartFailure, "Free space unknown")
        XCTAssertEqual(engine.startCount, 0)
    }

    func testDurationLimitIsEnforcedByPassingItToTheEngine() {
        recorder = makeRecorder(limits: AudioRecordingLimits(maxDurationSeconds: 42, minimumFreeBytes: 1))

        XCTAssertTrue(recorder.startRecording(to: recordingURL))

        XCTAssertEqual(engine.lastMaxDuration, 42)
        _ = recorder.stopRecording()
    }

    func testDefaultLimitsMatchDocumentedProbeBounds() {
        let limits = AudioRecordingLimits()

        XCTAssertEqual(limits.maxDurationSeconds, 300)
        XCTAssertEqual(limits.minimumFreeBytes, 50_000_000)
    }

    func testRealMicrophoneEngineEitherSavesReadableAudioOrReportsHonestly() throws {
        let realRecorder = AudioRecorder(engine: AVAudioRecorderEngine(), notificationCenter: NotificationCenter())
        guard realRecorder.startRecording(to: recordingURL) else {
            throw XCTSkip("Real microphone capture unavailable here: \(realRecorder.lastStartFailure ?? "unknown")")
        }
        Thread.sleep(forTimeInterval: 0.3)

        let result = realRecorder.stopRecording()

        if result.success {
            let path = try XCTUnwrap(result.filePath)
            let audioFile = try AVAudioFile(forReading: URL(fileURLWithPath: path))
            XCTAssertGreaterThan(audioFile.length, 0)
        } else {
            XCTAssertNotNil(result.interruption, "A non-success result must say why")
        }
    }
}

/// Drives the real `AVAudioRecorderEngine` delegate path with an attached (never started) `AVAudioRecorder`, so no microphone is needed.
final class AVAudioRecorderEngineFinalizationTests: XCTestCase {
    private var scratchURLs: [URL] = []

    override func tearDown() {
        scratchURLs.forEach { try? FileManager.default.removeItem(at: $0) }
        scratchURLs = []
        super.tearDown()
    }

    private func makeAttachedEngine(finishWaitSeconds: TimeInterval = 0.2) throws -> (AVAudioRecorderEngine, AVAudioRecorder) {
        let scratchURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("audio-engine-\(UUID().uuidString).wav")
        scratchURLs.append(scratchURL)
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16
        ]
        let attachedRecorder = try AVAudioRecorder(url: scratchURL, settings: settings)
        let engine = AVAudioRecorderEngine(finishWaitSeconds: finishWaitSeconds)
        engine.attachRecorderForTesting(attachedRecorder)
        return (engine, attachedRecorder)
    }

    func testSuccessfulFinishDeliveredDuringStopConfirmsClose() throws {
        let (engine, attachedRecorder) = try makeAttachedEngine()
        DispatchQueue.main.async { engine.audioRecorderDidFinishRecording(attachedRecorder, successfully: true) }

        XCTAssertTrue(engine.stopAndClose())
    }

    func testFalseFinishDeliveredDuringStopIsNotReportedAsClean() throws {
        let (engine, attachedRecorder) = try makeAttachedEngine()
        DispatchQueue.main.async { engine.audioRecorderDidFinishRecording(attachedRecorder, successfully: false) }

        XCTAssertFalse(engine.stopAndClose())
    }

    func testFalseFinishDeliveredBeforeStopIsNotReportedAsClean() throws {
        let (engine, attachedRecorder) = try makeAttachedEngine()
        engine.audioRecorderDidFinishRecording(attachedRecorder, successfully: false)

        XCTAssertFalse(engine.stopAndClose())
    }

    func testEncodeErrorIsNotReportedAsClean() throws {
        let (engine, attachedRecorder) = try makeAttachedEngine()
        engine.audioRecorderEncodeErrorDidOccur(attachedRecorder, error: nil)
        engine.audioRecorderDidFinishRecording(attachedRecorder, successfully: true)

        XCTAssertFalse(engine.stopAndClose())
    }

    func testMissingFinishCallbackWithinBoundIsUnconfirmed() throws {
        let (engine, _) = try makeAttachedEngine(finishWaitSeconds: 0.05)

        XCTAssertFalse(engine.stopAndClose())
    }

    func testFinishFromUnrelatedRecorderIsIgnored() throws {
        let (engine, _) = try makeAttachedEngine(finishWaitSeconds: 0.05)
        let (_, unrelatedRecorder) = try makeAttachedEngine()

        engine.audioRecorderDidFinishRecording(unrelatedRecorder, successfully: true)

        XCTAssertFalse(engine.stopAndClose())
    }

    func testStopWithoutRecorderIsNotClean() {
        XCTAssertFalse(AVAudioRecorderEngine(finishWaitSeconds: 0.05).stopAndClose())
    }
}
