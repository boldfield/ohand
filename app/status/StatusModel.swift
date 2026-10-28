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
    /// Every recovery that applies, most direct first. Empty when nothing needs doing.
    let actions: [StatusAction]

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

    /// What the failed-notes section can say. Locked keeps the count-only wording and offers unlock.
    enum FailedNotesAccess: Equatable {
        case notRead
        case readable
        case locked
        case unreadable
    }

    /// The most notes the Status screen lists before "Show all".
    static let listedNoteLimit = 5
    private static let readLimit = 100
    /// Pages followed per read (100 notes each), so a runaway cursor cannot loop forever.
    private static let maxPages = 50

    /// Newest first, as the core listed them. Empty unless `failedNotesAccess` is `readable`.
    @Published private(set) var failedNotes: [FailedNote] = []
    @Published private(set) var failedNotesAccess: FailedNotesAccess = .notRead
    /// The first line of each listed note's text, by item. A note whose text cannot be read has no entry.
    @Published private(set) var firstLines: [String: String] = [:]
    @Published private(set) var rerunSummary: FailedNotesRerunSummary?
    @Published private(set) var isRerunning = false

    private let health: ProcessingHealthReading
    private let failedWork: FailedInterpretationsControlling?
    private let noteText: FailedNoteTextReading?
    private var requestedLines: Set<String> = []
    /// Asks the person to unlock saved items; absent where the host offers none.
    let requestUnlock: (() -> Void)?
    private let permissions: PermissionsReading
    private let scheduling: ReminderSchedulingReading
    private let retrier: ProcessingRetrying
    private let openSystemSettings: () -> Void

    init(
        health: ProcessingHealthReading, permissions: PermissionsReading, retrier: ProcessingRetrying,
        scheduling: ReminderSchedulingReading = NoReminderSchedulingReport(),
        failedWork: FailedInterpretationsControlling? = nil, noteText: FailedNoteTextReading? = nil,
        requestUnlock: (() -> Void)? = nil,
        openSystemSettings: @escaping () -> Void
    ) {
        self.failedWork = failedWork
        self.noteText = noteText
        self.requestUnlock = requestUnlock
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
        await refreshFailedNotes()
        hasLoaded = true
        if !facts.contains(where: { $0.actions.contains(.retry) }) { retryRequested = false }
    }

    /// Runs the waiting work again from its saved source, then reads the status once more. The drain is asynchronous,
    /// so the read right after may still show the same state; the screen says the request was made.
    @MainActor func retry() async {
        retryRequested = true
        retrier.retryNow()
        await refresh()
    }

    /// The notes the Status screen lists: the first few, newest first.
    var listedNotes: [FailedNote] { Array(failedNotes.prefix(Self.listedNoteLimit)) }
    var hasMoreNotes: Bool { failedNotes.count > Self.listedNoteLimit }
    /// True when there are failed notes to run again.
    var offersNoteRetry: Bool { failedWork != nil && !failedNotes.isEmpty }

    /// Reads which notes failed to sort. A locked session keeps the section's count-only wording.
    @MainActor private func refreshFailedNotes() async {
        guard let failedWork else { return }
        do {
            var notes: [FailedNote] = []
            var cursor: String?
            for _ in 0..<Self.maxPages {
                let page = try await failedWork.failedInterpretations(limit: Self.readLimit, cursor: cursor)
                notes += page.failed.map(FailedNote.init)
                guard let next = page.nextCursor else { break }
                cursor = next
            }
            failedNotes = notes
            failedNotesAccess = .readable
        } catch {
            failedNotes = []
            firstLines = [:]
            requestedLines = []
            failedNotesAccess = (error as? JobConfigurationError) == .notAuthenticated ? .locked : .unreadable
            if failedNotesAccess == .locked { rerunSummary = nil }
        }
    }

    /// Reads one listed note's first line once, through the private item read. Row views call it as they appear.
    @MainActor func ensureFirstLine(itemID: String) async {
        guard let noteText, !requestedLines.contains(itemID) else { return }
        requestedLines.insert(itemID)
        if let line = await noteText.firstLine(itemID: itemID) {
            firstLines[itemID] = line
        } else {
            requestedLines.remove(itemID)
        }
    }

    /// Runs the listed failed notes again and records what happened. New jobs are then sent by a drain request.
    @MainActor func retryFailedNotes() async {
        guard let failedWork, !failedNotes.isEmpty, !isRerunning else { return }
        isRerunning = true
        defer { isRerunning = false }
        do {
            let result = try await failedWork.rerunFailedInterpretations(itemIDs: failedNotes.map(\.itemID))
            rerunSummary = FailedNotesRerunSummary(result)
            if result.moved > 0 { retrier.retryNow() }
        } catch {
            rerunSummary = .failed
        }
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
                detail: StatusText.captureDetail, actions: []),
            processingFact(health),
            notificationsFact(notifications, scheduling),
            StatusFact(
                kind: .sync, tone: .waiting, headline: StatusText.syncHeadline,
                detail: StatusText.syncDetail, actions: []),
            StatusFact(
                kind: .remoteInterpretation, tone: .waiting, headline: StatusText.remoteHeadline,
                detail: StatusText.remoteDetail, actions: []),
        ]
    }

    private static func processingFact(_ outcome: ProcessingHealthOutcome?) -> StatusFact {
        func fact(_ tone: StatusTone, _ headline: String, _ detail: String, _ actions: [StatusAction] = []) -> StatusFact {
            StatusFact(kind: .processing, tone: tone, headline: headline, detail: detail, actions: actions)
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
            // The core puts a stall ahead of needing attention, but its error list still carries any fault that needs
            // the person or has ended. Those stay visible next to the stall, with their own route.
            return fact(
                .needsAttention, StatusText.processingStalledHeadline,
                ([StatusText.processingStalledDetail] + attentionDetails(snapshot)).joined(separator: " "),
                [.retry] + attentionActions(snapshot))
        case .needsAttention:
            let details = attentionDetails(snapshot)
            return fact(
                .needsAttention, StatusText.processingAttentionHeadline,
                details.isEmpty ? StatusText.processingAttentionTerminalDetail : details.joined(separator: " "),
                attentionActions(snapshot))
        }
    }

    /// What the reported faults that will not clear by themselves mean for the person, one sentence per recovery path.
    private static func attentionDetails(_ snapshot: ProcessingHealth) -> [String] {
        var details: [String] = []
        if snapshot.errors.contains(where: { $0.recovery == .userAction }) {
            details.append(StatusText.processingAttentionUserDetail)
        }
        if snapshot.errors.contains(where: { $0.recovery == .terminal }) {
            details.append(StatusText.processingAttentionTerminalDetail)
        }
        return details
    }

    /// Re-running the queue cannot repair a destination, so a fault needing the person routes to Provider settings.
    private static func attentionActions(_ snapshot: ProcessingHealth) -> [StatusAction] {
        snapshot.errors.contains(where: { $0.recovery == .userAction }) ? [.openProviderSettings] : []
    }

    private static func notificationsFact(_ entry: PermissionEntry?, _ report: NotificationPermissionReport?) -> StatusFact {
        func fact(_ tone: StatusTone, _ headline: String, _ detail: String, _ actions: [StatusAction] = []) -> StatusFact {
            StatusFact(kind: .notifications, tone: tone, headline: headline, detail: detail, actions: actions)
        }
        guard let entry else {
            return fact(.waiting, StatusText.notificationsNotAskedHeadline, StatusText.notificationsNotAskedDetail)
        }
        switch entry.status {
        case .notRequested:
            return fact(.waiting, StatusText.notificationsNotAskedHeadline, StatusText.notificationsNotAskedDetail)
        case .denied:
            let headline = entry.change == .revoked ? StatusText.notificationsRevokedHeadline : StatusText.notificationsOffHeadline
            return fact(.needsAttention, headline, StatusText.notificationsOffDetail, [.openSettings])
        case .restricted:
            return fact(
                .needsAttention, StatusText.notificationsOffHeadline, StatusText.notificationsRestrictedDetail)
        case .granted:
            return grantedNotificationsFact(report, fact)
        }
    }

    /// Allowed notifications say nothing about scheduling, so this reads the last reminder pass itself. A pass that ran
    /// but did not converge, and daily prompts it held back, are reported too; neither reads as "nothing held back".
    private static func grantedNotificationsFact(
        _ report: NotificationPermissionReport?,
        _ fact: (StatusTone, String, String, [StatusAction]) -> StatusFact
    ) -> StatusFact {
        guard let report else {
            return fact(.waiting, StatusText.notificationsUncheckedHeadline, StatusText.notificationsUncheckedDetail, [])
        }
        if report.reconciliationFailure != nil {
            return fact(
                .needsAttention, StatusText.notificationsPassFailedHeadline, StatusText.notificationsPassFailedDetail, [])
        }
        let unfinished = report.converged ? [] : [StatusText.notificationsUnfinishedDetail]
        let heldBackPrompts = StatusText.heldBackPromptsDetail(
            deferred: report.promptsDeferred, withheld: report.promptsWithheld)
        func joined(_ first: [String]) -> String { (first + unfinished + heldBackPrompts).joined(separator: " ") }

        let reminders = report.readiness.reminders
        let unscheduledReasons = reminders.compactMap { status -> ReminderUnscheduledReason? in
            if case .unscheduled(let reason) = status.outlook { return reason }
            return nil
        }
        if !unscheduledReasons.isEmpty {
            let detail = joined(
                [StatusText.notificationsUnscheduledDetail(count: unscheduledReasons.count)]
                    + Set(unscheduledReasons.map(StatusText.unscheduledReason)).sorted())
            return fact(.needsAttention, StatusText.notificationsUnscheduledHeadline, detail, [])
        }
        if !report.converged {
            return fact(.needsAttention, StatusText.notificationsUnfinishedHeadline, joined([]), [])
        }
        let settledTone: StatusTone = heldBackPrompts.isEmpty ? .good : .waiting
        if report.readiness.awaitingInstallCount > 0 {
            return fact(
                .waiting, StatusText.notificationsInstallingHeadline,
                joined([StatusText.notificationsInstallingDetail(count: report.readiness.awaitingInstallCount)]), [])
        }
        if report.readiness.scheduledCount > 0 {
            return fact(
                settledTone, StatusText.notificationsScheduledHeadline,
                joined([StatusText.notificationsScheduledDetail(count: report.readiness.scheduledCount)]), [])
        }
        return fact(
            settledTone, StatusText.notificationsNoneHeadline,
            heldBackPrompts.isEmpty ? StatusText.notificationsNoneDetail : joined([StatusText.notificationsAllowedDetail]),
            [])
    }
}
