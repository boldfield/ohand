import XCTest
@testable import OhAnd

final class CaptureMessagesTests: XCTestCase {
    private func message(
        _ source: CaptureAcknowledgmentSource, _ save: CaptureSaveState, earlyEnd: VoiceRecordingEnd? = nil
    ) -> String? {
        CaptureMessages.message(for: CaptureAcknowledgment(source: source, save: save, progress: nil, earlyEnd: earlyEnd))
    }

    private func savedMessage(
        _ source: CaptureAcknowledgmentSource, progress: CaptureProgress?, earlyEnd: VoiceRecordingEnd? = nil
    ) -> String? {
        CaptureMessages.message(
            for: CaptureAcknowledgment(
                source: source, save: .saved(itemID: "item-1", alreadySaved: false), progress: progress, earlyEnd: earlyEnd),
            locale: Locale(identifier: "en_US"), timeZone: TimeZone(identifier: "UTC")!)
    }

    private func progress(
        _ transcription: CaptureTranscriptionProgress, _ interpretation: CaptureInterpretationProgress,
        _ reminder: CaptureReminderProgress
    ) -> CaptureProgress {
        CaptureProgress(transcription: transcription, interpretation: interpretation, reminder: reminder)
    }

    func testASavingStateHasNoMessage() {
        XCTAssertNil(message(.text, .saving))
    }

    func testASavedCaptureWithoutAReadSaysNotProcessedAndNoReminderConfirmed() {
        XCTAssertEqual(savedMessage(.text, progress: nil), "Not processed yet. No reminder confirmed.")
        XCTAssertEqual(savedMessage(.audio, progress: nil), "Not transcribed yet. No reminder confirmed.")
    }

    func testEveryReminderStateIsStatedForASavedCapture() {
        let done = { (reminder: CaptureReminderProgress) in self.progress(.notApplicable, .processed, reminder) }
        XCTAssertEqual(savedMessage(.text, progress: done(.notConfirmed)), "No reminder confirmed.")
        XCTAssertEqual(savedMessage(.text, progress: done(.notRequested)), "No reminder set.")
        XCTAssertEqual(
            savedMessage(.text, progress: done(.notScheduled(.permissionDenied))),
            "Reminder not scheduled: notifications are not allowed.")
        XCTAssertEqual(
            savedMessage(.text, progress: done(.notScheduled(.timeInPast))),
            "Reminder not scheduled: that time has passed.")
        XCTAssertEqual(
            savedMessage(.text, progress: done(.notScheduled(.capacityExceeded))),
            "Reminder not scheduled: too many pending reminders.")
        let installed = savedMessage(.text, progress: done(.installed(dueInstant: Date(timeIntervalSince1970: 1_790_000_000))))
        XCTAssertTrue(installed?.hasPrefix("Reminder set for ") ?? false, installed ?? "nil")
        XCTAssertFalse(installed?.contains("No reminder") ?? true)
    }

    func testEveryTranscriptionStateIsStatedForASavedRecording() {
        let none = CaptureReminderProgress.notConfirmed
        XCTAssertEqual(
            savedMessage(.audio, progress: progress(.pending, .pending, none)), "Not transcribed yet. No reminder confirmed.")
        XCTAssertEqual(
            savedMessage(.audio, progress: progress(.unavailable, .pending, none)),
            "Can't be transcribed on this phone. No reminder confirmed.")
        XCTAssertEqual(
            savedMessage(.audio, progress: progress(.failed, .pending, none)),
            "Transcription failed; the audio is kept. No reminder confirmed.")
        XCTAssertEqual(
            savedMessage(.audio, progress: progress(.transcribed, .pending, none)),
            "Transcribed, not processed yet. No reminder confirmed.")
    }

    func testEveryNonSaveOutcomeIsStillReported() {
        let states: [CaptureSaveState] = [
            .keptOnDevice, .notSaved, .itemDeleted, .cancelled, .unreadable(retained: true), .unreadable(retained: false),
        ]
        for source in [CaptureAcknowledgmentSource.text, .audio] {
            for state in states {
                XCTAssertNotNil(message(source, state), "\(source) \(state) is reported")
            }
        }
    }

    func testKeptForRetryAndUnrecoverableAreStatedHonestly() {
        XCTAssertEqual(message(.audio, .notSaved), "Not saved. The recording is kept on this phone.")
        XCTAssertEqual(message(.text, .notSaved), "Not saved. Your text is still here.")
        XCTAssertEqual(
            message(.audio, .keptOnDevice), "Kept on this phone. Not confirmed saved yet. No reminder confirmed.")
        XCTAssertEqual(message(.audio, .unreadable(retained: false)), "Nothing was recorded.")
        XCTAssertEqual(
            message(.audio, .unreadable(retained: true)), "Couldn't read the recording. What was captured is kept.")
    }

    func testAnEarlyEndIsNotedAndAUserStopIsNot() {
        XCTAssertEqual(
            savedMessage(.audio, progress: nil, earlyEnd: .durationLimit),
            "Stopped at the time limit. Not transcribed yet. No reminder confirmed.")
        XCTAssertEqual(
            message(.audio, .notSaved, earlyEnd: .deviceLocking),
            "Stopped when the phone locked. Not saved. The recording is kept on this phone.")
        XCTAssertFalse(savedMessage(.audio, progress: nil, earlyEnd: .stoppedByUser)?.contains("Stopped") ?? true)
    }

    func testTheTimerCountsWholeSeconds() {
        let start = Date(timeIntervalSince1970: 1_000)
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start), "0:00")
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start.addingTimeInterval(75.9)), "1:15")
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start.addingTimeInterval(-5)), "0:00")
    }
}
