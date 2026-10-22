import Foundation
import OhAndCoreBridge
import OhAndServices

/// Reads processing health through the lifecycle coordinator, which owns the health service's observer. It listens with
/// `setHealthObserver` and never takes the service's own observer, so the coordinator's launch and foreground health
/// steps keep working. A read that does not finish within the bound is reported as unreadable, never as healthy.
final class LifecycleHealthReader: ProcessingHealthReading, @unchecked Sendable {
    private let lifecycle: LifecycleCoordinator
    private let timeoutSeconds: TimeInterval
    private let lock = NSLock()
    private var waiters: [UUID: CheckedContinuation<ProcessingHealthOutcome, Never>] = [:]

    init(lifecycle: LifecycleCoordinator, timeoutSeconds: TimeInterval = 10) {
        self.lifecycle = lifecycle
        self.timeoutSeconds = timeoutSeconds
        lifecycle.setHealthObserver { [weak self] outcome in self?.deliver(outcome) }
    }

    func read() async -> ProcessingHealthOutcome {
        guard let service = lifecycle.processingHealth else { return .failed(CoreFailure.handleClosed) }
        let waiterID = UUID()
        let timeout = timeoutSeconds
        return await withCheckedContinuation { (continuation: CheckedContinuation<ProcessingHealthOutcome, Never>) in
            lock.lock()
            waiters[waiterID] = continuation
            lock.unlock()
            service.refresh()
            // A read that failed to start finishes without a core event, so the coordinator never hears of it.
            if !service.isReading, let finished = service.latestOutcome {
                finish(waiterID, with: finished)
            }
            Task.detached { [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
                self?.finish(waiterID, with: .failed(CoreFailure.handleClosed))
            }
        }
    }

    private func deliver(_ outcome: ProcessingHealthOutcome) {
        lock.lock()
        let pending = waiters
        waiters = [:]
        lock.unlock()
        pending.values.forEach { $0.resume(returning: outcome) }
    }

    private func finish(_ waiterID: UUID, with outcome: ProcessingHealthOutcome) {
        lock.lock()
        let continuation = waiters.removeValue(forKey: waiterID)
        lock.unlock()
        continuation?.resume(returning: outcome)
    }
}

/// The reminder pass the composition root last recorded.
struct RootReminderSchedulingReader: ReminderSchedulingReading {
    let root: ServiceCompositionRoot

    func lastReport() -> NotificationPermissionReport? { root.lastReminderReport }
}
