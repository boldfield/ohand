import Foundation

/// The result of one permission read and core pass.
public struct NotificationPermissionReport: Equatable, Sendable {
    public let permission: NotificationPermissionState
    public let transition: NotificationPermissionTransition
    public let trigger: NotificationReconcileTrigger
    /// Reminder status under the permission just read. When the core pass failed it is built
    /// from the facts of the last pass that succeeded, so a revoked permission is still reported
    /// as undeliverable during a core outage.
    public let readiness: NotificationReadinessReport
    public let promptsInstalled: Int
    public let promptsDeferred: Int
    /// Set when the core pass did not complete. The permission transition stays unrecorded, so
    /// the next pass repeats it, including re-arming.
    public let reconciliationFailure: NotificationBridgeError?
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
    private var inFlight: (id: UUID, task: Task<NotificationPermissionReport, Error>)?
    private var lastKnownReminders: [ReminderFacts] = []

    public init(
        center: NotificationCenterProviding,
        reconciliation: ReminderReconciling,
        observations: NotificationPermissionObservationStore
    ) {
        self.center = center
        self.reconciliation = reconciliation
        self.observations = observations
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
            allowOptionalPrompts: permission.canAlert
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
                promptsInstalled: outcome.promptsInstalled,
                promptsDeferred: outcome.promptsDeferred,
                reconciliationFailure: nil,
                converged: outcome.converged
            )
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            return NotificationPermissionReport(
                permission: permission,
                transition: transition,
                trigger: trigger,
                readiness: NotificationReadiness.evaluate(permission: permission, reminders: lastKnownReminders),
                promptsInstalled: 0,
                promptsDeferred: 0,
                reconciliationFailure: NotificationBridgeError.normalized(error),
                converged: false
            )
        }
    }
}
