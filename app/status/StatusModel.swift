import Combine
import Foundation
import OhAndServices

/// One read of background-processing health. `ProcessingHealthService` backs it in production.
protocol ProcessingHealthReading {
    func read() async -> ProcessingHealthOutcome
}

/// Asks the job queue to run now. The same saved source is processed again; nothing is re-captured or rewritten.
protocol ProcessingRetrying {
    func retryNow()
}

extension JobRunnerService: ProcessingRetrying {
    func retryNow() { requestDrain() }
}

/// The most recent reminder pass, as the app's composition root recorded it. `nil` before the first pass.
protocol ReminderSchedulingReading {
    func lastReport() -> NotificationPermissionReport?
}

/// Used when no reminder pass source exists (tests, previews): the status says scheduling has not been checked.
struct NoReminderSchedulingReport: ReminderSchedulingReading {
    func lastReport() -> NotificationPermissionReport? { nil }
}

enum StatusTone: Equatable {
    case good
    case waiting
    case needsAttention
}

enum StatusAction: Equatable {
    case none
    /// Run the queued work again from its saved source.
    case retry
    /// The permission can be switched on in iOS Settings.
    case openSettings
    /// A provider destination has to be chosen or signed in again; that happens in Provider settings.
    case openProviderSettings
}

enum StatusFactKind: String, CaseIterable {
    case capture
    case processing
    case notifications
    case sync
    case remoteInterpretation
}

struct StatusFact: Equatable, Identifiable {
    let kind: StatusFactKind
    let tone: StatusTone
    let headline: String
    let detail: String
    let action: StatusAction

    var id: StatusFactKind { kind }
}

/// The honest status screen: what is saved, what is waiting, what is unavailable and what is not scheduled.
///
/// Rules this type keeps:
/// * It never gates anything. Capture is always described as ready; no state here asks the person to triage a queue.
/// * A failed read is reported as unreadable, never as healthy.
/// * Retry only asks the existing job queue to drain again; it cannot touch a saved source.
/// * Sync is described as not configured, and remote interpretation is never described as on-device.
/// Confined to the main queue.
final class StatusModel: ObservableObject {
    @Published private(set) var facts: [StatusFact] = StatusModel.facts(health: nil, notifications: nil, scheduling: nil)
    @Published private(set) var hasLoaded = false
    /// Set once the person has asked for a retry, until a later read no longer offers one.
    @Published private(set) var retryRequested = false

    private let health: ProcessingHealthReading
    private let permissions: PermissionsReading
    private let scheduling: ReminderSchedulingReading
    private let retrier: ProcessingRetrying
    private let openSystemSettings: () -> Void

    init(
        health: ProcessingHealthReading, permissions: PermissionsReading, retrier: ProcessingRetrying,
        scheduling: ReminderSchedulingReading = NoReminderSchedulingReport(),
        openSystemSettings: @escaping () -> Void
    ) {
        self.health = health
        self.permissions = permissions
        self.scheduling = scheduling
        self.retrier = retrier
        self.openSystemSettings = openSystemSettings
    }

    /// Reads processing health and notification permission again. Reading asks for nothing.
    @MainActor func refresh() async {
        let outcome = await health.read()
        let entries = await permissions.refresh()
        facts = Self.facts(
            health: outcome, notifications: entries.first { $0.kind == .notifications }, scheduling: scheduling.lastReport())
        hasLoaded = true
        if !facts.contains(where: { $0.action == .retry }) { retryRequested = false }
    }

    /// Runs the waiting work again from its saved source, then reads the status once more. The drain is asynchronous,
    /// so the read right after may still show the same state; the screen says the request was made.
    @MainActor func retry() async {
        retryRequested = true
        retrier.retryNow()
        await refresh()
    }

    func openSettings() {
        openSystemSettings()
    }

    static func facts(
        health: ProcessingHealthOutcome?, notifications: PermissionEntry?, scheduling: NotificationPermissionReport?
    ) -> [StatusFact] {
        [
            StatusFact(
                kind: .capture, tone: .good, headline: StatusText.captureHeadline,
                detail: StatusText.captureDetail, action: .none),
            processingFact(health),
            notificationsFact(notifications, scheduling),
            StatusFact(
                kind: .sync, tone: .waiting, headline: StatusText.syncHeadline,
                detail: StatusText.syncDetail, action: .none),
            StatusFact(
                kind: .remoteInterpretation, tone: .waiting, headline: StatusText.remoteHeadline,
                detail: StatusText.remoteDetail, action: .none),
        ]
    }

