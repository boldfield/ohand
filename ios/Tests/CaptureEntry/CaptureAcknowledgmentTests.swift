import XCTest
@testable import OhAnd

final class CaptureProgressMappingTests: XCTestCase {
    private func status(
        processing: String = "unprocessed",
        transcription: String = "not_applicable",
        request: String? = nil,
        schedule: String? = nil,
        reason: String? = nil,
        due: Date? = nil
    ) -> CaptureItemStatus {
        CaptureItemStatus(
            processingState: processing, transcriptionState: transcription, reminderRequestState: request,
            reminderScheduleState: schedule, unschedulableReason: reason, installedDueInstant: due)
    }

    func testAFreshTextCaptureIsUnprocessedWithNoReminderConfirmed() {
        let progress = status(request: "not_requested").progress
        XCTAssertEqual(progress.transcription, .notApplicable)
        XCTAssertEqual(progress.interpretation, .pending)
        XCTAssertEqual(progress.reminder, .notConfirmed)
    }

    func testPendingTranscriptionNeverConfirmsAReminder() {
        let progress = status(
            processing: "processed", transcription: "audio_pending", request: "resolved",
            schedule: "scheduled", due: CaptureEntryFixtures.dueInstant).progress
        XCTAssertEqual(progress.transcription, .pending)
        XCTAssertEqual(progress.reminder, .notConfirmed)
    }

    func testProcessedWithoutAReminderRequestIsNotRequested() {
        XCTAssertEqual(status(processing: "processed", request: "not_requested").progress.reminder, .notRequested)
        XCTAssertEqual(status(processing: "abstained", request: "not_requested").progress.interpretation, .abstained)
    }

    func testUnschedulableCarriesItsReason() {
        XCTAssertEqual(
            status(processing: "processed", request: "unschedulable", reason: "time_in_past").progress.reminder,
            .notScheduled(.timeInPast))
        XCTAssertEqual(
            status(processing: "processed", request: "unschedulable", reason: "permission_denied").progress.reminder,
            .notScheduled(.permissionDenied))
        XCTAssertEqual(
            status(processing: "processed", request: "unsupported_recurrence").progress.reminder,
            .notScheduled(.unsupportedRecurrence))
    }

    func testOnlyAScheduledReminderWithAResolvedTimeIsInstalled() {
        let due = CaptureEntryFixtures.dueInstant
        XCTAssertEqual(
            status(processing: "processed", request: "resolved", schedule: "scheduled", due: due).progress.reminder,
            .installed(dueInstant: due))
        XCTAssertEqual(
            status(processing: "processed", request: "resolved", schedule: "scheduled").progress.reminder,
            .notConfirmed, "a schedule state without a time is not an installed reminder")
        XCTAssertEqual(
            status(processing: "processed", request: "resolved", schedule: "pending_schedule", due: due).progress.reminder,
            .notConfirmed)
        XCTAssertEqual(
            status(processing: "processed", request: "resolved", schedule: "schedule_failed").progress.reminder,
            .notScheduled(.other))
    }

    func testUnknownStatesAreTreatedAsNotConfirmed() {
        let progress = status(processing: "mystery", transcription: "mystery", request: "mystery").progress
        XCTAssertEqual(progress.transcription, .pending)
        XCTAssertEqual(progress.interpretation, .pending)
        XCTAssertEqual(progress.reminder, .notConfirmed)
    }

    func testTranscriptionOutcomes() {
        XCTAssertEqual(status(transcription: "transcribed").progress.transcription, .transcribed)
        XCTAssertEqual(status(transcription: "transcription_unsupported").progress.transcription, .unavailable)
        XCTAssertEqual(status(transcription: "transcription_failed").progress.transcription, .failed)
    }
}

final class CaptureAcknowledgmentWordingTests: XCTestCase {
    private let wording = CaptureEntryFixtures.wording

    private func message(
        _ source: CaptureAcknowledgmentSource,
        _ save: CaptureSaveState,
        progress: CaptureProgress? = nil,
        earlyEnd: VoiceRecordingEnd? = nil
    ) -> String {
        wording.message(for: CaptureAcknowledgment(source: source, save: save, progress: progress, earlyEnd: earlyEnd))
    }

    private let savedState = CaptureSaveState.saved(itemID: "item-1", alreadySaved: false)

    func testSavedTextBeforeProcessingSaysNoReminderIsConfirmed() {
        XCTAssertEqual(
            message(.text, savedState),
            "Text saved. It has not been processed yet. No reminder has been confirmed.")
    }

    func testSavedAudioBeforeTranscriptionSaysSo() {
        XCTAssertEqual(
            message(.audio, savedState),
            "Audio saved. It has not been transcribed yet. No reminder has been confirmed.")
    }

