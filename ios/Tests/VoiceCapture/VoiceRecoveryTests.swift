import AVFoundation
import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Re-entry after an interruption or a cancellation: recordings the user never finished are found again without any
/// cleanup queue, each offers bounded finish, continue and delete choices, nothing is deleted or submitted without an
/// explicit call for that one recording, and the messages state only what was read back from the files.
final class VoiceRecoveryTests: VoiceCaptureTestCase {
    private func restart(limits: VoiceRecordingLimits = VoiceRecordingLimits(), maxListedRecordings: Int = 20) {
        controller = makeController(limits: limits)
        coordinator = makeCoordinator(limits: limits, maxListedRecordings: maxListedRecordings)
    }

    private func staleRecording(_ captureID: String, canContinue: Bool = true) -> RecoverableVoiceRecording {
        RecoverableVoiceRecording(
            captureID: captureID, fileName: "\(captureID).wav", fileSizeBytes: 100, durationSeconds: 0.5,
            modifiedAt: nil, canContinue: canContinue)
    }

    private func finishResult(_ recording: RecoverableVoiceRecording) throws -> Result<VoiceCaptureOutcome, VoiceRecoveryFailure> {
        var result: Result<VoiceCaptureOutcome, VoiceRecoveryFailure>?
        coordinator.finish(recording) { result = $0 }
        return try XCTUnwrap(result, "finish did not complete")
    }

    private func deleteResult(_ recording: RecoverableVoiceRecording) throws -> Result<VoiceDeletedRecording, VoiceRecoveryFailure> {
        var result: Result<VoiceDeletedRecording, VoiceRecoveryFailure>?
        coordinator.delete(recording) { result = $0 }
        return try XCTUnwrap(result, "delete did not complete")
    }

    private func continueResult(_ recording: RecoverableVoiceRecording) throws -> Result<VoiceRecordingStarted, VoiceRecoveryFailure> {
        var result: Result<VoiceRecordingStarted, VoiceRecoveryFailure>?
        coordinator.continueRecording(recording) { result = $0 }
        return try XCTUnwrap(result, "continue did not complete")
    }

    private func failure<Value>(_ result: Result<Value, VoiceRecoveryFailure>) -> VoiceRecoveryFailure? {
        if case .failure(let failure) = result { return failure }
        return nil
    }

    // MARK: Discovery

    func testACancelledRecordingIsFoundAgainAfterRestartAndNothingWasSubmitted() throws {
        _ = try startRecording()
        controller.cancel()
        guard case .retainedUnsubmitted(let cancelled) = try lastOutcome() else { return XCTFail("expected a retained recording") }

        restart()
        let listing = try refreshListing()

        let recording = try XCTUnwrap(listing.recordings.first)
        XCTAssertEqual(listing.recordings.count, 1)
        XCTAssertEqual(recording.captureID, "voice-test-1")
        XCTAssertEqual(try XCTUnwrap(recording.durationSeconds), cancelled.durationSeconds, accuracy: 0.001)
        XCTAssertEqual(recording.fileSizeBytes, Int64(cancelled.fileSizeBytes))
        XCTAssertTrue(recording.canFinish)
        XCTAssertTrue(recording.canContinue)
        XCTAssertTrue(importer.importedRecords.isEmpty, "discovery submits nothing")
        XCTAssertTrue(exists(recordingFile("voice-test-1")), "a cancellation never erased the audio")
        XCTAssertFalse(listing.recordingsUnknown)
        XCTAssertEqual(listing.notShownCount, 0)
    }

    func testNormalCaptureIsUsableImmediatelyWhileRecoverableRecordingsExist() throws {
        try leaveRecording("voice-left-1")
        restart()
        _ = try refreshListing()

        let started = try startRecording()
        confirmNextImport(started.captureID)
        controller.stop()

        guard case .saved = try lastOutcome() else { return XCTFail("expected saved, got \(String(describing: outcomes.last))") }
        XCTAssertTrue(exists(recordingFile("voice-left-1")), "the unfinished recording is still offered, not consumed")
    }

