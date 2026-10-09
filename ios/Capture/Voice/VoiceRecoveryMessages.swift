import Foundation

/// The wording and rows for recovered and just-ended voice recordings. Every sentence is built from facts the
/// recorder verified (duration read back from the file, bytes on disk, the way the recording ended) and says "saved"
/// only for an outcome the core confirmed.
enum VoiceRecoveryMessages {
    // MARK: Facts

    /// Minutes and seconds, rounded to the nearest second.
    static func duration(_ seconds: Double) -> String {
        let totalSeconds = max(0, Int(seconds.rounded()))
        return "\(totalSeconds / 60):" + String(format: "%02d", totalSeconds % 60)
    }

    /// Decimal units with fixed formatting, so a message never depends on the device locale.
    static func size(_ bytes: Int64) -> String {
        if bytes < 1_000 { return "\(bytes) B" }
        if bytes < 1_000_000 { return "\((bytes + 500) / 1_000) KB" }
        let tenthsOfMegabytes = (bytes + 50_000) / 100_000
        return "\(tenthsOfMegabytes / 10).\(tenthsOfMegabytes % 10) MB"
    }

    static func reason(for end: VoiceRecordingEnd) -> String? {
        switch end {
        case .stoppedByUser: return nil
        case .cancelledByUser: return "you cancelled"
        case .durationLimit: return "the time limit was reached"
        case .sizeLimit: return "the size limit was reached"
        case .lowStorage: return "the device was low on storage"
        case .audioInterrupted: return "the recording was interrupted"
        case .leftForeground: return "the app left the screen"
        case .deviceLocking: return "the device was locking"
        case .recorderFailed: return "the recorder reported an error"
        case .recoveredOnReentry: return "it was found again after the app restarted"
        }
    }

    // MARK: Rows

    static func rows(for listing: VoiceRecoveryListing) -> [VoiceRecoveryRow] {
        listing.recordings.map { recording -> VoiceRecoveryRow in
            guard let durationSeconds = recording.durationSeconds else {
                return VoiceRecoveryRow(
                    fileName: recording.fileName,
                    title: "Recording that cannot be played back",
                    detail: "\(size(recording.fileSizeBytes)) is kept on this device. It cannot be read as audio, "
                        + "so it cannot be saved. You can delete it.",
                    actions: [.delete])
            }
            var actions: [VoiceRecoveryAction] = [.finish]
            if recording.canContinue { actions.append(.continueRecording) }
            actions.append(.delete)
            var detail = "\(duration(durationSeconds)), \(size(recording.fileSizeBytes)). Kept on this device. "
                + "It is not saved until you finish it."
            if recording.isUnjoinedAddedAudio {
                detail += " This is audio added to another recording that was not joined to it."
            } else if recording.hasUnjoinedAddedAudio {
                detail += " Audio added to it earlier is kept as a separate recording, so more cannot be added until "
                    + "that one is finished or deleted."
            }
            return VoiceRecoveryRow(
                fileName: recording.fileName,
                title: recording.isUnjoinedAddedAudio ? "Added audio not joined" : "Recording not saved yet",
                detail: detail,
                actions: actions)
        }
    }

    /// Lines that keep unknowns, overflow and unconfirmed captures visible next to the rows.
    static func notices(for listing: VoiceRecoveryListing) -> [String] {
        var lines: [String] = []
        if listing.recordingsUnknown {
            lines.append("Recordings left on this device could not be checked, so none are listed. Nothing was deleted.")
        }
        if listing.notShownCount > 0 {
            let more = plural(listing.notShownCount, "recording")
            lines.append("Kept on this device and not shown yet: \(more) more.")
        }
        if !listing.pendingCaptures.isEmpty {
            let captures = plural(listing.pendingCaptures.count, "capture")
            lines.append("Kept on this device and not confirmed saved: \(captures). They will be retried.")
        }
        if !listing.confirmedCaptureIDs.isEmpty {
            let captures = plural(listing.confirmedCaptureIDs.count, "capture")
            lines.append("Confirmed saved while waiting: \(captures).")
        }
        return lines
    }

