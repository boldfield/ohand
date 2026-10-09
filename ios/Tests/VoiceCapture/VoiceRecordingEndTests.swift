import AVFoundation
import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Every way a recording can end closes the file first, reads it back, and reports one honest outcome. Audio ownership
/// moves from the in-progress store to the finalized store only through the real ingress service, and "saved" appears
/// only after the (scripted) core confirmation.
final class VoiceRecordingEndTests: VoiceCaptureTestCase {
    private let captureID = "voice-test-1"

    private func savedSummary() throws -> VoiceRecordingSummary {
        guard case .saved(let summary, _) = try lastOutcome() else {
            XCTFail("expected saved, got \(String(describing: outcomes.last))")
            throw UnexpectedOutcomeError.notSaved
        }
        return summary
    }

    // MARK: User stop and ownership

    func testUserStopHandsTheAudioToIngressAndIsSavedOnlyAfterConfirmation() throws {
        _ = try startRecording()
        confirmNextImport(captureID)
        var stateAtImport: VoiceRecorderState?
        var durableAtImport = false
        importer.onImport = { [unowned self] _ in
            stateAtImport = controller.state
            durableAtImport = exists(recordURL(captureID)) && exists(finalizedRecording(captureID))
        }

        controller.stop()

        guard case .saved(let summary, let acknowledgment) = try lastOutcome() else { return XCTFail("expected saved") }
        XCTAssertTrue(summary.isComplete)
        XCTAssertEqual(summary.end, .stoppedByUser)
        XCTAssertEqual(summary.durationSeconds, 0.5, accuracy: 0.001)
        XCTAssertEqual(acknowledgment.itemID, "item-1")
        XCTAssertEqual(outcomes.count, 1)
        XCTAssertEqual(stateAtImport, .idle, "a new capture is usable while the import is in flight")
        XCTAssertTrue(durableAtImport, "the staging record and the finalized audio are durable before the core is asked")
        XCTAssertFalse(exists(recordingFile(captureID)), "the audio was moved, not copied")
        XCTAssertTrue(exists(finalizedRecording(captureID)))
        XCTAssertFalse(exists(recordURL(captureID)), "the staging record is removed only after confirmation")
        XCTAssertEqual(try Data(contentsOf: finalizedRecording(captureID)).count, summary.fileSizeBytes)
        XCTAssertEqual(engine.closeCount, 1)
        XCTAssertFalse(poller.isPolling)
        XCTAssertEqual(states.count, 3)
        XCTAssertEqual(states.last, .idle)

        let record = try XCTUnwrap(importer.importedRecords.first)
        XCTAssertEqual(record.captureID, captureID)
        XCTAssertNil(record.text)
        XCTAssertEqual(record.audio, IngressAudioHandoff(inProgressFileName: "voice-test-1.wav", finalizedFileName: "voice-test-1.wav"))
        XCTAssertEqual(record.coreAudioReference, "FinalizedAudio/voice-test-1.wav")
    }

    func testAnUnconfirmedImportIsKeptNotSavedAndRecoversWithoutDuplicates() throws {
        _ = try startRecording()
        importer.results = [.failed(.notCommitted)]

        controller.stop()

        guard case .keptForRetry(let summary, .importUnconfirmed) = try lastOutcome() else {
            return XCTFail("expected keptForRetry, got \(String(describing: outcomes.last))")
        }
        XCTAssertTrue(summary.isComplete, "the recording is complete; only the import is unconfirmed")
        XCTAssertTrue(exists(recordURL(captureID)), "the durable staging record remains")
        XCTAssertTrue(exists(finalizedRecording(captureID)))

        confirmNextImport(captureID)
        let report = try XCTUnwrap(recover(service))
        guard case .saved(let acknowledgment)? = report.entries.first?.outcome else { return XCTFail("expected recovery to save") }
        XCTAssertEqual(report.entries.count, 1)
        XCTAssertEqual(acknowledgment.captureID, captureID)
        XCTAssertFalse(exists(recordURL(captureID)))
    }

    func testAHandoffThatCannotStageLeavesTheAudioRecoverableInPlace() throws {
        _ = try startRecording()
        failingFileSystem.failing = [.write]

        controller.stop()

        guard case .keptForRetry(_, .notStaged) = try lastOutcome() else {
            return XCTFail("expected notStaged, got \(String(describing: outcomes.last))")
        }
        XCTAssertTrue(exists(recordingFile(captureID)), "the recording was not moved or deleted")
        XCTAssertFalse(exists(recordURL(captureID)))
        XCTAssertTrue(importer.importedRecords.isEmpty)

        failingFileSystem.failing = []
        let report = try XCTUnwrap(recover(service))
        XCTAssertEqual(report.unclaimedInProgressAudio, ["voice-test-1.wav"], "recovery sees the recording instead of hiding it")
        XCTAssertTrue(report.entries.isEmpty)
    }