    func testRecoveryNeverBlocksStartingANewRecording() throws {
        _ = try startRecording()
        controller.stop()
        restart()
        importer.holdsCompletions = true
        var listing: VoiceRecoveryListing?
        coordinator.refresh { listing = $0 }
        XCTAssertNil(listing, "the ingress pass is waiting on the core")

        let started = try startRecording()
        XCTAssertEqual(started.captureID, "voice-test-2")
        controller.cancel()
        importer.release()

        let completed = try XCTUnwrap(listing)
        XCTAssertEqual(completed.pendingCaptures, [VoicePendingCapture(captureID: "voice-test-1", reason: .importUnconfirmed)])
        XCTAssertEqual(completed.recordings.map(\.captureID), ["voice-test-2"])
    }

    func testRecordingsCutOffBeforeHandoffAreListedWithHonestFacts() throws {
        try leaveRecording("voice-killed-1", frames: 16000)
        try Data([0x00, 0x01, 0x02]).write(to: recordingFile("voice-garbled"))
        FileManager.default.createFile(atPath: recordingFile("voice-empty").path, contents: nil)

        let listing = try refreshListing()

        XCTAssertEqual(Set(listing.recordings.map(\.fileName)), ["voice-killed-1.wav", "voice-garbled.wav"],
                       "an empty file holds no audio and is not offered")
        let killed = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-killed-1" })
        XCTAssertEqual(try XCTUnwrap(killed.durationSeconds), 1.0, accuracy: 0.001)
        XCTAssertTrue(killed.canContinue)
        let garbled = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-garbled" })
        XCTAssertNil(garbled.durationSeconds, "unreadable bytes are not given a made-up duration")
        XCTAssertEqual(garbled.fileSizeBytes, 3)
        XCTAssertFalse(garbled.canFinish)
        XCTAssertFalse(garbled.canContinue)

        let rows = VoiceRecoveryMessages.rows(for: listing)
        XCTAssertEqual(rows.first { $0.fileName == "voice-killed-1.wav" }?.actions, [.finish, .continueRecording, .delete])
        XCTAssertEqual(rows.first { $0.fileName == "voice-garbled.wav" }?.actions, [.delete])
        XCTAssertTrue(exists(recordingFile("voice-empty")), "listing deletes nothing")
        XCTAssertTrue(exists(recordingFile("voice-garbled")))
    }

    func testAnInterruptedRecordingThatReachedIngressIsPendingNotOfferedAsUnsubmitted() throws {
        _ = try startRecording()
        postInterruption(.began)
        guard case .keptForRetry(let summary, .importUnconfirmed) = try lastOutcome() else { return XCTFail("expected keptForRetry") }
        XCTAssertEqual(summary.end, .audioInterrupted)

        restart()
        var listing = try refreshListing()
        XCTAssertTrue(listing.recordings.isEmpty)
        XCTAssertEqual(listing.pendingCaptures, [VoicePendingCapture(captureID: "voice-test-1", reason: .importUnconfirmed)])
        XCTAssertTrue(VoiceRecoveryMessages.notices(for: listing).contains { $0.contains("not confirmed saved") })

        confirmNextImport("voice-test-1")
        listing = try refreshListing()
        XCTAssertEqual(listing.confirmedCaptureIDs, ["voice-test-1"])
        XCTAssertTrue(listing.pendingCaptures.isEmpty)
    }

    func testUnknownIngressStateListsNothingAndRefusesEveryAction() throws {
        _ = try startRecording()
        controller.stop()
        try leaveRecording("voice-left-1")
        restart()
        failingFileSystem.failing = [.read]

        let listing = try refreshListing()

        XCTAssertTrue(listing.recordingsUnknown)
        XCTAssertTrue(listing.recordings.isEmpty, "an unknown state is never shown as a list")
        XCTAssertEqual(listing.pendingCaptures.first?.reason, .stagingRecordUnreadable)
        XCTAssertTrue(VoiceRecoveryMessages.notices(for: listing).contains { $0.contains("could not be checked") })
        let startsBefore = engine.startCount
        XCTAssertEqual(failure(try deleteResult(staleRecording("voice-left-1"))), .ingressStateUnknown)
        XCTAssertEqual(failure(try continueResult(staleRecording("voice-left-1"))), .ingressStateUnknown)
        XCTAssertEqual(engine.startCount, startsBefore)
        XCTAssertTrue(exists(recordingFile("voice-left-1")))
    }

