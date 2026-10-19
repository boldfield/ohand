import Foundation
import OhAndCoreBridge
import OhAndServices

/// What the reminder section says about the reminder as it stands. Each case is one honest state; none of them claims
/// the operating system holds, or has delivered, a notification unless the core recorded that.
enum ReminderStanding: Equatable {
    /// The action has no reminder.
    case none
    /// The user cancelled the reminder. The item stays.
    case cancelled
    /// The request asked for a repeat, which this version does not schedule. Nothing is scheduled.
    case unsupportedRepeat
    /// A request exists but no time was resolved from it. Nothing is scheduled.
    case timeNotResolved
    /// The time is stored but too many reminders are waiting for the system's limit, so this one is held back.
    case heldBackForCapacity
    /// The stored time was not in the future when it was scheduled. The user must choose a later one.
    case opportunityExpired
    /// The core held the reminder back because notifications were refused.
    case heldBackForPermission
    case unschedulable
    /// Saved, but notifications are off or not asked for, so the system cannot alert whatever is installed.
    case permissionBlocked(NotificationPermissionState)
    /// The system reported the notification installed. This is not a statement that it was or will be shown.
    case installed
    /// The system refused the install; the core keeps the time and tries again at the next pass.
    case installFailed
    /// Saved in the app. The next reminder pass installs it; nothing says it is installed yet.
    case savedAwaitingInstall
    /// The time passed and no delivery was recorded, so "it fired" is not claimed.
    case passedWithoutDeliveryEvidence
    /// The time passed and the core recorded a delivery.
    case delivered

    static func of(
        _ reminder: ReminderRecordAnswer?, permission: NotificationPermissionState?, now: Date
    ) -> ReminderStanding {
        guard let reminder else { return .none }
        switch reminder.requestState {
        case "not_requested": return .none
        case "cancelled": return .cancelled
        case "unsupported_recurrence": return .unsupportedRepeat
        case "not_scheduled_yet": return .timeNotResolved
        case "unschedulable":
            switch reminder.unschedulableReason {
            case "capacity_exceeded": return .heldBackForCapacity
            case "time_in_past": return .opportunityExpired
            case "permission_denied": return .heldBackForPermission
            default: return .unschedulable
            }
        case "resolved":
            if let due = reminder.resolvedDate, due <= now {
                return reminder.deliveryState == "unknown" ? .passedWithoutDeliveryEvidence : .delivered
            }
            if let permission, !permission.canAlert { return .permissionBlocked(permission) }
            switch reminder.scheduleState {
            case "scheduled": return .installed
            case "schedule_failed": return .installFailed
            default: return .savedAwaitingInstall
            }
        default:
            return .unschedulable
        }
    }

    var title: String {
        switch self {
        case .none: return "No reminder"
        case .cancelled: return "Reminder cancelled"
        case .unsupportedRepeat: return "Repeat not supported"
        case .timeNotResolved: return "No time set"
        case .heldBackForCapacity: return "Held back"
        case .opportunityExpired: return "Time has passed"
        case .heldBackForPermission, .permissionBlocked: return "Notifications are off"
        case .unschedulable: return "Not scheduled"
        case .installed: return "Scheduled"
        case .installFailed: return "Not installed yet"
        case .savedAwaitingInstall: return "Saved"
        case .passedWithoutDeliveryEvidence: return "Time has passed"
        case .delivered: return "Delivered"
        }
    }

    var detail: String {
        switch self {
        case .none: return "Choose a date and time to be reminded about this action."
        case .cancelled: return "No reminder will be shown. Choose a date and time to set one again."
        case .unsupportedRepeat:
            return "This request asked for a repeating reminder. Repeats are not supported yet, so nothing is scheduled. "
                + "Choose one date and time instead."
        case .timeNotResolved: return "The request did not name a time, so nothing is scheduled. Choose a date and time."
        case .heldBackForCapacity:
            return "The system limits how many reminders can wait at once, so this one is held back and nothing is "
                + "scheduled for it. It is kept and will be scheduled when room frees up."
        case .opportunityExpired:
            return "The time was already past when it came to be scheduled, so nothing was scheduled. "
                + "Choose a later date and time."
        case .heldBackForPermission:
            return "Notifications are turned off for this app, so nothing is scheduled. Turn them on in Settings."
        case .unschedulable: return "This reminder could not be scheduled. Choose a different date and time."
        case .permissionBlocked(let permission):
            return permission == .denied
                ? "The time is saved, but notifications are turned off for this app, so it cannot alert. "
                    + "Turn them on in Settings."
                : "The time is saved, but you have not allowed notifications yet, so it cannot alert."
        case .installed: return "The system has this reminder queued. Whether it appears depends on your device settings."
        case .installFailed:
            return "The time is saved. The system did not accept the reminder, and it will be tried again."
        case .savedAwaitingInstall:
            return "The time is saved in the app. It has not been confirmed as scheduled with the system yet."
        case .passedWithoutDeliveryEvidence:
            return "The time has passed. Nothing recorded whether it was shown, so it is not claimed to have been."
        case .delivered: return "The time has passed and the notification was delivered."
        }
    }

    /// Whether there is a reminder to cancel.
    var offersCancel: Bool {
        switch self {
        case .none, .cancelled: return false
        default: return true
        }
    }
}

/// What the last save or cancel reported. Words are fixed; nothing is composed from the item.
enum ReminderEditMessage: Equatable {
    /// The edit is stored. The standing says whether the system has it.
    case saved
    case cancelled
    /// The chosen time is not in the future. Nothing was stored.
    case timeInPast
    /// The reminder changed since it was shown, so it was reloaded. The choice made here was not applied.
    case changedElsewhere
    /// The item can no longer carry a reminder (completed, cancelled or deleted).
    case itemClosed
    /// The answer was not confirmed. Trying again changes at most one reminder.
    case notConfirmed
    case invalidZone

    var text: String {
        switch self {
        case .saved: return "Saved."
        case .cancelled: return "Reminder cancelled."
        case .timeInPast: return "Choose a time in the future. Nothing was changed."
        case .changedElsewhere: return "This reminder changed since you opened it. It was reloaded; choose again."
        case .itemClosed: return "This item is closed, so its reminder can no longer be changed."
        case .notConfirmed: return "Could not confirm the change. Try again; it will not create a second reminder."
        case .invalidZone: return "That time zone is not recognised. Nothing was changed."
        }
    }
}

/// The stored resolved time, separate from the draft being edited.
enum ReminderStoredTime {
    /// The stored instant written in its own zone, or nil when none is stored. A zone the system does not know is
    /// named and the time is written in UTC, so the instant is never shifted silently.
    static func text(for reminder: ReminderRecordAnswer?) -> String? {
        guard let instant = reminder?.resolvedDate else { return nil }
        let zone = reminder?.timezoneID.flatMap { TimeZone(identifier: $0) } ?? TimeZone(identifier: "UTC")!
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = zone
        formatter.dateFormat = "yyyy-MM-dd HH:mm zzz"
        return "Stored time: \(formatter.string(from: instant))"
    }
}
