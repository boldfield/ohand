import Foundation

enum CaptureAcknowledgmentSource: Equatable {
    case text
    case audio
}

enum CaptureSaveState: Equatable {
    case saving
    /// The core confirmed the durable import.
    case saved(itemID: String, alreadySaved: Bool)
    /// Durable on this device, import not confirmed.
    case keptOnDevice
    /// Nothing durable was written for this attempt; the input is still held.
    case notSaved
    case itemDeleted
    /// A recording the user cancelled; the audio stays on the device, unsubmitted.
    case cancelled
    /// No readable audio. `retained` says whether unreadable bytes were left for recovery.
    case unreadable(retained: Bool)
}

/// What the surface knows about one capture right now. `progress` is present only after a read of the saved item
/// succeeded; without it a saved capture is worded as not yet processed and no reminder confirmed.
struct CaptureAcknowledgment: Equatable {
    var source: CaptureAcknowledgmentSource
    var save: CaptureSaveState
    var progress: CaptureProgress?
    /// Set when a recording ended on its own (limit, interruption, leaving the screen, lock) rather than by the user.
    var earlyEnd: VoiceRecordingEnd?

    var savedItemID: String? {
        if case .saved(let itemID, _) = save { return itemID }
        return nil
    }

    var isSaving: Bool { save == .saving }
}

/// The only place acknowledgment words are chosen. Every saved capture that is not an installed reminder says so in
/// words ("no reminder has been confirmed", "not scheduled", ...), so a bare success never implies a reminder exists.
/// "Reminder set" appears only for an installed reminder, with its resolved time.
struct CaptureAcknowledgmentWording {
    var locale: Locale
    var timeZone: TimeZone

    static var standard: CaptureAcknowledgmentWording {
        CaptureAcknowledgmentWording(locale: .autoupdatingCurrent, timeZone: .autoupdatingCurrent)
    }

    func message(for acknowledgment: CaptureAcknowledgment) -> String {
        var sentences = [saveSentences(for: acknowledgment)]
        if let end = acknowledgment.earlyEnd, let note = Self.earlyEndNote(end) {
            sentences.append(note)
        }
        return sentences.joined(separator: " ")
    }

    private func saveSentences(for acknowledgment: CaptureAcknowledgment) -> String {
        let subject = acknowledgment.source == .audio ? "Audio" : "Text"
        switch acknowledgment.save {
        case .saving:
            return "Saving\u{2026}"
        case .saved:
            return ["\(subject) saved.", progressSentence(for: acknowledgment), reminderSentence(for: acknowledgment)]
                .compactMap { $0 }.joined(separator: " ")
        case .keptOnDevice:
            return "\(subject) kept on this device. It is not confirmed saved. No reminder has been confirmed."
        case .notSaved:
            return acknowledgment.source == .audio
                ? "Not saved. The recording is kept on this device."
                : "Not saved. Your text is still here."
        case .itemDeleted:
            return "That capture was deleted, so it was not saved again."
        case .cancelled:
            return "Recording cancelled. The audio is kept on this device and was not saved."
        case .unreadable(let retained):
            return retained
                ? "The recording could not be read back. What was captured is kept on this device."
                : "Nothing was recorded."
        }
    }

    private func progressSentence(for acknowledgment: CaptureAcknowledgment) -> String? {
        guard let progress = acknowledgment.progress else {
            return acknowledgment.source == .audio
                ? "It has not been transcribed yet."
                : "It has not been processed yet."
        }
        switch progress.transcription {
        case .pending:
            return "It has not been transcribed yet."
        case .unavailable:
            return "It cannot be transcribed on this device."
        case .failed:
            return "Transcription failed; the audio is kept."
        case .transcribed:
            return progress.interpretation == .pending ? "It has been transcribed but not processed yet." : nil
        case .notApplicable:
            return progress.interpretation == .pending ? "It has not been processed yet." : nil
        }
    }

    private func reminderSentence(for acknowledgment: CaptureAcknowledgment) -> String {
        switch acknowledgment.progress?.reminder ?? .notConfirmed {
        case .notConfirmed:
            return "No reminder has been confirmed."
        case .notRequested:
            return "No reminder was set."
        case .notScheduled(let block):
            return "Reminder not scheduled: \(Self.blockReason(block))."
        case .installed(let dueInstant):
            return "Reminder set for \(formatted(dueInstant))."
        }
    }

    private static func blockReason(_ block: CaptureReminderBlock) -> String {
        switch block {
        case .timeInPast: return "that time has already passed"
        case .permissionDenied: return "notifications are not allowed"
        case .capacityExceeded: return "there are too many pending reminders"
        case .unsupportedRecurrence: return "repeating reminders are not supported yet"
        case .other: return "it could not be scheduled"
        }
    }

    private static func earlyEndNote(_ end: VoiceRecordingEnd) -> String? {
        switch end {
        case .stoppedByUser, .cancelledByUser, .recoveredOnReentry: return nil
        case .durationLimit: return "Recording stopped at the time limit."
        case .sizeLimit: return "Recording stopped at the size limit."
        case .lowStorage: return "Recording stopped because storage is low."
        case .audioInterrupted: return "Recording stopped because audio was interrupted."
        case .leftForeground: return "Recording stopped when the app left the screen."
        case .deviceLocking: return "Recording stopped when the device locked."
        case .recorderFailed: return "The recorder stopped unexpectedly; what was captured so far is kept."
        }
    }

    private func formatted(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.setLocalizedDateFormatFromTemplate("EEEMMMdjmm")
        return formatter.string(from: date)
    }
}
