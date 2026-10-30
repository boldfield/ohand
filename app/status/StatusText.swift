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
    static let notificationsAllowedDetail = "Notifications are allowed."
    static let notificationsUnfinishedHeadline = "Some reminder scheduling did not go through"
    static let notificationsUnfinishedDetail =
        "Reminders are saved, but the system did not accept every change the last scheduling pass asked for. It is "
        + "tried again when Oh And next opens."

    /// Daily prompts the last pass did not schedule. Reminders always come first, so these never displace one.
    static func heldBackPromptsDetail(deferred: Int, withheld: Int) -> [String] {
        var details: [String] = []
        if deferred > 0 {
            details.append(
                "\(deferred) daily \(deferred == 1 ? "prompt waits" : "prompts wait") for room with the system; reminders come first.")
        }
        if withheld > 0 {
            details.append(
                "\(withheld) daily \(withheld == 1 ? "prompt is" : "prompts are") held back until notifications can alert.")
        }
        return details
    }

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

    static let unlockLabel = "Unlock to see which"
    static let showAllLabel = "Show all"
    static let failedNotesTitle = "Notes that did not sort"
    static let processingSettingsLabel = "Open Processing settings"
    static let rerunFailed = "Could not run them again. Your notes are untouched. Check again in a moment."
    static let notePlaceholder = "A saved note"
    static let sortingLabel = "Sorting…"

    static func capturedLabel(_ date: Date?) -> String {
        guard let date else { return "Saved earlier" }
        return "Saved " + date.formatted(date: .abbreviated, time: .shortened)
    }

    static func rerunSummary(queued: Int, needsSetting: Int) -> String {
        func notes(_ count: Int) -> String { "\(count) \(count == 1 ? "note" : "notes")" }
        switch (queued > 0, needsSetting > 0) {
        case (true, true):
            return "\(notes(queued)) queued again. \(needsSetting) can't run until a setting changes."
        case (true, false): return "\(notes(queued)) queued again."
        case (false, true): return "\(notes(needsSetting)) can't run until a setting changes."
        case (false, false): return "Nothing needed to run again."
        }
    }

    /// A "Try again" over several requests where a later one did not go through: what was sent, then what was not.
    static func rerunIncomplete(queued: Int, needsSetting: Int, notRun: Int) -> String {
        let sent = queued > 0 || needsSetting > 0 ? rerunSummary(queued: queued, needsSetting: needsSetting) + " " : ""
        let rest = notRun == 1 ? "1 note was not run again" : "\(notRun) notes were not run again"
        return sent + rest + ". Check again in a moment."
    }

    /// One plain line for each machine label the core can record on a failed job. Anything else is not echoed.
    static func failureReason(_ label: String) -> String {
        switch label {
        case "timeout": return "The provider took too long to answer"
        case "cancelled": return "The work was cancelled before it finished"
        case "unavailable": return "The provider could not be reached"
        case "rate_limited": return "The provider asked to slow down"
        case "unauthorized": return "The provider did not accept the sign-in"
        case "unsupported": return "No provider was allowed when it was saved"
        case "unsupported_job_version": return "It was saved by a version that cannot sort it"
        case "invalid_output": return "The provider's answer could not be used"
        case "output_too_large": return "The provider's answer was too large"
        case "input_too_large": return "The note was too long to send"
        case "capability_unavailable": return "A needed capability was not available"
        case "capability_did_not_settle": return "A needed capability did not finish in time"
        case "profile_mismatch": return "The provider settings changed while it waited"
        case "rejected": return "The request was not accepted"
        case "source_unavailable": return "The saved original could not be read"
        case "permanent": return "The provider reported a failure that will not clear"
        case "native_failure": return "The app could not send the request"
        case "offline_deferred": return "It was put off while the phone was offline"
        case "retries_exhausted": return "It was tried several times and did not go through"
        case "interrupted": return "The app stopped before it finished"
        case "internal_error": return "The app hit an unexpected error"
        case "profile_revoked": return "The provider was turned off for this note"
        case "profile_missing": return "The provider it was set for is no longer there"
        default: return "It stopped for a reason this version cannot name"
        }
    }

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