    private static func processingFact(_ outcome: ProcessingHealthOutcome?) -> StatusFact {
        func fact(_ tone: StatusTone, _ headline: String, _ detail: String, _ action: StatusAction = .none) -> StatusFact {
            StatusFact(kind: .processing, tone: tone, headline: headline, detail: detail, action: action)
        }
        guard let outcome else {
            return fact(.waiting, StatusText.processingCheckingHeadline, StatusText.processingCheckingDetail)
        }
        guard case .snapshot(let snapshot) = outcome else {
            return fact(.waiting, StatusText.processingUnknownHeadline, StatusText.processingUnknownDetail)
        }
        switch snapshot.summary {
        case .idle:
            return fact(.good, StatusText.processingIdleHeadline, StatusText.processingIdleDetail)
        case .working:
            return fact(
                .good, StatusText.processingWorkingHeadline,
                StatusText.workingDetail(pending: snapshot.pendingJobs, running: snapshot.runningJobs))
        case .waiting:
            let reasons = snapshot.errors.compactMap { StatusText.waitingReason($0.kind) }
            let detail = ([StatusText.processingWaitingDetail] + Array(Set(reasons)).sorted()).joined(separator: " ")
            return fact(.waiting, StatusText.processingWaitingHeadline, detail)
        case .stalled:
            return fact(.needsAttention, StatusText.processingStalledHeadline, StatusText.processingStalledDetail, .retry)
        case .needsAttention:
            let needsUser = snapshot.errors.contains { $0.recovery == .userAction }
            let detail = needsUser ? StatusText.processingAttentionUserDetail : StatusText.processingAttentionTerminalDetail
            return fact(
                .needsAttention, StatusText.processingAttentionHeadline, detail, needsUser ? .openProviderSettings : .none)
        }
    }

    private static func notificationsFact(_ entry: PermissionEntry?, _ report: NotificationPermissionReport?) -> StatusFact {
        func fact(_ tone: StatusTone, _ headline: String, _ detail: String, _ action: StatusAction = .none) -> StatusFact {
            StatusFact(kind: .notifications, tone: tone, headline: headline, detail: detail, action: action)
        }
        guard let entry else {
            return fact(.waiting, StatusText.notificationsNotAskedHeadline, StatusText.notificationsNotAskedDetail)
        }
        switch entry.status {
        case .notRequested:
            return fact(.waiting, StatusText.notificationsNotAskedHeadline, StatusText.notificationsNotAskedDetail)
        case .denied:
            let headline = entry.change == .revoked ? StatusText.notificationsRevokedHeadline : StatusText.notificationsOffHeadline
            return fact(.needsAttention, headline, StatusText.notificationsOffDetail, .openSettings)
        case .restricted:
            return fact(
                .needsAttention, StatusText.notificationsOffHeadline, StatusText.notificationsRestrictedDetail)
        case .granted:
            return grantedNotificationsFact(report, fact)
        }
    }

    /// Allowed notifications say nothing about scheduling, so this reads the last reminder pass itself.
    private static func grantedNotificationsFact(
        _ report: NotificationPermissionReport?,
        _ fact: (StatusTone, String, String, StatusAction) -> StatusFact
    ) -> StatusFact {
        guard let report else {
            return fact(.waiting, StatusText.notificationsUncheckedHeadline, StatusText.notificationsUncheckedDetail, .none)
        }
        if report.reconciliationFailure != nil {
            return fact(
                .needsAttention, StatusText.notificationsPassFailedHeadline, StatusText.notificationsPassFailedDetail, .none)
        }
        let reminders = report.readiness.reminders
        let unscheduledReasons = reminders.compactMap { status -> ReminderUnscheduledReason? in
            if case .unscheduled(let reason) = status.outlook { return reason }
            return nil
        }
        if !unscheduledReasons.isEmpty {
            let detail = ([StatusText.notificationsUnscheduledDetail(count: unscheduledReasons.count)]
                + Set(unscheduledReasons.map(StatusText.unscheduledReason)).sorted()).joined(separator: " ")
            return fact(.needsAttention, StatusText.notificationsUnscheduledHeadline, detail, .none)
        }
        if report.awaitingInstallCount > 0 {
            return fact(
                .waiting, StatusText.notificationsInstallingHeadline,
                StatusText.notificationsInstallingDetail(count: report.awaitingInstallCount), .none)
        }
        if report.scheduledCount > 0 {
            return fact(
                .good, StatusText.notificationsScheduledHeadline,
                StatusText.notificationsScheduledDetail(count: report.scheduledCount), .none)
        }
        return fact(.good, StatusText.notificationsNoneHeadline, StatusText.notificationsNoneDetail, .none)
    }
}
