import Foundation
import OhAndCoreBridge
import OhAndServices

/// The words the retention screen uses for the core's states. Kept apart from the model so each statement is checked
/// against the case that produces it.
enum RetentionSettingsText {
    static let defaultPeriodDays: Int64 = 7

    /// States the stored period, and names 7 days as the default only as the default.
    static func policyExplanation(_ policy: RetentionPolicySetting) -> String {
        let length = periodLength(policy)
        let base = "Recordings are kept for \(length) after they are successfully turned into text, then the audio is "
            + "removed. The text stays."
        return policy.isDefault ? base : base + " The default is \(defaultPeriodDays) days."
    }

    /// Only a recording that is an item's sole source reaches this screen, and the core refuses to delete such audio on
    /// its own, so no audio-only delete is offered here. Item deletion is the explicit destructive path.
    static func offersDeleteAudio(_ entry: UnresolvedSoleAudioCase) -> Bool { false }
    static let soleSourceExplanation =
        "A recording that has no text yet is never removed automatically, whatever the period is. It stays until it has "
        + "text or you delete the whole item."
    static let deleteItemNote =
        "This recording is the only copy of the note, so it is kept until it has text or the item is deleted."
    static let pressureLowNotice =
        "This device is low on storage. Recordings that are the only copy of a note are still kept; free space by "
        + "deleting items you no longer need."
    static let lockedText = "Your saved items are locked. Go back and open Settings again to unlock them."
    static let noUnresolvedText = "No recording is waiting on text. Your captures are saved."
    static let periodChangeNote = "Changing the period moves when transcribed audio expires. It never deletes audio now."

    static func periodSummary(_ policy: RetentionPolicySetting) -> String {
        let length = periodLength(policy)
        return policy.isDefault ? "\(length) (default)" : length
    }

    private static func periodLength(_ policy: RetentionPolicySetting) -> String {
        let days = policy.periodSeconds / 86_400
        let remainder = policy.periodSeconds % 86_400
        let length: String
        if remainder == 0 {
            length = days == 1 ? "1 day" : "\(days) days"
        } else if policy.periodSeconds < 86_400 {
            length = policy.periodSeconds == 1 ? "1 second" : "\(policy.periodSeconds) seconds"
        } else {
            length = "\(days) days and \(remainder) seconds"
        }
        return length
    }

    static func reason(_ reason: AudioUnresolvedReason) -> String {
        switch reason {
        case .transcriptionPending: return "Waiting to be turned into text."
        case .transcriptionUnsupported: return "This recording cannot be turned into text on this device."
        case .transcriptionFailed: return "Turning this recording into text failed."
        case .textWithoutTranscript: return "Text exists, but not from this recording."
        case .transcriptRecordInconsistent, .transcriptEmpty, .transcriptAudioMismatch, .transcriptionTimeUnreadable:
            return "The text for this recording could not be confirmed."
        }
    }

    static func failure(_ error: Error) -> String {
        guard let error = error as? RetentionControlError else {
            return "Something went wrong. Nothing was changed."
        }
        switch error {
        case .notAuthenticated:
            return lockedText
        case .invalidRequest:
            return "That request is not valid. Nothing was changed."
        case .itemNotFound, .itemDeleted:
            return "This item is no longer available."
        case .noAudio, .audioRemoved:
            return "This recording's audio is already gone."
        case .soleSourceUseItemDeletion:
            return "This audio is the only copy of the note, so it is kept. Retry turning it into text, or delete the "
                + "whole item from its details."
        case .jobInFlight:
            return "Turning this recording into text is already queued or running."
        case .notRetryable:
            return "This recording cannot be retried right now."
        case .removalDenied:
            return "The audio was not removed because it is not allowed to be removed now. Nothing was changed."
        case .audioRemovalUnavailable:
            return "Audio removal is not ready yet. Nothing was changed; try again shortly."
        case .audioRemovalIncomplete:
            return "Removing the audio did not finish. It is not marked as done; try again."
        case .audioReferenceUnresolvable:
            return "This recording's file could not be found in the audio store, so it cannot be removed here."
        case .core, .unreadable:
            return "The app could not complete that. Nothing was changed."
        }
    }
}
