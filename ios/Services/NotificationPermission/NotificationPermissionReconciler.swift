import Foundation

/// The result of one permission read and core pass.
public struct NotificationPermissionReport: Equatable, Sendable {
    public let permission: NotificationPermissionState
    public let transition: NotificationPermissionTransition
    public let trigger: NotificationReconcileTrigger
    /// Reminder status under the permission just read. When the core pass failed it is built
    /// from the core's stored facts if they can still be read, else from the last pass that
    /// succeeded, so a revoked permission is still reported as undeliverable during an outage.
    public let readiness: NotificationReadinessReport
    public let promptsGranted: Int
    public let promptsDeferred: Int
    public let promptsWithheld: Int
    /// Set when the core pass did not complete. The permission transition stays unrecorded, so
    /// the next pass repeats it, including re-arming.
    public let reconciliationFailure: ReminderReconciliationFailure?
    public let converged: Bool
}

/// Reads the notification permission and, on every supported event (launch, foreground, return
/// from Settings), runs the core's reconciliation under it.
///
/// Permission loss or denial is reported as unscheduled/undeliverable status; it removes nothing
/// from the core's durable reminders. Regaining it re-arms schedules the OS rejected as
/// unauthorized and lets the core's capacity pass refill held-back reminders. Revocation, denial
/// and the passage of time are never recorded as delivery or attention: this type has no path
/// that writes either.
///
/// Passes are single-flight. A refresh that arrives while one runs joins it instead of starting
/// a second, so concurrent launch and foreground events cannot repeat a re-arm or install a
/// request twice; the permission is read once more afterwards, and a change that happened during
/// the pass starts a new one. The core's pass is also idempotent.
public actor NotificationPermissionReconciler {
    private let center: NotificationCenterProviding
    private let reconciliation: ReminderReconciling
    private let observations: NotificationPermissionObservationStore
    private let promptCandidates: @Sendable () -> [OptionalPromptCandidate]
    private var inFlight: (id: UUID, task: Task<NotificationPermissionReport, Error>)?
    private var lastKnownReminders: [ReminderFacts] = []

    public init(
        center: NotificationCenterProviding,
        reconciliation: ReminderReconciling,
        observations: NotificationPermissionObservationStore,
        promptCandidates: @escaping @Sendable () -> [OptionalPromptCandidate] = { [] }
    ) {
        self.center = center
        self.reconciliation = reconciliation
        self.observations = observations
        self.promptCandidates = promptCandidates
    }

    @discardableResult
    public func refresh(trigger: NotificationReconcileTrigger) async throws -> NotificationPermissionReport {
        while true {
            try Task.checkCancellation()
            if let running = inFlight {
                let joined = try await running.task.value
                let current = NotificationPermissionState(await center.authorization())
                if current == joined.permission { return joined }
                continue
            }
            let id = UUID()
            let task = Task { [self] in
                do {
                    let report = try await runPass(trigger: trigger)
                    finish(id)
                    return report
                } catch {
                    finish(id)
                    throw error
                }
            }
            inFlight = (id, task)
            return try await task.value
        }
    }

    private func finish(_ id: UUID) {
        if inFlight?.id == id { inFlight = nil }
    }

    private func runPass(trigger: NotificationReconcileTrigger) async throws -> NotificationPermissionReport {
        let permission = NotificationPermissionState(await center.authorization())
        let transition = NotificationPermissionTransition.between(
            previous: observations.lastObserved(), current: permission)
        let request = ReminderReconciliationRequest(
            permission: permission,
            transition: transition,
            trigger: trigger,
            rearmRejectedSchedules: permission.canAlert && (transition == .regained || transition == .firstObservation),
            allowOptionalPrompts: permission.canAlert,
            promptCandidates: promptCandidates()
        )
        do {
            let outcome = try await reconciliation.reconcile(request)
            lastKnownReminders = outcome.reminders
            observations.record(permission)
            return NotificationPermissionReport(
                permission: permission,
                transition: transition,
                trigger: trigger,
                readiness: NotificationReadiness.evaluate(permission: permission, reminders: outcome.reminders),
                promptsGranted: outcome.promptsGranted,
                promptsDeferred: outcome.promptsDeferred,
                promptsWithheld: outcome.promptsWithheld,
                reconciliationFailure: nil,
                converged: outcome.converged
            )
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            let reportedReminders = await factsAfterFailedPass()
            return NotificationPermissionReport(
                permission: permission,
                transition: transition,
                trigger: trigger,
                readiness: NotificationReadiness.evaluate(permission: permission, reminders: reportedReminders),
                promptsGranted: 0,
                promptsDeferred: 0,
                promptsWithheld: 0,
                reconciliationFailure: ReminderReconciliationFailure.normalized(error),
                converged: false
            )
        }
    }

    /// The core's stored facts when it can still be read, else the facts of the last pass that
    /// succeeded, so a revoked permission is reported even before any pass has completed.
    private func factsAfterFailedPass() async -> [ReminderFacts] {
        if let stored = try? await reconciliation.storedFacts() {
            lastKnownReminders = stored
            return stored
        }
        return lastKnownReminders
    }
}
