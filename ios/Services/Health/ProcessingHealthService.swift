import Foundation
import OhAndCoreBridge

/// The result of one health read: the core's snapshot, or the failure that prevented it.
public enum ProcessingHealthOutcome: Equatable, Sendable {
    case snapshot(ProcessingHealth)
    case failed(CoreFailure)
}

/// Reads background-processing health from the core on demand.
///
/// Health is a pull-only fact. This type never posts a notification, schedules anything or touches the job queue: a
/// reader asks, the core answers from durable state, and the owner of a status surface decides whether to show it.
/// A read runs on the core's worker and never blocks a capture save, and capture never waits for one.
///
/// * **One read at a time.** `refresh()` while a read is outstanding does nothing; the outstanding read's snapshot is
///   what the caller gets.
/// * **A failed read is a failed read.** It is reported as `.failed` and never as a healthy or idle snapshot.
///
/// The owner of the core's single event handler must forward every event to `handle(_:)`; it returns `true` when the
/// event belonged to this service.
public final class ProcessingHealthService: @unchecked Sendable {
    /// Operation IDs for health reads start here: above ordinary core operations and below the job runner's range.
    public static let firstOperationID: UInt64 = 1 << 61

    private let core: CoreHandle
    private let stallAfterSeconds: UInt32
    private let leaseGraceSeconds: UInt32
    private let stateLock = NSLock()
    private var nextOperationID = ProcessingHealthService.firstOperationID
    private var activeOperationID: UInt64?
    private var latest: ProcessingHealthOutcome?
    private var observer: (@Sendable (ProcessingHealthOutcome) -> Void)?

    /// `stallAfterSeconds` and `leaseGraceSeconds` of 0 select the core's defaults.
    public init(core: CoreHandle, stallAfterSeconds: UInt32 = 0, leaseGraceSeconds: UInt32 = 0) {
        self.core = core
        self.stallAfterSeconds = stallAfterSeconds
        self.leaseGraceSeconds = leaseGraceSeconds
    }

    /// The most recent completed read, if any.
    public var latestOutcome: ProcessingHealthOutcome? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return latest
    }

    public var isReading: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return activeOperationID != nil
    }

    /// Called with every completed read, on the thread that delivered the core event (the main queue).
    public func setObserver(_ observer: (@Sendable (ProcessingHealthOutcome) -> Void)?) {
        stateLock.lock()
        self.observer = observer
        stateLock.unlock()
    }

    /// Asks the core for a fresh snapshot unless a read is already outstanding.
    public func refresh() {
        stateLock.lock()
        guard activeOperationID == nil else {
            stateLock.unlock()
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        activeOperationID = operationID
        stateLock.unlock()

        do {
            try core.startProcessingHealth(
                operationID: operationID, stallAfterSeconds: stallAfterSeconds, leaseGraceSeconds: leaseGraceSeconds)
        } catch {
            let failure = (error as? CoreFailure) ?? CoreFailure.handleClosed
            finish(operationID: operationID, outcome: .failed(failure))
        }
    }

    /// Applies a core event. Returns `true` when it belonged to this service's reads (or was a stale event of one),
    /// in which case the caller should not process it further.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        guard event.operationID >= Self.firstOperationID, event.operationID < JobRunnerService.firstOperationID else {
            return false
        }
        finish(operationID: event.operationID, outcome: Self.outcome(of: event))
        return true
    }

    private func finish(operationID: UInt64, outcome: ProcessingHealthOutcome) {
        stateLock.lock()
        guard operationID == activeOperationID else {
            stateLock.unlock()
            return
        }
        activeOperationID = nil
        latest = outcome
        let notify = observer
        stateLock.unlock()
        notify?(outcome)
    }

    private static func outcome(of event: CoreEvent) -> ProcessingHealthOutcome {
        switch event.outcome {
        case .failure(let failure):
            return .failed(failure)
        case .success(let bytes):
            guard let snapshot = try? JSONDecoder().decode(ProcessingHealth.self, from: bytes) else {
                return .failed(
                    CoreFailure(
                        errorClass: .permanent, code: "unreadable_health", message: "the health snapshot was unreadable"))
            }
            return .snapshot(snapshot)
        }
    }
}
