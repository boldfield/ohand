import Foundation

/// `PendingProcessingReading` over the core's processing-health snapshot, whose pending count already includes
/// running jobs.
///
/// It uses `refresh()` and the service's latest outcome, never its observer slot, so a status surface that observes
/// the same service is not displaced. The owner of the core's event handler must forward events to the health
/// service as usual; without that no snapshot arrives and the read ends `unknown` at its timeout, never `none`.
public struct ProcessingHealthPendingReader: PendingProcessingReading {
    private let service: ProcessingHealthService
    private let timeout: TimeInterval
    private let pollInterval: TimeInterval

    public init(service: ProcessingHealthService, timeout: TimeInterval = 3, pollInterval: TimeInterval = 0.02) {
        self.service = service
        self.timeout = timeout
        self.pollInterval = pollInterval
    }

    public func read() async -> PendingProcessingStatus {
        service.refresh()
        let deadline = Date().addingTimeInterval(timeout)
        while service.isReading {
            if Task.isCancelled || Date() >= deadline { return .unknown }
            try? await Task.sleep(nanoseconds: UInt64(max(0.001, pollInterval) * 1_000_000_000))
        }
        guard case .snapshot(let health)? = service.latestOutcome else { return .unknown }
        return health.pendingJobs > 0 ? .pending(jobs: health.pendingJobs) : PendingProcessingStatus.none
    }
}
