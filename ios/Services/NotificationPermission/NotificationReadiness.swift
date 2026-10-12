import Foundation
import OhAndCoreBridge

extension ReminderFacts {
    /// Nil when the item has no reminder.
    public init?(_ report: ItemStatusReport) {
        guard let requestState = report.reminderRequestState else { return nil }
        self.init(
            itemID: report.itemID,
            requestState: requestState,
            scheduleState: report.reminderScheduleState,
            deliveryState: report.reminderDeliveryState,
            acknowledgmentState: report.reminderAcknowledgmentState,
            unschedulableReason: report.unschedulableReason
        )
    }
}

public enum ReminderUnscheduledReason: Equatable, Sendable {
    case capacityExceeded
    case timeInPast
    case permissionDenied
    /// The OS refused the install and the core withholds it until it is re-armed.
    case installFailed
    case unrecognized
}

public enum ReminderUndeliverableReason: Equatable, Sendable {
    case permissionDenied
    case permissionNotRequested
}

/// Whether a reminder can alert, as far as the permission and the core's stored state say. It is
/// never evidence that an alert was delivered or seen.
public enum ReminderOutlook: Equatable, Sendable {
    case notApplicable
    /// Wanted and installable; the core has not confirmed the install yet.
    case awaitingInstall
    case scheduled
    /// The core holds the reminder back; nothing is installed for it.
    case unscheduled(ReminderUnscheduledReason)
    /// Wanted, but the permission prevents the OS from alerting, whatever is installed.
    case undeliverable(ReminderUndeliverableReason)
}

public struct ReminderStatus: Equatable, Sendable {
    public let facts: ReminderFacts
    public let outlook: ReminderOutlook
}

public struct NotificationReadinessReport: Equatable, Sendable {
    public let permission: NotificationPermissionState
    public let reminders: [ReminderStatus]

    public var optionalPromptsAllowed: Bool { permission.canAlert }

    public var scheduledCount: Int { count { $0 == .scheduled } }
    public var awaitingInstallCount: Int { count { $0 == .awaitingInstall } }

    public var unscheduledCount: Int {
        count { if case .unscheduled = $0 { return true } else { return false } }
    }

    public var undeliverableCount: Int {
        count { if case .undeliverable = $0 { return true } else { return false } }
    }

    private func count(_ matches: (ReminderOutlook) -> Bool) -> Int {
        reminders.filter { matches($0.outlook) }.count
    }
}

/// Folds the permission and the core's stored reminder facts into a status a person can act on.
///
/// Only those two inputs are read. The device clock, Focus and the due instant are not inputs:
/// a reminder whose time has passed keeps the delivery and acknowledgment facts the core holds
/// (`unknown` until the OS reports otherwise), and a Focus that silences an alert cannot be seen
/// here and is not guessed.
public enum NotificationReadiness {
    public static func evaluate(
        permission: NotificationPermissionState,
        reminders: [ReminderFacts]
    ) -> NotificationReadinessReport {
        NotificationReadinessReport(
            permission: permission,
            reminders: reminders.map { ReminderStatus(facts: $0, outlook: outlook(permission, $0)) }
        )
    }

    static func outlook(_ permission: NotificationPermissionState, _ facts: ReminderFacts) -> ReminderOutlook {
        switch facts.requestState {
        case "resolved":
            switch permission {
            case .denied: return .undeliverable(.permissionDenied)
            case .notRequested: return .undeliverable(.permissionNotRequested)
            case .usable: break
            }
            switch facts.scheduleState {
            case "scheduled": return .scheduled
            case "schedule_failed": return .unscheduled(.installFailed)
            default: return .awaitingInstall
            }
        case "unschedulable":
            switch facts.unschedulableReason {
            case "capacity_exceeded": return .unscheduled(.capacityExceeded)
            case "time_in_past": return .unscheduled(.timeInPast)
            case "permission_denied": return .unscheduled(.permissionDenied)
            default: return .unscheduled(.unrecognized)
            }
        default:
            return .notApplicable
        }
    }
}
