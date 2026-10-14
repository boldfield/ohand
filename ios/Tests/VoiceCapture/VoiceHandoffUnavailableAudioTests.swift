import XCTest
@testable import OhAnd
@testable import OhAndServices

/// A handoff that answers with a fixed result, optionally after changing the in-progress store, to stand in for an
/// ingress writer whose staged audio turned out to be unavailable.
private final class ScriptedVoiceHandoff: VoiceRecordingHandoffReceiving {
    var result: VoiceHandoffResult
    var beforeAnswering: ((VoiceRecordingHandoff) -> Void)?
    private(set) var handoffs: [VoiceRecordingHandoff] = []

    init(result: VoiceHandoffResult) {
        self.result = result
    }

    func handOff(_ handoff: VoiceRecordingHandoff, completion: @escaping (VoiceHandoffResult) -> Void) {
        handoffs.append(handoff)
        beforeAnswering?(handoff)
        completion(result)
    }
}

/// Deletes the in-progress recording just before the real ingress writer reads it, as a file lost between the close
/// and the handoff would be, and passes the writer's answer through.
private final class SourceRemovingVoiceHandoff: VoiceRecordingHandoffReceiving {
    private let underlying: VoiceRecordingHandoffReceiving
    private let removeSource: () -> Void
    private(set) var forwardedResult: VoiceHandoffResult?

    init(forwardingTo underlying: VoiceRecordingHandoffReceiving, removeSource: @escaping () -> Void) {
        self.underlying = underlying
        self.removeSource = removeSource
    }

    func handOff(_ handoff: VoiceRecordingHandoff, completion: @escaping (VoiceHandoffResult) -> Void) {
        removeSource()
        underlying.handOff(handoff) { [self] result in
            forwardedResult = result
            completion(result)
        }
    }
}

/// A staged recording whose audio is unavailable is unrecoverable: never kept for retry, never saved. `fileRetained`
/// is read from the in-progress store after the handoff answered.
final class VoiceHandoffUnavailableAudioTests: VoiceCaptureTestCase {
    private let captureID = "voice-test-1"
    private var scriptedOutcomes: [VoiceCaptureOutcome] = []

    private func lastScriptedOutcome() throws -> VoiceCaptureOutcome {
        try XCTUnwrap(scriptedOutcomes.last, "no outcome was delivered")
    }

    private func makeScriptedController(answering scripted: VoiceRecordingHandoffReceiving) -> VoiceRecordingController {
        let fixedCaptureID = captureID
        let scriptedController = VoiceRecordingController(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
            engine: engine,
            permission: permission,
            handoff: scripted,
            environment: testEnvironment,
            notificationCenter: notificationCenter,
            makeCaptureID: { fixedCaptureID })
        scriptedController.onOutcome = { [unowned self] in scriptedOutcomes.append($0) }
        return scriptedController
    }

    private func assertUnrecoverable(_ outcome: VoiceCaptureOutcome, fileRetained expected: Bool) {
        guard case .unrecoverable(let reportedID, let end, let fileRetained) = outcome else {
            return XCTFail("expected unrecoverable, got \(outcome)")
        }
        XCTAssertEqual(reportedID, captureID)
        XCTAssertEqual(end, .stoppedByUser)
        XCTAssertEqual(fileRetained, expected)
    }

    // MARK: Mapping

    func testTheIngressAdapterMapsAnUnavailableSourceToTheNewCaseAndNothingElseChanges() {
        let problems: [IngressStagingFailure] = [.audioSourceMissing, .audioSourceEmpty, .audioDestinationConflict]
        for problem in problems {
            XCTAssertEqual(
                IngressVoiceHandoff.result(for: .keptForRetry(captureID: captureID, problem: .sourceUnavailable(problem))),
                .stagedAudioUnavailable, "\(problem)")
        }
        let otherProblems: [IngressPendingProblem] = [
            .notCommitted, .commitUnknown, .coreUnavailable, .conflictingReuse, .rejected(code: "x"),
            .recordUnreadable(.corruptRecord),
        ]
        for problem in otherProblems {
            XCTAssertEqual(
                IngressVoiceHandoff.result(for: .keptForRetry(captureID: captureID, problem: problem)),
                .keptForRetry, "\(problem)")
        }
        XCTAssertEqual(
            IngressVoiceHandoff.result(for: .notStaged(captureID: captureID, failure: .audioSourceMissing)), .notStaged)
        XCTAssertEqual(IngressVoiceHandoff.result(for: .itemDeleted(captureID: captureID)), .itemDeleted)
    }

    // MARK: Controller

    func testUnavailableAudioWithTheInProgressFileStillPresentIsUnrecoverableWithTheFileRetained() throws {
        let scripted = ScriptedVoiceHandoff(result: .stagedAudioUnavailable)
        let scriptedController = makeScriptedController(answering: scripted)
        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        scriptedController.start { started = $0 }
        _ = try XCTUnwrap(started).get()

        scriptedController.stop()

        let outcome = try lastScriptedOutcome()
        assertUnrecoverable(outcome, fileRetained: true)
        XCTAssertEqual(scriptedOutcomes.count, 1)
        XCTAssertTrue(exists(recordingFile(captureID)))
        XCTAssertEqual(scriptedController.state, .idle)
        XCTAssertEqual(scriptedController.pendingHandoffCount, 0)

        let message = VoiceRecoveryMessages.message(for: outcome)
        XCTAssertTrue(message.contains("cannot be saved"))
        XCTAssertFalse(message.hasPrefix("Saved"))
        XCTAssertFalse(message.contains("retried"))
        XCTAssertFalse(message.contains("Not saved yet"))
    }

