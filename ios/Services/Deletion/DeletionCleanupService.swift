import Foundation

/// Executes and reconciles the native effects of per-item deletion: notification cancellation, ingress staging and
/// audio removal. The core records the tombstone, removes readable and searchable content, cancels jobs and writes one
/// durable cleanup row per effect (L01); this service performs each effect and records its completion.
///
/// Every step is idempotent and ordered effect first, completion second. A launch or foreground pass calls
/// `reconcile()`, which finds items with pending rows and repeats whatever an interruption left undone, so reopening
/// after a failure at any step converges without a separate recovery path. A step that cannot finish stays pending and
/// is reported with a fixed retry reason; only a step that can never succeed is recorded as terminally failed. Steps of
/// one item are independent, so one failing effect never blocks the others.
///
/// Calls are serialized: a second call waits for the running one, so two passes never perform the same effect at once.
/// Delete-all generation fencing and credential reset are outside this per-item cleanup.
actor DeletionCleanupService {
    private static let stepOrder: [DeletionCleanupKind] = [.cancelNotifications, .clearIngress, .removeAudio]

    private let core: DeletionCleanupCore
    private let notifications: DeletionNotificationCancelling
    private let localCleanup: DeletionLocalCleanup
    private var isRunning = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(
        core: DeletionCleanupCore,
        notifications: DeletionNotificationCancelling,
        localCleanup: DeletionLocalCleanup
    ) {
        self.core = core
        self.notifications = notifications
        self.localCleanup = localCleanup
    }

    /// Completes the pending cleanup of every deleted item. Safe to call on every launch and foreground, repeatedly.
    func reconcile() async -> DeletionReconcileReport {
        await acquire()
        defer { release() }

        let itemIDs: [String]
        do {
            itemIDs = try await core.itemsWithPendingCleanup()
        } catch {
            return DeletionReconcileReport(listingFailure: Self.coreReason(error), items: [])
        }
        var results: [DeletionItemResult] = []
        for itemID in Set(itemIDs).sorted() {
            if Task.isCancelled {
                return DeletionReconcileReport(listingFailure: .cancelled, items: results)
            }
            results.append(await cleanUpLocked(itemID: itemID))
        }
        // Read after the pass so a row this pass failed is included. A terminally failed row leaves the pending list,
        // so without this an empty pending list would read as convergence.
        do {
            let failures = try await core.terminallyFailedCleanup()
            return DeletionReconcileReport(
                listingFailure: nil,
                items: results.map { Self.attaching(failures, to: $0) },
                terminalFailures: failures.map { DeletionTerminalFailure(itemID: $0.itemID, kind: $0.kind) })
        } catch {
            return DeletionReconcileReport(listingFailure: Self.coreReason(error), items: results)
        }
    }

    /// Completes the pending cleanup of one item, typically right after the core recorded its deletion. The result
    /// includes the item's terminally failed rows, so a repeat call after a terminal failure is never reported settled.
    func cleanUp(itemID: String) async -> DeletionItemResult {
        await acquire()
        defer { release() }
        let result = await cleanUpLocked(itemID: itemID)
        guard result.listingFailure == nil else { return result }
        do {
            return Self.attaching(try await core.terminallyFailedCleanup(), to: result)
        } catch {
            return DeletionItemResult(itemID: itemID, listingFailure: Self.coreReason(error), steps: result.steps)
        }
    }

    private static func attaching(_ failures: [DeletionCleanupWork], to result: DeletionItemResult) -> DeletionItemResult {
        DeletionItemResult(
            itemID: result.itemID, listingFailure: result.listingFailure, steps: result.steps,
            terminalFailures: failures.filter { $0.itemID == result.itemID }.map { $0.kind })
    }

    private func cleanUpLocked(itemID: String) async -> DeletionItemResult {
        let pending: [DeletionCleanupWork]
        do {
            pending = try await core.pendingCleanup(itemID: itemID)
        } catch {
            return DeletionItemResult(itemID: itemID, listingFailure: Self.coreReason(error), steps: [])
        }
        var steps: [DeletionStepResult] = []
        for kind in Self.stepOrder {
            for work in pending where work.kind == kind {
                steps.append(DeletionStepResult(kind: kind, outcome: await runStep(work)))
            }
        }
        return DeletionItemResult(itemID: itemID, listingFailure: nil, steps: steps)
    }

    private func runStep(_ work: DeletionCleanupWork) async -> DeletionStepOutcome {
        if Task.isCancelled { return .retryLater(.cancelled) }
        switch work.kind {
        case .cancelNotifications: return await cancelNotifications(work)
        case .clearIngress: return await clearIngress(work)
        case .removeAudio: return await removeAudio(work)
        }
    }

    private func cancelNotifications(_ work: DeletionCleanupWork) async -> DeletionStepOutcome {
        let identifiers: [NotificationIdentifier]
        do {
            identifiers = try await core.reminderNotificationIdentifiers(itemID: work.itemID)
        } catch {
            return .retryLater(Self.coreReason(error))
        }

        var firstFailure: DeletionRetryReason?
        for identifier in identifiers {
            if Task.isCancelled { return .retryLater(.cancelled) }
            do {
                try await notifications.cancel(identifier)
            } catch {
                firstFailure = firstFailure ?? Self.notificationReason(error)
                continue
            }
            do {
                try await core.recordNotificationCancelled(identifier)
            } catch {
                firstFailure = firstFailure ?? Self.coreReason(error)
            }
        }
        if let firstFailure { return .retryLater(firstFailure) }

        // A late install can leave a request under one of the item's reminders that the core never learned of, so the
        // OS list, not the core's record alone, decides whether anything is still active.
        let reminderIDs = Set(identifiers.map { $0.reminderID })
        if !reminderIDs.isEmpty {
            do {
                for leftover in try await pending(forReminders: reminderIDs) {
                    try await notifications.cancel(leftover.identifier)
                }
                let remaining = try await pending(forReminders: reminderIDs)
                if !remaining.isEmpty {
                    return .retryLater(.notificationStillPending)
                }
            } catch {
                return .retryLater(Self.notificationReason(error))
            }
        }
        return await recordCompleted(work)
    }

    private func pending(forReminders reminderIDs: Set<OpaqueIdentifier>) async throws -> [PendingNotification] {
        try await notifications.pendingNotifications().filter { reminderIDs.contains($0.identifier.reminderID) }
    }

    private func clearIngress(_ work: DeletionCleanupWork) async -> DeletionStepOutcome {
        let target: DeletionCleanupTarget?
        do {
            target = try await core.cleanupTarget(itemID: work.itemID)
        } catch {
            return .retryLater(Self.coreReason(error))
        }
        if let target {
            do {
                try localCleanup.clearIngress(captureID: target.captureID)
            } catch DeletionLocalCleanupError.unsafeCaptureIdentifier {
                return await recordFailure(work, .unsafeCaptureIdentifier)
            } catch DeletionLocalCleanupError.ingressOwnershipUnresolved {
                return .retryLater(.ingressOwnershipUnresolved)
            } catch {
                return .retryLater(.storageUnavailable)
            }
        }
        return await recordCompleted(work)
    }

    private func removeAudio(_ work: DeletionCleanupWork) async -> DeletionStepOutcome {
        let target: DeletionCleanupTarget?
        do {
            target = try await core.cleanupTarget(itemID: work.itemID)
        } catch {
            return .retryLater(Self.coreReason(error))
        }
        if let target {
            do {
                try localCleanup.removeAudio(captureID: target.captureID, audioReference: target.audioReference)
            } catch DeletionLocalCleanupError.unsafeCaptureIdentifier {
                return await recordFailure(work, .unsafeCaptureIdentifier)
            } catch DeletionLocalCleanupError.audioReferenceUnresolvable {
                return await recordFailure(work, .audioReferenceUnresolvable)
            } catch PlaybackFenceError.audioInUse {
                return .retryLater(.audioInPlayback)
            } catch {
                return .retryLater(.storageUnavailable)
            }
        }
        return await recordCompleted(work)
    }

    private func recordCompleted(_ work: DeletionCleanupWork) async -> DeletionStepOutcome {
        do {
            try await core.completeCleanup(workID: work.workID)
            return .completed
        } catch {
            return .retryLater(Self.coreReason(error))
        }
    }

    private func recordFailure(_ work: DeletionCleanupWork, _ reason: DeletionPermanentFailure) async -> DeletionStepOutcome {
        do {
            try await core.failCleanup(workID: work.workID)
            return .failedPermanently(reason)
        } catch {
            return .retryLater(Self.coreReason(error))
        }
    }

    private static func coreReason(_ error: Error) -> DeletionRetryReason {
        (Task.isCancelled || error is CancellationError) ? .cancelled : .coreUnavailable
    }

    private static func notificationReason(_ error: Error) -> DeletionRetryReason {
        if Task.isCancelled || error is CancellationError { return .cancelled }
        if let bridgeError = error as? NotificationBridgeError, bridgeError.errorClass == .cancelled { return .cancelled }
        return .notificationUnavailable
    }

    private func acquire() async {
        if isRunning {
            await withCheckedContinuation { (waiter: CheckedContinuation<Void, Never>) in
                waiters.append(waiter)
            }
        } else {
            isRunning = true
        }
    }

    private func release() {
        if waiters.isEmpty {
            isRunning = false
        } else {
            waiters.removeFirst().resume()
        }
    }
}
