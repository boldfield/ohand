import Foundation

/// The short messages the home shows under the composer. Each outcome is still reported honestly, in a line instead of
/// a status paragraph. A confirmed save is also shown as the Saved pill, but the pill never stands in for the
/// transcription and reminder state: a saved capture always says what is and is not confirmed.
enum CaptureMessages {
    static func message(
        for acknowledgment: CaptureAcknowledgment,
        locale: Locale = .autoupdatingCurrent,
        timeZone: TimeZone = .autoupdatingCurrent
    ) -> String? {
        let outcome: String?
        switch acknowledgment.save {
        case .saving:
            return "Saving\u{2026}"
        case .saved:
            outcome = savedSentences(for: acknowledgment, locale: locale, timeZone: timeZone)
        case .keptOnDevice:
            outcome = "Kept on this phone. Not confirmed saved yet. No reminder confirmed."
        case .notSaved:
            outcome = acknowledgment.source == .audio
                ? "Not saved. The recording is kept on this phone."
                : "Not saved. Your text is still here."
        case .itemDeleted:
            outcome = "That capture was deleted, so it was not saved again."
        case .cancelled:
            outcome = "Recording discarded from here. The audio is kept on this phone."
        case .unreadable(let retained):
            outcome = retained
                ? "Couldn't read the recording. What was captured is kept."
                : "Nothing was recorded."
        }
        let sentences = [acknowledgment.earlyEnd.flatMap(earlyEndNote), outcome].compactMap { $0 }
        return sentences.isEmpty ? nil : sentences.joined(separator: " ")
    }

    private static func savedSentences(for acknowledgment: CaptureAcknowledgment, locale: Locale, timeZone: TimeZone) -> String {
        [progressSentence(for: acknowledgment), reminderSentence(for: acknowledgment, locale: locale, timeZone: timeZone)]
            .compactMap { $0 }.joined(separator: " ")
    }

    private static func progressSentence(for acknowledgment: CaptureAcknowledgment) -> String? {
        guard let progress = acknowledgment.progress else {
            return acknowledgment.source == .audio ? "Not transcribed yet." : "Not processed yet."
        }
        switch progress.transcription {
        case .pending: return "Not transcribed yet."
        case .unavailable: return "Can't be transcribed on this phone."
        case .failed: return "Transcription failed; the audio is kept."
        case .transcribed: return progress.interpretation == .pending ? "Transcribed, not processed yet." : nil
        case .notApplicable: return progress.interpretation == .pending ? "Not processed yet." : nil
        }
    }

    private static func reminderSentence(for acknowledgment: CaptureAcknowledgment, locale: Locale, timeZone: TimeZone) -> String {
        switch acknowledgment.progress?.reminder ?? .notConfirmed {
        case .notConfirmed: return "No reminder confirmed."
        case .notRequested: return "No reminder set."
        case .notScheduled(let block): return "Reminder not scheduled: \(blockReason(block))."
        case .installed(let dueInstant):
            let formatter = DateFormatter()
            formatter.locale = locale
            formatter.timeZone = timeZone
            formatter.setLocalizedDateFormatFromTemplate("EEEMMMdjmm")
            return "Reminder set for \(formatter.string(from: dueInstant))."
        }
    }

    private static func blockReason(_ block: CaptureReminderBlock) -> String {
        switch block {
        case .timeInPast: return "that time has passed"
        case .permissionDenied: return "notifications are not allowed"
        case .capacityExceeded: return "too many pending reminders"
        case .unsupportedRecurrence: return "repeating reminders are not supported yet"
        case .other: return "it could not be scheduled"
        }
    }

    static let protectedDataNotice = "Not unlocked since restart. Saving may fail, but what you enter is kept."

    static let waitingForMicrophone = "Waiting for the microphone\u{2026}"
    static let finishingRecording = "Finishing\u{2026}"

    private static func earlyEndNote(_ end: VoiceRecordingEnd) -> String? {
        switch end {
        case .stoppedByUser, .cancelledByUser, .recoveredOnReentry: return nil
        case .durationLimit: return "Stopped at the time limit."
        case .sizeLimit: return "Stopped at the size limit."
        case .lowStorage: return "Stopped: storage is low."
        case .audioInterrupted: return "Stopped: audio was interrupted."
        case .leftForeground: return "Stopped when the app left the screen."
        case .deviceLocking: return "Stopped when the phone locked."
        case .recorderFailed: return "The recorder stopped unexpectedly."
        }
    }

    /// The timer text for a recording that began at `startedAt`.
    static func elapsedText(since startedAt: Date, now: Date) -> String {
        let seconds = max(0, Int(now.timeIntervalSince(startedAt)))
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }
}