    func testListingIsBoundedNewestFirstAndSaysHowManyAreNotShown() throws {
        let base = Date(timeIntervalSince1970: 1_789_000_000)
        for (index, name) in ["voice-a", "voice-b", "voice-c", "voice-d", "voice-e"].enumerated() {
            try leaveRecording(name, modifiedAt: base.addingTimeInterval(Double(index) * 60))
        }
        restart(maxListedRecordings: 3)

        let listing = try refreshListing()

        XCTAssertEqual(listing.recordings.map(\.captureID), ["voice-e", "voice-d", "voice-c"])
        XCTAssertEqual(listing.notShownCount, 2)
        XCTAssertTrue(VoiceRecoveryMessages.notices(for: listing).contains { $0.contains("2 recordings") })
        XCTAssertTrue(exists(recordingFile("voice-a")), "recordings beyond the bound stay on the device")
    }

    func testTheActiveRecordingIsNeverOffered() throws {
        try leaveRecording("voice-left-1")
        _ = try startRecording()

        let listing = try refreshListing()

        XCTAssertEqual(listing.recordings.map(\.captureID), ["voice-left-1"])
        XCTAssertEqual(failure(try deleteResult(staleRecording("voice-test-1"))), .recordingActive)
        XCTAssertEqual(failure(try finishResult(staleRecording("voice-test-1"))), .recordingActive)
        XCTAssertTrue(exists(recordingFile("voice-test-1")))
    }

    // MARK: Finish

    func testFinishSavesOnlyAfterTheCoreConfirmsAndNeverCallsItComplete() throws {
        let modifiedAt = Date(timeIntervalSince1970: 1_789_999_000)
        try leaveRecording("voice-left-1", frames: 16000, modifiedAt: modifiedAt)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        confirmNextImport("voice-left-1")

        let outcome = try finishResult(recording).get()

        guard case .saved(let summary, let acknowledgment) = outcome else { return XCTFail("expected saved, got \(outcome)") }
        XCTAssertEqual(acknowledgment.itemID, "item-1")
        XCTAssertEqual(summary.end, .recoveredOnReentry)
        XCTAssertFalse(summary.isComplete, "how a recovered recording ended is unknown")
        XCTAssertEqual(summary.durationSeconds, 1.0, accuracy: 0.001)
        XCTAssertFalse(exists(recordingFile("voice-left-1")))
        XCTAssertTrue(exists(finalizedRecording("voice-left-1")))
        XCTAssertFalse(exists(recordURL("voice-left-1")))
        let record = try XCTUnwrap(importer.importedRecords.first)
        XCTAssertEqual(record.captureID, "voice-left-1")
        XCTAssertEqual(
            record.context.captureInstant,
            ISO8601DateFormatter().string(from: modifiedAt.addingTimeInterval(-1.0)))
        let message = VoiceRecoveryMessages.message(for: outcome)
        XCTAssertTrue(message.hasPrefix("Saved, but the recording ended early"))
        XCTAssertTrue(message.contains("found again after the app restarted"))
    }

    func testFinishWithoutConfirmationIsKeptNotSavedAndLeavesTheAudioDurable() throws {
        try leaveRecording("voice-left-1")
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        importer.results = [.failed(.notCommitted)]

        let outcome = try finishResult(recording).get()

        guard case .keptForRetry(_, .importUnconfirmed) = outcome else { return XCTFail("expected keptForRetry, got \(outcome)") }
        let message = VoiceRecoveryMessages.message(for: outcome)
        XCTAssertFalse(message.hasPrefix("Saved"))
        XCTAssertTrue(message.contains("Not saved yet"))
        XCTAssertTrue(exists(finalizedRecording("voice-left-1")))
        XCTAssertTrue(exists(recordURL("voice-left-1")))
        let listing = try refreshListing()
        XCTAssertTrue(listing.recordings.isEmpty, "a staged recording is retried by ingress, not offered twice")
        XCTAssertEqual(listing.pendingCaptures.map(\.captureID), ["voice-left-1"])
    }

