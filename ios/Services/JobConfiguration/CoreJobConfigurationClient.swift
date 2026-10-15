import Foundation
import OhAndCoreBridge

/// The core's job-configuration controls as native code uses them (`core/src/ffi/job_configuration.rs`,
/// `ohand_core_start_job_configuration`). The core owns every rule: which jobs may move, which profile a re-pin selects,
/// and whether that profile is authorized for the route.
protocol JobConfigurationCore: Sendable {
    /// Queues `command` and calls `completion` once with the core's answer. Commands reach the core in call order.
    func submit(
        _ command: JobConfigurationCommand,
        completion: @escaping @Sendable (Result<JobConfigurationAnswer, CoreFailure>) -> Void)
}

/// Runs job-configuration requests over a `CoreHandle`: the production `JobConfigurationCore`. The core answers on its
/// single event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when
/// the event answered one of this client's operations. Operation IDs start at `firstOperationID`, far from the ranges the
/// other services use. Call `shutDown()` before closing the core so no completion is left waiting.
public final class CoreJobConfigurationClient: JobConfigurationCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 47

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the job configuration client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private struct Pending {
        let command: JobConfigurationCommand
        let completion: @Sendable (Result<JobConfigurationAnswer, CoreFailure>) -> Void
    }

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreJobConfigurationClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    func submit(
        _ command: JobConfigurationCommand,
        completion: @escaping @Sendable (Result<JobConfigurationAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = Pending(command: command, completion: completion)
        lock.unlock()
        do {
            try core.startJobConfiguration(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.completion(.failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let waiting = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let waiting else { return false }
        switch event.outcome {
        case .failure(let failure):
            waiting.completion(.failure(failure))
        case .success(let data):
            if let answer = try? JobConfigurationAnswer.decode(data, for: waiting.command) {
                waiting.completion(.success(answer))
            } else {
                waiting.completion(.failure(Self.unreadableAnswer))
            }
        }
        return true
    }

    /// Fails every operation still waiting and refuses later ones. Idempotent.
    public func shutDown() {
        lock.lock()
        isShutDown = true
        let waiting = pending
        pending = [:]
        lock.unlock()
        for entry in waiting.values {
            entry.completion(.failure(Self.shutDownFailure))
        }
    }
}
