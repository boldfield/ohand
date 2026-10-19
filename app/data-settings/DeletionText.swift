import Foundation
import OhAndCoreBridge
import OhAndServices

/// The words the deletion and reset controls use. Kept apart from the models so each statement is checked against the
/// case that produces it. Nothing here names an item, a capture, a file or a secret.
enum DeletionText {
    // MARK: Item deletion

    static let itemHeading = "Delete this item"
    static let itemExplanation =
        "Deleting removes this item's text, its search entries, its reminders and its recording from this device. "
        + "It cannot be undone."
    static let itemLimitsNote =
        "Copies you exported or shared earlier, and copies in device backups, are outside this app. Deleting here cannot "
        + "recall or delete them."
    static let lockedText = "Your saved items are locked. Open this screen again to unlock them."
    static let deletingText = "Deleting…"
    static let completeText = "Deleted. Nothing of this item remains on this device."
    static let staleText = "This item changed since you opened it. Open it again to see the latest, then delete it."
    static let notFoundText = "This item is not available. It may already have been deleted."
    static let invalidText = "This item could not be deleted."
    static let unavailableText = "The deletion could not be started. Nothing was deleted. Try again later."

    static func kind(_ kind: DeletionCleanupKind) -> String {
        switch kind {
        case .removeAudio: return "the recording file"
        case .clearIngress: return "an unsent copy of the capture"
        case .cancelNotifications: return "its reminder notifications"
        }
    }

    static func list(_ kinds: [DeletionCleanupKind]) -> String {
        let names = kinds.map(kind)
        switch names.count {
        case 0: return ""
        case 1: return names[0]
        default: return names.dropLast().joined(separator: ", ") + " and " + names[names.count - 1]
        }
    }

    /// The deletion is recorded and the item is unreadable; these effects still have to finish.
    static func pending(_ pending: [DeletionCleanupKind], failed: [DeletionCleanupKind]) -> String {
        var text = "Deleted and hidden. Removing \(list(pending)) has not finished yet; it will be retried."
        if !failed.isEmpty { text += " " + failedText(failed) }
        return text
    }

    static let cleanupUnfinished =
        "Deleted and hidden. Final cleanup has not finished yet; it will be retried."

    static func failedText(_ failed: [DeletionCleanupKind]) -> String {
        "Removing \(list(failed)) failed and will not be retried automatically."
    }

    static func onlyFailed(_ failed: [DeletionCleanupKind]) -> String {
        "Deleted and hidden. " + failedText(failed)
    }

    // MARK: Reset

    static let resetHeading = "Delete all data"
    static let resetExplanation =
        "Deleting all data removes every note, recording, transcript, search entry and reminder from this device and starts "
        + "again from empty. It cannot be undone."
    static let resetLimitsNote =
        "Copies you exported or shared earlier, and copies in device backups, are outside this app. Deleting all data "
        + "cannot recall or delete them."
    static let resetCaptureNote =
        "You can keep capturing while this runs. A note captured during or after the reset is kept as new content and is "
        + "never removed by it."
    static let credentialQuestion = "Provider settings and stored credentials"
    static let keepCredentialsTitle = "Keep my provider settings and keys"
    static let removeCredentialsTitle = "Remove my provider settings and keys too"
    static let choiceRequiredNote = "Choose what happens to your provider settings and keys before you continue."
    static let resettingText = "Deleting all data…"
    static let completedText =
        "All data was deleted from this device. New captures are saved as fresh content."
    static let pendingHeadline =
        "A reset is under way but has not finished. Your data stays hidden from reads while it finishes. Try again to "
        + "continue; it also resumes when the app opens."
    static let notStartedText = "Nothing was deleted. Try again."
    static let unavailableResetText = "The reset could not be read. Try again later."

    static func blocker(_ blocker: ResetBlocker) -> String {
        switch blocker {
        case .coreUnavailable: return "The stored data is not available right now."
        case .markerNotRecorded, .markerUnreadable, .markerNotCleared: return "A record of the reset could not be updated."
        case .stagingUnreadable, .stagingPurgeIncomplete: return "Some unsaved captures have not been cleared yet."
        case .itemCleanupIncomplete: return "Some items still have recordings, files or reminders to remove."
        case .exportTemporariesRemain: return "Some export files have not been removed yet."
        case .credentialRemovalIncomplete: return "Some provider keys have not been removed yet."
        case .credentialsUnreadable: return "Your stored provider keys could not be checked, so the reset did not start."
        case .coreResetPending: return "The stored data has not finished resetting."
        case .cancelled: return "The reset was interrupted."
        }
    }

    static func progress(_ progress: ResetProgress) -> String? {
        var parts: [String] = []
        if progress.itemsAwaitingTombstone > 0 {
            parts.append("\(count(progress.itemsAwaitingTombstone)) still to delete")
        }
        if progress.cleanupPending > 0 { parts.append("\(count(progress.cleanupPending)) cleanup steps pending") }
        if progress.cleanupFailed > 0 { parts.append("\(count(progress.cleanupFailed)) cleanup steps failed") }
        return parts.isEmpty ? nil : parts.joined(separator: "; ") + "."
    }

    private static func count(_ value: Int) -> String { String(value) }

    static func pendingDetail(blockers: [ResetBlocker], progress: ResetProgress?) -> String {
        var lines = [pendingHeadline]
        var seen = Set<ResetBlocker>()
        for item in blockers where seen.insert(item).inserted { lines.append(blocker(item)) }
        if let progress, let summary = Self.progress(progress) { lines.append(summary) }
        return lines.joined(separator: " ")
    }

    static func notStarted(blockers: [ResetBlocker]) -> String {
        var seen = Set<ResetBlocker>()
        let reasons = blockers.filter { seen.insert($0).inserted }.map(blocker)
        return ([notStartedText] + reasons).joined(separator: " ")
    }
}