    func testADeletedItemIsReportedAsDeletedNotSaved() throws {
        _ = try startRecording()
        importer.results = [.failed(.itemDeleted)]

        controller.stop()

        guard case .itemDeleted = try lastOutcome() else { return XCTFail("got \(String(describing: outcomes.last))") }
    }

    func testHandoffAnsweringLaterLeavesTheRecorderFreeForTheNextCapture() throws {
        _ = try startRecording()
        confirmNextImport(captureID)
        importer.holdsCompletions = true

        controller.stop()

        XCTAssertEqual(controller.state, .idle)
        XCTAssertEqual(controller.pendingHandoffCount, 1)
        XCTAssertTrue(outcomes.isEmpty, "nothing is reported as saved before the core answers")
        XCTAssertEqual(try startRecording().captureID, "voice-test-2")

        importer.release()

        XCTAssertEqual(controller.pendingHandoffCount, 0)
        XCTAssertEqual(try savedSummary().captureID, captureID)
        guard case .recording = controller.state else { return XCTFail("the second recording continues") }
    }

    // MARK: Early ends

    private func endEarly(_ expected: VoiceRecordingEnd, trigger: () -> Void) throws {
        _ = try startRecording()
        confirmNextImport(captureID)

        trigger()

        let summary = try savedSummary()
        XCTAssertEqual(summary.end, expected)
        XCTAssertFalse(summary.isComplete, "an early end is never reported as a complete recording")
        XCTAssertEqual(summary.durationSeconds, 0.5, accuracy: 0.001, "the readable prefix is kept")
        XCTAssertEqual(outcomes.count, 1)
        XCTAssertEqual(engine.closeCount, 1)
        XCTAssertEqual(controller.state, .idle)
        XCTAssertFalse(poller.isPolling)
        XCTAssertTrue(exists(finalizedRecording(captureID)))
    }

    func testAudioInterruptionKeepsThePrefixAndNeverResumesIntoTheSameFile() throws {
        try endEarly(.audioInterrupted) { postInterruption(.began) }

        postInterruption(.ended)
        controller.stop()

        XCTAssertEqual(engine.startCount, 1, "the interruption ending does not restart the recorder")
        XCTAssertEqual(outcomes.count, 1)
        XCTAssertEqual(engine.closeCount, 1)

        importer.results = [FakeForegroundIngressImporter.confirmation("voice-test-2")]
        XCTAssertEqual(try startRecording().captureID, "voice-test-2", "a new capture uses a new file")
        XCTAssertTrue(exists(recordingFile("voice-test-2")))
        XCTAssertTrue(exists(finalizedRecording(captureID)), "the first recording is untouched")
    }

    func testLeavingTheForegroundEndsTheRecordingAndKeepsThePrefix() throws {
        try endEarly(.leftForeground) { postBackground() }
    }

    func testDeviceLockEndsTheRecordingAndKeepsThePrefix() throws {
        try endEarly(.deviceLocking) { postDeviceLocking() }
    }

    func testDurationLimitReportedByTheEngine() throws {
        try endEarly(.durationLimit) { engine.onEvent?(.reachedDurationLimit) }
        XCTAssertTrue(try savedSummary().end.reachedLimit)
    }

    func testDurationLimitEnforcedByThePollWhenTheEngineStaysSilent() throws {
        _ = try startRecording()
        confirmNextImport(captureID)

        now = now.addingTimeInterval(VoiceRecordingLimits().maxDurationSeconds - 1)
        poller.tick()
        guard case .recording = controller.state else { return XCTFail("still within the limit") }

        now = now.addingTimeInterval(1)
        poller.tick()

        XCTAssertEqual(try savedSummary().end, .durationLimit)
    }

    func testSizeLimitStopsTheRecordingAtThePoll() throws {
        var limits = VoiceRecordingLimits()
        limits.maxFileBytes = 1_000
        controller = makeController(limits: limits)
        try endEarly(.sizeLimit) { poller.tick() }
    }

