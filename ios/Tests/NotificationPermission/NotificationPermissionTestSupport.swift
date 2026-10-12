import Foundation
@testable import OhAndServices

/// A scripted `ReminderReconciling` for the reconciler's own unit tests: it records every request it is
/// given and answers with scripted reminder facts. The real-core behavior is covered by
/// `NotificationPermissionCoreIntegrationTests`, which runs `CoreReminderReconciler`.
final class RecordingReconciliation: ReminderReconciling, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedRequests: [ReminderReconciliationRequest] = []
    private var scriptedOutcomes: [Result<ReminderReconciliationOutcome, Error>] = []
    private var fallback: ReminderReconciliationOutcome
    private var stored: Result<[ReminderFacts], Error> = .failure(ReminderReconciliationFailure.coreUnavailable)
    var gate: SuspensionGate?

    init(facts: [ReminderFacts] = [], promptsGranted: Int = 0, promptsDeferred: Int = 0) {
        fallback = ReminderReconciliationOutcome(
            reminders: facts, promptsGranted: promptsGranted, promptsDeferred: promptsDeferred, converged: true)
    }

    func setStoredFacts(_ result: Result<[ReminderFacts], Error>) {
        lock.lock()
        defer { lock.unlock() }
        stored = result
    }

    func storedFacts() async throws -> [ReminderFacts] {
        lock.lock()
        defer { lock.unlock() }
        return try stored.get()
    }

    var requests: [ReminderReconciliationRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recordedRequests
    }

    func setOutcome(_ outcome: ReminderReconciliationOutcome) {
        lock.lock()
        defer { lock.unlock() }
        fallback = outcome
    }

    func script(_ result: Result<ReminderReconciliationOutcome, Error>) {
        lock.lock()
        defer { lock.unlock() }
        scriptedOutcomes.append(result)
    }

    func reconcile(_ request: ReminderReconciliationRequest) async throws -> ReminderReconciliationOutcome {
        lock.lock()
        recordedRequests.append(request)
        let gate = self.gate
        lock.unlock()
        if let gate { await gate.wait() }
        lock.lock()
        defer { lock.unlock() }
        if !scriptedOutcomes.isEmpty { return try scriptedOutcomes.removeFirst().get() }
        return fallback
    }
}

enum ReminderFixtures {
    static func resolved(
        _ itemID: String, schedule: String = "scheduled", delivery: String = "unknown"
    ) -> ReminderFacts {
        ReminderFacts(
            itemID: itemID, requestState: "resolved", scheduleState: schedule,
            deliveryState: delivery, acknowledgmentState: "not_acknowledged", unschedulableReason: nil)
    }

    static func unschedulable(_ itemID: String, reason: String) -> ReminderFacts {
        ReminderFacts(
            itemID: itemID, requestState: "unschedulable", scheduleState: "not_scheduled",
            deliveryState: "unknown", acknowledgmentState: "not_acknowledged", unschedulableReason: reason)
    }
}

/// Waits until `condition` holds, yielding so suspended tasks can run.
func eventually(timeout: TimeInterval = 5, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
    return condition()
}
