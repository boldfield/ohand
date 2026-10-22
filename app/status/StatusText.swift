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

    static let notificationsUncheckedHeadline = "Reminder scheduling has not been checked yet"
    static let notificationsUncheckedDetail =
        "Notifications are allowed, but no reminder pass has finished since this version opened. Reminders are saved."
    static let notificationsPassFailedHeadline = "Reminder scheduling did not finish"
    static let notificationsPassFailedDetail =
        "Reminders are saved, but the last attempt to schedule them failed. It is tried again when Oh And next opens."
    static let notificationsUnscheduledHeadline = "Some reminders are not scheduled"
    static let notificationsInstallingHeadline = "Reminders are being scheduled"
    static let notificationsScheduledHeadline = "Reminders are scheduled"
    static let notificationsNoneHeadline = "No reminders are waiting to be scheduled"
    static let notificationsNoneDetail = "Notifications are allowed. Nothing is held back."

    static func notificationsUnscheduledDetail(count: Int) -> String {
        "\(count) saved \(count == 1 ? "reminder is" : "reminders are") not scheduled, so \(count == 1 ? "it" : "they") will not alert as things stand. The reminder itself is kept."
    }

    static func notificationsInstallingDetail(count: Int) -> String {
        "\(count) saved \(count == 1 ? "reminder is" : "reminders are") waiting for the system to confirm scheduling."
    }

    static func notificationsScheduledDetail(count: Int) -> String {
        "\(count) saved \(count == 1 ? "reminder is" : "reminders are") scheduled with the system. That is a schedule, not a promise that an alert is shown."
    }

    static func unscheduledReason(_ reason: ReminderUnscheduledReason) -> String {
        switch reason {
        case .capacityExceeded: return "The system holds only so many; the rest wait until there is room."
        case .timeInPast: return "Its time has already passed."
        case .permissionDenied: return "Notifications are off."
        case .installFailed: return "The system did not accept it; it is tried again."
        case .unrecognized: return "Its schedule could not be read."
        }
    }
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

    static let retryRequestedNote =
        "Asked the waiting work to run again from the saved original. Check again in a moment to see where it stands."
    static let providerSettingsLabel = "Open Provider settings"
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