    func testLowStorageWhileRecordingStopsAndKeepsWhatWasCaptured() throws {
        try endEarly(.lowStorage) {
            freeSpace = VoiceRecordingLimits().minimumFreeBytesWhileRecording - 1
            poller.tick()
        }
    }

    func testRecorderFailureKeepsTheReadablePrefix() throws {
        try endEarly(.recorderFailed) { engine.onEvent?(.failed) }
    }

    func testAnUnconfirmedCloseWithAReadablePrefixIsPartialNotComplete() throws {
        _ = try startRecording()
        engine.closeBehavior = .reportFailure
        confirmNextImport(captureID)

        controller.stop()

        let summary = try savedSummary()
        XCTAssertEqual(summary.end, .stoppedByUser)
        XCTAssertFalse(summary.closedCleanly)
        XCTAssertFalse(summary.isComplete)
    }

    func testAnEventArrivingWhileTheFileIsClosingCannotProduceASecondOutcome() throws {
        _ = try startRecording()
        confirmNextImport(captureID)
        engine.duringClose = { [unowned self] in
            postInterruption(.began)
            postBackground()
            engine.onEvent?(.failed)
            poller.tick()
        }

        controller.stop()

        XCTAssertEqual(outcomes.count, 1)
        XCTAssertEqual(try savedSummary().end, .stoppedByUser)
        XCTAssertEqual(engine.closeCount, 1)
        XCTAssertEqual(importer.importedRecords.count, 1)
    }

    func testRecordingFileProtectionFailureIsReportedAndDoesNotLoseAudio() throws {
        protectionError = NSError(domain: "synthetic.protection", code: 3)
        _ = try startRecording()
        confirmNextImport(captureID)

        controller.stop()

        let summary = try savedSummary()
        XCTAssertFalse(summary.protectionApplied)
        XCTAssertTrue(exists(finalizedRecording(captureID)))
    }

    // MARK: Unreadable audio and cancellation

    func testUnreadableAudioIsNeverHandedOffAndItsBytesAreKept() throws {
        let triggers: [() -> Void] = [{ self.controller.stop() }, { self.postInterruption(.began) }]
        for (index, trigger) in triggers.enumerated() {
            let id = "voice-test-\(index + 1)"
            _ = try startRecording()
            engine.closeBehavior = .corruptFile

            trigger()

            let expectedEnd: VoiceRecordingEnd = index == 0 ? .stoppedByUser : .audioInterrupted
            XCTAssertEqual(try lastOutcome(), .unrecoverable(captureID: id, end: expectedEnd, fileRetained: true))
            XCTAssertEqual(try Data(contentsOf: recordingFile(id)), Data([0x00, 0x01, 0x02]), "unreadable bytes stay for recovery")
            XCTAssertFalse(exists(recordURL(id)))
        }
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertEqual(controller.state, .idle)
    }

    func testAnEmptyFileIsRemovedAndReportedAsNothingRecorded() throws {
        _ = try startRecording()
        engine.closeBehavior = .truncateToEmpty

        controller.stop()

        XCTAssertEqual(try lastOutcome(), .unrecoverable(captureID: captureID, end: .stoppedByUser, fileRetained: false))
        XCTAssertFalse(exists(recordingFile(captureID)))
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    func testCancelKeepsTheAudioUnsubmittedAndIsIdempotent() throws {
        _ = try startRecording()

        controller.cancel()

        guard case .retainedUnsubmitted(let summary) = try lastOutcome() else { return XCTFail("got \(String(describing: outcomes.last))") }
        XCTAssertEqual(summary.end, .cancelledByUser)
        XCTAssertFalse(summary.isComplete)
        XCTAssertTrue(exists(recordingFile(captureID)), "cancel does not erase what was recorded")
        XCTAssertFalse(exists(finalizedRecording(captureID)))
        XCTAssertFalse(exists(recordURL(captureID)))
        XCTAssertTrue(importer.importedRecords.isEmpty, "a cancelled recording is never acknowledged")
        XCTAssertEqual(controller.state, .idle)
        XCTAssertFalse(poller.isPolling)

        controller.cancel()
        controller.stop()

        XCTAssertEqual(outcomes.count, 1)
        XCTAssertEqual(engine.closeCount, 1)
    }

    func testStopAndCancelWhileIdleDoNothing() throws {
        controller.stop()
        controller.cancel()
        postInterruption(.began)
        postBackground()
        postDeviceLocking()

        XCTAssertTrue(outcomes.isEmpty)
        XCTAssertEqual(engine.closeCount, 0)
        XCTAssertTrue(states.isEmpty)
    }
}