    // MARK: Outcomes and failures

    static func message(for outcome: VoiceCaptureOutcome) -> String {
        switch outcome {
        case .saved(let summary, _):
            if summary.isComplete { return "Saved. \(duration(summary.durationSeconds)) recorded." }
            let ended = "Saved, but the recording ended early" + because(summary.end)
            return ended + " \(duration(summary.durationSeconds)) was kept."
        case .keptForRetry(let summary, let kept):
            var text = "Recorded \(duration(summary.durationSeconds))"
            text += summary.isComplete ? "." : ", ended early" + because(summary.end)
            text += " Not saved yet."
            text += kept == .importUnconfirmed
                ? " It is kept on this device and will be retried."
                : " It is kept on this device and could not be prepared to save yet."
            return text
        case .retainedUnsubmitted(let summary):
            return "Cancelled. The \(duration(summary.durationSeconds)) recording is kept on this device and is not saved. "
                + "You can finish it or delete it later."
        case .itemDeleted:
            return "The item for this recording was deleted. Nothing more was saved."
        case .continuationNotJoined(_, let end, let reason, let segmentRetained):
            var text = reason == .noReadableAudio
                ? "The added audio could not be read back."
                : "The added audio could not be joined."
            text += " Your earlier recording is unchanged."
            if segmentRetained { text += " The added audio is kept as a separate recording." }
            if end.reachedLimit { text += " A limit was reached." }
            return text
        case .unrecoverable(_, _, let fileRetained):
            return fileRetained
                ? "The recording could not be read back. What was written is kept on this device but cannot be saved."
                : "Nothing was recorded."
        }
    }

    static func message(for failure: VoiceRecoveryFailure) -> String {
        switch failure {
        case .recordingNotFound: return "That recording is no longer on this device."
        case .alreadyInProgress: return "That recording is already being handled."
        case .recordingActive: return "That recording is being recorded right now."
        case .ingressStateUnknown: return "Saved captures could not be checked, so this recording was left alone."
        case .ownedByStagedCapture: return "This recording is already waiting to be saved, so it was left alone."
        case .notReadable: return "This recording cannot be read as audio, so it cannot be saved. It was kept."
        case .removalFailed: return "The recording could not be deleted and is still on this device."
        case .recorderRefused(let start): return message(for: start)
        }
    }

    static func message(for failure: VoiceStartFailure) -> String {
        switch failure {
        case .alreadyActive: return "A recording is already in progress."
        case .notInForeground: return "Recording starts only while the app is open."
        case .microphoneDenied: return "Microphone access is off. Nothing was recorded."
        case .storageUnavailable: return "Recording storage is unavailable. Nothing was recorded."
        case .storageSpaceUnknown: return "Free space could not be checked. Nothing was recorded."
        case .insufficientStorage: return "Not enough free space to record. Nothing was recorded."
        case .recorderUnavailable: return "The recorder could not start. Nothing was recorded."
        case .recordingNotContinuable: return "That recording cannot be added to. It was left as it is."
        case .continuationLeftoverPresent:
            return "Audio added to that recording earlier is still kept as a separate recording. "
                + "Finish or delete it first. Nothing was recorded."
        case .nothingLeftToRecord: return "That recording is already as long as a recording can be. It was left as it is."
        }
    }

    static func message(for deleted: VoiceDeletedRecording) -> String {
        if let durationSeconds = deleted.durationSeconds {
            return "Deleted the \(duration(durationSeconds)) recording (\(size(deleted.fileSizeBytes)))."
        }
        return "Deleted the unreadable recording (\(size(deleted.fileSizeBytes)))."
    }

    private static func plural(_ count: Int, _ noun: String) -> String {
        count == 1 ? "1 \(noun)" : "\(count) \(noun)s"
    }

    private static func because(_ end: VoiceRecordingEnd) -> String {
        reason(for: end).map { " because \($0)." } ?? "."
    }
}
