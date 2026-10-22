import Foundation
import OhAndServices

/// The words the status screen uses. Kept apart from the model so each statement is checked against the case that
/// produces it.
enum StatusText {
    static let captureHeadline = "Capture is ready"
    static let captureDetail =
        "A new capture is saved on this device the moment you make it, whatever the states below say. Nothing here has to "
        + "be cleared first."

    static let syncHeadline = "Sync is not configured"
    static let syncDetail =
        "This version keeps your items on this device only. Nothing is synced to another device or account."

    static let remoteHeadline = "Interpretation may use a provider"
    static let remoteDetail =
        "Saving and finding your own words happens on this device. Sorting and interpreting a note runs on a provider "
        + "only when you have chosen one and allowed it; that work is not on-device."

    static let processingIdleHeadline = "Nothing is waiting"
    static let processingIdleDetail = "Every capture has been handled."
    static let processingCheckingHeadline = "Checking processing status"
    static let processingCheckingDetail = "Nothing has been read yet. Your captures are saved either way."
    static let processingUnknownHeadline = "Processing status could not be read"
    static let processingUnknownDetail =
        "This is not a report that anything is wrong. Your captures are saved either way. Check again in a moment."
    static let processingStalledHeadline = "Processing is behind"
    static let processingStalledDetail =
        "Some work is overdue and nothing picked it up. Your captures and their original words are untouched. Trying "
        + "again resumes the same work from the saved source."
    static let processingAttentionHeadline = "Some work needs you"
    static let processingAttentionUserDetail =
        "A destination needs to be chosen or signed in again, in Provider settings. Your captures are saved and wait "
        + "there."
    static let processingAttentionTerminalDetail =
        "Some work has ended and will not run again by itself. The captures behind it, and their original words, are "
        + "untouched."
    static let processingWaitingHeadline = "Work is waiting"
    static let processingWaitingDetail =
        "Captures are saved. This work continues by itself when the network, provider or capability it needs is back."
    static let processingWorkingHeadline = "Work is in progress"

    static let notificationsOnHeadline = "Reminders can be delivered"
    static let notificationsOnDetail = "Notifications are allowed, so saved reminders will appear when they are due."
    static let notificationsNotAskedHeadline = "Notifications have not been asked for yet"
    static let notificationsNotAskedDetail =
        "Reminders are saved. Oh And asks the first time one needs to be shown."
    static let notificationsOffHeadline = "Reminders will not be shown"
    static let notificationsOffDetail =
        "Notifications are off, so a saved reminder is not scheduled to appear. The reminder itself is kept and "
        + "appears again once notifications are on."
    static let notificationsRevokedHeadline = "Notifications were turned off"
    static let notificationsRestrictedDetail =
        "A device policy prevents notifications, and Settings cannot change that. Reminders are still saved."

    static let retryLabel = "Try again"
    static let openSettingsLabel = "Open iOS Settings"
    static let refreshLabel = "Check again"

    static let introText =
        "Plain facts about what is saved, what is waiting and what is not set up. You never need to open this to keep "
        + "using Oh And."

    static func workingDetail(pending: Int, running: Int) -> String {
        let total = pending + running
        let noun = total == 1 ? "item is" : "items are"
        return "\(total) \(noun) being worked on. Captures are saved either way."
    }

    static func waitingReason(_ kind: ProcessingErrorKind) -> String? {
        switch kind {
        case .waitingForNetwork: return "Waiting for a network connection."
        case .capabilityUnavailable: return "Waiting for a capability that is not available right now."
        case .retryScheduled: return "A retry is already scheduled."
        case .leaseExpired: return "Interrupted work will be picked up again."
        default: return nil
        }
    }
}