    func testTranscribedButUnprocessedAudio() {
        let progress = CaptureProgress(transcription: .transcribed, interpretation: .pending, reminder: .notConfirmed)
        XCTAssertEqual(
            message(.audio, savedState, progress: progress),
            "Audio saved. It has been transcribed but not processed yet. No reminder has been confirmed.")
    }

    func testNotScheduledStatesWhy() {
        let progress = CaptureProgress(
            transcription: .notApplicable, interpretation: .processed, reminder: .notScheduled(.timeInPast))
        XCTAssertEqual(
            message(.text, savedState, progress: progress),
            "Text saved. Reminder not scheduled: that time has already passed.")
    }

    func testInstalledReminderStatesItsResolvedTime() {
        let progress = CaptureProgress(
            transcription: .notApplicable, interpretation: .processed,
            reminder: .installed(dueInstant: CaptureEntryFixtures.dueInstant))
        let text = message(.text, savedState, progress: progress)
        XCTAssertTrue(text.hasPrefix("Text saved. Reminder set for "), text)
        XCTAssertTrue(text.contains("Sep 21") || text.contains("Sep 22"), "the date is in the sentence: \(text)")
    }

    func testNotRequestedAfterProcessing() {
        let progress = CaptureProgress(transcription: .notApplicable, interpretation: .processed, reminder: .notRequested)
        XCTAssertEqual(message(.text, savedState, progress: progress), "Text saved. No reminder was set.")
    }

    func testKeptOnDeviceIsNotSaved() {
        XCTAssertEqual(
            message(.text, .keptOnDevice),
            "Text kept on this device. It is not confirmed saved. No reminder has been confirmed.")
    }

    func testUnsavedStatesKeepTheInputAndSayNothingAboutReminders() {
        XCTAssertEqual(message(.text, .notSaved), "Not saved. Your text is still here.")
        XCTAssertEqual(message(.audio, .notSaved), "Not saved. The recording is kept on this device.")
        XCTAssertEqual(
            message(.audio, .cancelled), "Recording cancelled. The audio is kept on this device and was not saved.")
        XCTAssertEqual(message(.audio, .unreadable(retained: false)), "Nothing was recorded.")
    }

    func testBackgroundInterruptionNamesTheEndAndStillDeniesAReminder() {
        let text = message(.audio, savedState, earlyEnd: .leftForeground)
        XCTAssertTrue(text.contains("left the screen"), text)
        XCTAssertTrue(text.contains("No reminder has been confirmed"), text)
    }

    func testEveryNonInstalledSavedOrKeptMessageDeniesAReminder() {
        let transcriptions: [CaptureTranscriptionProgress] = [.notApplicable, .pending, .transcribed, .unavailable, .failed]
        let interpretations: [CaptureInterpretationProgress] = [.pending, .processed, .abstained, .uninterpreted]
        let blocks: [CaptureReminderBlock] = [.timeInPast, .permissionDenied, .capacityExceeded, .unsupportedRecurrence, .other]
        let reminders: [CaptureReminderProgress] =
            [.notConfirmed, .notRequested] + blocks.map { .notScheduled($0) }
        let ends: [VoiceRecordingEnd?] = [
            nil, .stoppedByUser, .durationLimit, .sizeLimit, .lowStorage, .audioInterrupted, .leftForeground,
            .deviceLocking, .recorderFailed, .recoveredOnReentry,
        ]
        var checked = 0
        for source in [CaptureAcknowledgmentSource.text, .audio] {
            for end in ends {
                var variants: [CaptureAcknowledgment] = [
                    CaptureAcknowledgment(source: source, save: savedState, progress: nil, earlyEnd: end),
                    CaptureAcknowledgment(source: source, save: .keptOnDevice, progress: nil, earlyEnd: end),
                ]
                for transcription in transcriptions {
                    for interpretation in interpretations {
                        for reminder in reminders {
                            variants.append(CaptureAcknowledgment(
                                source: source, save: savedState,
                                progress: CaptureProgress(
                                    transcription: transcription, interpretation: interpretation, reminder: reminder),
                                earlyEnd: end))
                        }
                    }
                }
                for variant in variants {
                    let text = wording.message(for: variant).lowercased()
                    XCTAssertTrue(
                        text.contains("no reminder") || text.contains("not scheduled"),
                        "a bare success must not imply a reminder: \(text)")
                    XCTAssertFalse(text.contains("reminder set"), text)
                    checked += 1
                }
            }
        }
        XCTAssertGreaterThan(checked, 1000)
    }

    func testNoMessageOffersSuggestionsOrPrompts() {
        let forbiddenFragments = ["would you like", "try ", "suggest", "did you know", "?"]
        for save in [savedState, .keptOnDevice, .notSaved, .itemDeleted, .cancelled, .unreadable(retained: true), .saving] {
            let text = message(.text, save).lowercased()
            for fragment in forbiddenFragments {
                XCTAssertFalse(text.contains(fragment), "\(text) contains \(fragment)")
            }
        }
    }
}