    func testFinishRefusesAnUnreadableRecordingAndKeepsItsBytes() throws {
        try Data([0x00, 0x01, 0x02]).write(to: recordingFile("voice-garbled"))
        let recording = try XCTUnwrap(try refreshListing().recordings.first)

        XCTAssertEqual(failure(try finishResult(recording)), .notReadable)

        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertEqual(try Data(contentsOf: recordingFile("voice-garbled")), Data([0x00, 0x01, 0x02]))
    }

    func testAFinishAlreadyInFlightRefusesASecondAndTheFileIsHandledOnce() throws {
        try leaveRecording("voice-left-1")
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        confirmNextImport("voice-left-1")
        importer.holdsCompletions = true
        var first: Result<VoiceCaptureOutcome, VoiceRecoveryFailure>?
        coordinator.finish(recording) { first = $0 }
        XCTAssertNil(first)

        XCTAssertEqual(failure(try finishResult(recording)), .alreadyInProgress)
        XCTAssertEqual(failure(try deleteResult(recording)), .alreadyInProgress)
        importer.release()

        guard case .saved? = try first?.get() else { return XCTFail("expected the first finish to save") }
        XCTAssertEqual(importer.importedRecords.count, 1)
        XCTAssertEqual(failure(try finishResult(recording)), .recordingNotFound)
    }

    // MARK: Delete

    func testDeleteRemovesExactlyTheChosenRecordingAndNothingElse() throws {
        try leaveRecording("voice-a", frames: 48000)
        try leaveRecording("voice-b")
        let listing = try refreshListing()
        let chosen = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-a" })

        let deleted = try deleteResult(chosen).get()

        XCTAssertEqual(deleted.captureID, "voice-a")
        XCTAssertEqual(try XCTUnwrap(deleted.durationSeconds), 3.0, accuracy: 0.001)
        XCTAssertEqual(deleted.fileSizeBytes, chosen.fileSizeBytes)
        XCTAssertFalse(exists(recordingFile("voice-a")))
        XCTAssertTrue(exists(recordingFile("voice-b")))
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertTrue(VoiceRecoveryMessages.message(for: deleted).hasPrefix("Deleted the 0:03 recording"))
        XCTAssertEqual(failure(try deleteResult(chosen)), .recordingNotFound)
    }

    func testDeleteCanRemoveAnUnreadableRecordingTheUserChoosesToDiscard() throws {
        try Data([0x00, 0x01, 0x02]).write(to: recordingFile("voice-garbled"))
        let recording = try XCTUnwrap(try refreshListing().recordings.first)

        let deleted = try deleteResult(recording).get()

        XCTAssertNil(deleted.durationSeconds)
        XCTAssertFalse(exists(recordingFile("voice-garbled")))
        XCTAssertTrue(VoiceRecoveryMessages.message(for: deleted).contains("unreadable"))
    }

    func testDeleteRefusesAFileThatAStagedCaptureOwns() throws {
        failingFileSystem.failing = [.move]
        _ = try startRecording()
        controller.stop()
        guard case .keptForRetry(_, .importUnconfirmed) = try lastOutcome() else { return XCTFail("expected keptForRetry") }
        XCTAssertTrue(exists(recordURL("voice-test-1")))
        XCTAssertTrue(exists(recordingFile("voice-test-1")), "the audio could not move, so the staged record still owns it")
        restart()

        XCTAssertEqual(failure(try deleteResult(staleRecording("voice-test-1"))), .ownedByStagedCapture)
        XCTAssertEqual(failure(try continueResult(staleRecording("voice-test-1"))), .ownedByStagedCapture)

        XCTAssertTrue(exists(recordingFile("voice-test-1")))
        XCTAssertEqual(engine.startCount, 1)
    }

    func testDeleteReportsAFailureToRemoveRatherThanClaimingSuccess() throws {
        try leaveRecording("voice-left-1")
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        var environment = testEnvironment
        environment.removeFile = { _ in }
        coordinator = VoiceRecoveryCoordinator(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio), ingress: recovery, handoff: handoff,
            controller: controller, environment: environment)

        XCTAssertEqual(failure(try deleteResult(recording)), .removalFailed)