    func testUnavailableAudioWithTheInProgressFileGoneIsUnrecoverableWithNothingRetained() throws {
        let scripted = ScriptedVoiceHandoff(result: .stagedAudioUnavailable)
        scripted.beforeAnswering = { [unowned self] _ in
            try? FileManager.default.removeItem(at: recordingFile(captureID))
        }
        let scriptedController = makeScriptedController(answering: scripted)
        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        scriptedController.start { started = $0 }
        _ = try XCTUnwrap(started).get()

        scriptedController.stop()

        let outcome = try lastScriptedOutcome()
        assertUnrecoverable(outcome, fileRetained: false)
        XCTAssertFalse(exists(recordingFile(captureID)))
        XCTAssertEqual(VoiceRecoveryMessages.message(for: outcome), "Nothing was recorded.")
    }

    func testTheRealIngressServiceReportingAMissingSourceReachesTheControllerAsUnrecoverable() throws {
        let removing = SourceRemovingVoiceHandoff(forwardingTo: handoff) { [unowned self] in
            try? FileManager.default.removeItem(at: recordingFile(captureID))
        }
        let scriptedController = makeScriptedController(answering: removing)
        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        scriptedController.start { started = $0 }
        _ = try XCTUnwrap(started).get()

        scriptedController.stop()

        XCTAssertEqual(removing.forwardedResult, .stagedAudioUnavailable)
        assertUnrecoverable(try lastScriptedOutcome(), fileRetained: false)
        XCTAssertTrue(importer.importedRecords.isEmpty, "the core is never asked to import audio that is not there")
    }

    func testTheRealIngressServiceReportingADestinationConflictLeavesTheInProgressFileAndIsUnrecoverable() throws {
        try Data("someone else's audio".utf8).write(to: finalizedRecording(captureID))
        _ = try startRecording()

        controller.stop()

        assertUnrecoverable(try lastOutcome(), fileRetained: true)
        XCTAssertTrue(exists(recordingFile(captureID)))
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    func testTheExistingResultsKeepTheirOutcomes() throws {
        let summary = VoiceRecordingSummary(
            captureID: captureID, inProgressFileName: "\(captureID).wav", startedAt: now, durationSeconds: 3,
            fileSizeBytes: 96_000, end: .stoppedByUser, closedCleanly: true, protectionApplied: true)
        func outcome(_ result: VoiceHandoffResult) -> VoiceCaptureOutcome {
            VoiceRecordingController.outcome(for: result, summary: summary, inProgressFileExists: {
                XCTFail("only unavailable audio reads the in-progress store")
                return true
            })
        }

        XCTAssertEqual(outcome(.keptForRetry), .keptForRetry(summary, .importUnconfirmed))
        XCTAssertEqual(outcome(.notStaged), .keptForRetry(summary, .notStaged))
        XCTAssertEqual(outcome(.itemDeleted), .itemDeleted(summary))
        XCTAssertEqual(
            outcome(.saved(itemID: "item-1", savedAt: "2026-10-08T09:30:02Z", alreadyImported: false)),
            .saved(summary, VoiceSaveAcknowledgment(itemID: "item-1", savedAt: "2026-10-08T09:30:02Z", alreadyImported: false)))
    }

    // MARK: Restart recovery

    func testFinishingARecoveredRecordingWithUnavailableAudioIsUnrecoverableAndNeverKept() throws {
        try leaveRecording("voice-left-1")
        let recording = try XCTUnwrap(try refreshListing().recordings.first)
        let scripted = ScriptedVoiceHandoff(result: .stagedAudioUnavailable)
        let scriptedCoordinator = VoiceRecoveryCoordinator(
            inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
            ingress: recovery,
            handoff: scripted,
            controller: controller,
            environment: testEnvironment)

        var result: Result<VoiceCaptureOutcome, VoiceRecoveryFailure>?
        scriptedCoordinator.finish(recording) { result = $0 }
        let present = try XCTUnwrap(result).get()
        guard case .unrecoverable("voice-left-1", .recoveredOnReentry, true) = present else {
            return XCTFail("expected unrecoverable with the file retained, got \(present)")
        }

        try FileManager.default.removeItem(at: recordingFile("voice-left-1"))
        scripted.beforeAnswering = nil
        try leaveRecording("voice-left-2")
        let second = try XCTUnwrap(try refreshListing().recordings.first { $0.captureID == "voice-left-2" })
        scripted.beforeAnswering = { [unowned self] _ in
            try? FileManager.default.removeItem(at: recordingFile("voice-left-2"))
        }
        scriptedCoordinator.finish(second) { result = $0 }
        let absent = try XCTUnwrap(result).get()
        guard case .unrecoverable("voice-left-2", .recoveredOnReentry, false) = absent else {
            return XCTFail("expected unrecoverable with nothing retained, got \(absent)")
        }
        XCTAssertEqual(VoiceRecoveryMessages.message(for: absent), "Nothing was recorded.")
    }
}
