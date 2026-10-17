import Foundation

/// The short messages the home shows under the composer. Each outcome is still reported honestly, in a line instead of
/// a status paragraph. A confirmed save has no message here: it is shown as the Saved pill.
enum CaptureMessages {
    static func message(for acknowledgment: CaptureAcknowledgment) -> String? {
        let subject = acknowledgment.source == .audio ? "recording" : "text"
        let outcome: String?
        switch acknowledgment.save {
        case .saving, .saved:
            outcome = nil
        case .keptOnDevice:
            outcome = "Kept on this phone. Not confirmed saved yet."
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
        let early = acknowledgment.earlyEnd.flatMap(earlyEndNote)
        switch (outcome, early) {
        case (let outcome?, let early?): return "\(early) \(outcome)"
        case (let outcome?, nil): return outcome
        case (nil, let early?) where acknowledgment.save != .saving: return "\(early) The \(subject) is saved."
        case (nil, _): return nil
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