        XCTAssertTrue(exists(recordingFile("voice-left-1")))
    }

    // MARK: Continue

    func testContinueAddsAudioWithinTheRemainingBoundAndSavesTheJoinedRecording() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000

        let started = try continueResult(recording).get()

        XCTAssertEqual(started.captureID, "voice-left-1")
        XCTAssertEqual(try XCTUnwrap(engine.lastMaxDuration), 300 - 0.5, accuracy: 0.001, "the bound covers the whole recording")
        guard case .recording(let activeID, _) = controller.state else { return XCTFail("expected recording") }
        XCTAssertEqual(activeID, "voice-left-1")
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-1-continued.wav")))
        XCTAssertTrue(try refreshListing().recordings.isEmpty, "a recording being extended is not offered")

        confirmNextImport("voice-left-1")
        controller.stop()

        guard case .saved(let summary, _) = try lastOutcome() else { return XCTFail("expected saved, got \(String(describing: outcomes.last))") }
        XCTAssertEqual(summary.durationSeconds, 0.75, accuracy: 0.001)
        XCTAssertEqual(summary.end, .stoppedByUser)
        XCTAssertTrue(summary.isComplete)
        XCTAssertEqual(try audioFrames(finalizedRecording("voice-left-1")), 12000)
        XCTAssertFalse(exists(recordingFile("voice-left-1")))
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-continued.wav")))
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-joined.wav")))
        XCTAssertTrue(protectedURLs.contains(recordingFile("voice-left-1")), "the joined recording is protected again")
        XCTAssertEqual(importer.importedRecords.map(\.captureID), ["voice-left-1"])
    }

    func testCancellingAContinuationKeepsTheJoinedAudioUnsubmitted() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()

        controller.cancel()

        guard case .retainedUnsubmitted(let summary) = try lastOutcome() else { return XCTFail("expected a retained recording") }
        XCTAssertEqual(summary.durationSeconds, 0.75, accuracy: 0.001)
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 12000)
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-continued.wav")))
        let listing = try refreshListing()
        XCTAssertEqual(listing.recordings.map(\.captureID), ["voice-left-1"])
        XCTAssertEqual(try XCTUnwrap(listing.recordings.first?.durationSeconds), 0.75, accuracy: 0.001)
    }

    func testAnInterruptedContinuationIsJoinedAndHandedOffAsIncomplete() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()

        postInterruption(.began)

        guard case .keptForRetry(let summary, .importUnconfirmed) = try lastOutcome() else {
            return XCTFail("expected keptForRetry, got \(String(describing: outcomes.last))")
        }
        XCTAssertEqual(summary.end, .audioInterrupted)
        XCTAssertFalse(summary.isComplete)
        XCTAssertEqual(summary.durationSeconds, 0.75, accuracy: 0.001)
        XCTAssertEqual(try audioFrames(finalizedRecording("voice-left-1")), 12000)
        XCTAssertTrue(exists(recordURL("voice-left-1")))
    }

    func testAContinuationThatFailsToJoinLeavesBothRecordingsAndSubmitsNothing() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()
        joinShouldFail = true

        controller.stop()

        let outcome = try lastOutcome()
        XCTAssertEqual(outcome, .continuationNotJoined(
            captureID: "voice-left-1", end: .stoppedByUser, reason: .joinFailed, segmentRetained: true))
        XCTAssertEqual(controller.state, .idle)
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000, "the earlier recording is unchanged")
        XCTAssertEqual(try audioFrames(inProgressAudioURL("voice-left-1-continued.wav")), 4000)
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-joined.wav")))
        XCTAssertTrue(importer.importedRecords.isEmpty)
        let message = VoiceRecoveryMessages.message(for: outcome)
        XCTAssertTrue(message.contains("earlier recording is unchanged"))
        XCTAssertEqual(try refreshListing().recordings.count, 2, "both stay recoverable")
    }

    func testAContinuationWhoseReplacementFailsRemovesOnlyTheUnverifiedCopy() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()
        replaceShouldFail = true

        controller.stop()

        guard case .continuationNotJoined(_, _, .joinFailed, true) = try lastOutcome() else { return XCTFail("expected a failed join") }
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertEqual(try audioFrames(inProgressAudioURL("voice-left-1-continued.wav")), 4000)
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-joined.wav")))
    }

    func testAContinuationWithNoReadableAudioLeavesTheEarlierRecordingAlone() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.closeBehavior = .truncateToEmpty
        _ = try continueResult(recording).get()

        controller.stop()

        XCTAssertEqual(try lastOutcome(), .continuationNotJoined(
            captureID: "voice-left-1", end: .stoppedByUser, reason: .noReadableAudio, segmentRetained: false))
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-continued.wav")), "an empty file held no audio")
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    func testAContinuationWithUnreadableBytesKeepsThemAsTheirOwnRecording() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.closeBehavior = .corruptFile
        _ = try continueResult(recording).get()

        controller.stop()

        XCTAssertEqual(try lastOutcome(), .continuationNotJoined(
            captureID: "voice-left-1", end: .stoppedByUser, reason: .noReadableAudio, segmentRetained: true))
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-1-continued.wav")))
    }

    func testAContinuationStopsAtTheWholeRecordingsDurationBound() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()

        now = now.addingTimeInterval(299.4)
        poller.tick()
        guard case .recording = controller.state else { return XCTFail("the bound was not reached yet") }

        confirmNextImport("voice-left-1")
        now = now.addingTimeInterval(0.2)
        poller.tick()

        guard case .saved(let summary, _) = try lastOutcome() else { return XCTFail("expected saved") }
        XCTAssertEqual(summary.end, .durationLimit)
        XCTAssertFalse(summary.isComplete)
    }

    func testContinueIsNotOfferedOrAllowedWhenNoRoomIsLeft() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        var shortLimits = VoiceRecordingLimits()
        shortLimits.maxDurationSeconds = 1.0
        restart(limits: shortLimits)

        let listing = try refreshListing()
        let recording = try XCTUnwrap(listing.recordings.first)
        XCTAssertFalse(recording.canContinue)
        XCTAssertEqual(VoiceRecoveryMessages.rows(for: listing).first?.actions, [.finish, .delete])
        var forced = recording
        forced.canContinue = true
        XCTAssertEqual(failure(try continueResult(forced)), .recorderRefused(.nothingLeftToRecord))
        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertEqual(failure(try deleteResult(recording)), nil, "a refused continue releases the recording for other choices")

        try leaveRecording("voice-left-2", frames: 8000)
        var smallLimits = VoiceRecordingLimits()
        smallLimits.maxFileBytes = 40_000
        restart(limits: smallLimits)
        XCTAssertEqual(try refreshListing().recordings.first?.canContinue, false, "the size bound applies too")
    }

    func testContinueRefusesARecordingItCannotRead() throws {
        try Data([0x00, 0x01, 0x02]).write(to: recordingFile("voice-garbled"))
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        var forced = recording
        forced.canContinue = true

        XCTAssertEqual(failure(try continueResult(forced)), .recorderRefused(.recordingNotContinuable))

        XCTAssertEqual(engine.startCount, 0)
        XCTAssertEqual(try Data(contentsOf: recordingFile("voice-garbled")), Data([0x00, 0x01, 0x02]))
    }

    func testContinueIsRefusedInTheBackgroundWithoutTouchingTheRecording() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        isForeground = false

        XCTAssertEqual(failure(try continueResult(recording)), .recorderRefused(.notInForeground))

        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertEqual(engine.startCount, 0)
    }

    // MARK: Continuation leftovers

    func testAfterAFailedJoinContinueIsNotOfferedSaysWhyAndWorksOnceTheAddedAudioIsHandled() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        engine.framesWritten = 4000
        _ = try continueResult(recording).get()
        joinShouldFail = true
        controller.stop()
        joinShouldFail = false

        restart()
        let listing = try refreshListing()

        let base = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-left-1" })
        let segment = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-left-1-continued" })
        XCTAssertFalse(base.canContinue, "continue is not offered while the earlier added audio is waiting")
        XCTAssertTrue(base.hasUnjoinedAddedAudio)
        XCTAssertTrue(segment.isUnjoinedAddedAudio)
        let rows = VoiceRecoveryMessages.rows(for: listing)
        let baseRow = try XCTUnwrap(rows.first { $0.fileName == base.fileName })
        XCTAssertEqual(baseRow.actions, [.finish, .delete])
        XCTAssertTrue(baseRow.detail.contains("separate recording"), "the row says why more cannot be added")
        XCTAssertEqual(try XCTUnwrap(rows.first { $0.fileName == segment.fileName }).title, "Added audio kept separately")

        var forced = base
        forced.canContinue = true
        let refusal = failure(try continueResult(forced))
        XCTAssertEqual(refusal, .recorderRefused(.continuationLeftoverPresent))
        let message = VoiceRecoveryMessages.message(for: try XCTUnwrap(refusal))
        XCTAssertFalse(message.contains("storage is unavailable"), "storage is fine; the leftover is the cause")
        XCTAssertTrue(message.contains("separate recording"))
        XCTAssertEqual(engine.startCount, 1, "the refused continue never started the recorder")

        _ = try deleteResult(segment).get()
        let afterDeleting = try XCTUnwrap(try refreshListing().recordings.first)
        XCTAssertTrue(afterDeleting.canContinue)
        _ = try continueResult(afterDeleting).get()
        controller.cancel()
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 12000)
    }

    func testAJoinedCopyLeftByAProcessDeathIsNeverListedAndCannotBeSubmittedTwice() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        try leaveRecording("voice-left-1-continued", frames: 4000, seed: 7)
        try leaveRecording("voice-left-1-joined", frames: 2000)
        restart()

        let listing = try refreshListing()

        XCTAssertEqual(Set(listing.recordings.map { $0.fileName }), ["voice-left-1.wav", "voice-left-1-continued.wav"])
        XCTAssertFalse(exists(inProgressAudioURL("voice-left-1-joined.wav")), "the copy was made from files that still exist")
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 8000)
        XCTAssertEqual(try audioFrames(inProgressAudioURL("voice-left-1-continued.wav")), 4000)
        confirmNextImport("voice-left-1")
        let base = try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-left-1" })
        _ = try finishResult(base).get()
        XCTAssertEqual(importer.importedRecords.count, 1)
        XCTAssertFalse(importer.importedRecords.contains { $0.captureID.contains("joined") })
    }

    func testAJoinedFileWithoutBothSourcesIsKeptAndListedBecauseItMayBeTheOnlyCopy() throws {
        try leaveRecording("voice-left-1-joined", frames: 12000)
        try leaveRecording("voice-left-2", frames: 8000)
        try leaveRecording("voice-left-2-joined", frames: 12000)
        restart()

        let names = Set(try refreshListing().recordings.map { $0.fileName })

        XCTAssertEqual(names, ["voice-left-1-joined.wav", "voice-left-2.wav", "voice-left-2-joined.wav"])
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-1-joined.wav")))
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-2-joined.wav")))
    }

    func testASegmentIdenticalToTheRecordingTailIsKeptBecauseTheJoinMayHaveFailed() throws {
        try leaveRecording("voice-left-1", frames: 12000)
        try leaveRecording("voice-left-1-continued", frames: 4000)
        restart()

        let listing = try refreshListing()

        XCTAssertEqual(Set(listing.recordings.map { $0.fileName }), ["voice-left-1.wav", "voice-left-1-continued.wav"])
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-1-continued.wav")), "the only copy of added audio is never deleted")
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 12000)
        XCTAssertEqual(try audioFrames(inProgressAudioURL("voice-left-1-continued.wav")), 4000)
        XCTAssertFalse(try XCTUnwrap(listing.recordings.first { $0.captureID == "voice-left-1" }).canContinue)
    }

    func testASegmentThatIsNotTheTailOfTheRecordingIsKeptAsItsOwnRecording() throws {
        try leaveRecording("voice-left-1", frames: 8050)
        try leaveRecording("voice-left-1-continued", frames: 4000, seed: 7)
        restart()

        let listing = try refreshListing()

        XCTAssertEqual(Set(listing.recordings.map { $0.fileName }), ["voice-left-1.wav", "voice-left-1-continued.wav"])
        XCTAssertTrue(exists(inProgressAudioURL("voice-left-1-continued.wav")), "nothing is discarded on a guess")
    }

    func testFinishChecksOwnershipLikeContinueAndDelete() throws {
        failingFileSystem.failing = [.move]
        _ = try startRecording()
        controller.stop()
        guard case .keptForRetry(_, .importUnconfirmed) = try lastOutcome() else { return XCTFail("expected keptForRetry") }
        restart()

        XCTAssertEqual(failure(try finishResult(staleRecording("voice-test-1"))), .ownedByStagedCapture)

        XCTAssertTrue(exists(recordingFile("voice-test-1")))
    }

    func testFinishRefusesWhenIngressOwnershipIsUnknown() throws {
        _ = try startRecording()
        controller.stop()
        try leaveRecording("voice-left-1")
        restart()
        let recording = try XCTUnwrap(try refreshListing().recordings.first { $0.captureID == "voice-left-1" })
        failingFileSystem.failing = [.read]
        let importsBefore = importer.importedRecords.count

        XCTAssertEqual(failure(try finishResult(recording)), .ingressStateUnknown)

        XCTAssertTrue(exists(recordingFile("voice-left-1")))
        XCTAssertEqual(importer.importedRecords.count, importsBefore, "nothing was submitted")
    }

    // MARK: Messages

    func testMessagesStateOnlyVerifiedFacts() throws {
        XCTAssertEqual(VoiceRecoveryMessages.duration(0), "0:00")
        XCTAssertEqual(VoiceRecoveryMessages.duration(75.4), "1:15")
        XCTAssertEqual(VoiceRecoveryMessages.size(999), "999 B")
        XCTAssertEqual(VoiceRecoveryMessages.size(1_500), "2 KB")
        XCTAssertEqual(VoiceRecoveryMessages.size(12_345_678), "12.3 MB")

        let summary = VoiceRecordingSummary(
            captureID: "voice-x", inProgressFileName: "voice-x.wav", startedAt: now, durationSeconds: 12,
            fileSizeBytes: 384_000, end: .audioInterrupted, closedCleanly: true, protectionApplied: true)
        let acknowledgment = VoiceSaveAcknowledgment(itemID: "item-1", savedAt: "2026-10-08T09:30:02Z", alreadyImported: false)

        let saved = VoiceRecoveryMessages.message(for: .saved(summary, acknowledgment))
        XCTAssertTrue(saved.contains("ended early because the recording was interrupted"))
        XCTAssertTrue(saved.contains("0:12"))
        var complete = summary
        complete.end = .stoppedByUser
        XCTAssertEqual(VoiceRecoveryMessages.message(for: .saved(complete, acknowledgment)), "Saved. 0:12 recorded.")

        let kept = VoiceRecoveryMessages.message(for: .keptForRetry(complete, .importUnconfirmed))
        XCTAssertFalse(kept.hasPrefix("Saved"))
        XCTAssertTrue(kept.contains("Not saved yet"))
        let cancelled = VoiceRecoveryMessages.message(for: .retainedUnsubmitted(summary))
        XCTAssertTrue(cancelled.hasPrefix("Cancelled"))
        XCTAssertTrue(cancelled.contains("is not saved"))
        XCTAssertEqual(
            VoiceRecoveryMessages.message(for: .unrecoverable(captureID: "voice-x", end: .recorderFailed, fileRetained: false)),
            "Nothing was recorded.")
        XCTAssertTrue(
            VoiceRecoveryMessages.message(for: .unrecoverable(captureID: "voice-x", end: .recorderFailed, fileRetained: true))
                .contains("kept on this device but cannot be saved"))

        let failures: [VoiceRecoveryFailure] = [
            .recordingNotFound, .alreadyInProgress, .recordingActive, .ingressStateUnknown, .ownedByStagedCapture,
            .notReadable, .removalFailed, .recorderRefused(.microphoneDenied), .recorderRefused(.nothingLeftToRecord),
            .recorderRefused(.continuationLeftoverPresent),
        ]
        for failure in failures {
            XCTAssertFalse(VoiceRecoveryMessages.message(for: failure).isEmpty)
        }
    }

    func testAnActualCancellationMessageDoesNotClaimItWasSaved() throws {
        _ = try startRecording()
        controller.cancel()

        let message = VoiceRecoveryMessages.message(for: try lastOutcome())

        XCTAssertTrue(message.contains("is kept on this device and is not saved"))
        XCTAssertFalse(message.contains("Saved"))
    }
}
