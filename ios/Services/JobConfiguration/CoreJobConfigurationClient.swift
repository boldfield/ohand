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

/// Runs job-configuration requests over a `CoreHandle`: the production `JobConfigurationCore`. The reads of stored
/// state (`jobsPinnedToProfile`, `jobsRetiredByRevocation`, `requeueRecord`) go only through the `PrivateReadGate`, so a capture-only session never
/// reaches the core and an answer that arrives after a relock is dropped undecoded. Mutations go straight to the core;
/// `JobConfigurationService` requires an authenticated session before it sends one. The core answers on its
/// single event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when
/// the event answered one of this client's operations. Operation IDs start at `firstOperationID`, far from the ranges the
/// other services use. Call `shutDown()` before closing the core so no completion is left waiting.
final class CoreJobConfigurationClient: JobConfigurationCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 47

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the job configuration client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private struct Pending {
        let command: JobConfigurationCommand
        let isGatedRead: Bool
        let completion: @Sendable (Result<JobConfigurationAnswer, CoreFailure>) -> Void
    }

    private let core: CoreHandle
    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreJobConfigurationClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    init(core: CoreHandle, gate: PrivateReadGate) {
        self.core = core
        self.gate = gate
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
        let isGatedRead = Self.isStoredStateRead(command)
        pending[operationID] = Pending(command: command, isGatedRead: isGatedRead, completion: completion)
        lock.unlock()
        do {
            switch command {
            case .jobsPinnedToProfile(let profileVersion, let limit):
                try gate.startJobsPinnedToProfile(operationID: operationID, profileVersion: profileVersion, limit: limit)
            case .jobsRetiredByRevocation(let profileVersion, let limit, let after):
                try gate.startJobsRetiredByRevocation(
                    operationID: operationID, profileVersion: profileVersion, limit: limit, after: after)
            case .requeueRecord(let jobID):
                try gate.startRequeueRecord(operationID: operationID, jobID: jobID)
            case .requeueJob, .revokeProfile, .repinWaitingWork:
                try core.startJobConfiguration(operationID: operationID, command: command)
            }
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            if let denial = error as? PrivateReadDenied {
                removed?.completion(.failure(CoreItemStatusReader.failure(for: denial)))
            } else {
                removed?.completion(.failure((error as? CoreFailure) ?? Self.unreadableAnswer))
            }
        }
    }

    private static func isStoredStateRead(_ command: JobConfigurationCommand) -> Bool {
        switch command {
        case .jobsPinnedToProfile, .jobsRetiredByRevocation, .requeueRecord: return true
        case .requeueJob, .revokeProfile, .repinWaitingWork: return false
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let waiting = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let waiting else { return false }
        if waiting.isGatedRead {
            do {
                try gate.admit(operationID: event.operationID)
            } catch {
                let denial = (error as? PrivateReadDenied) ?? .notAGatedRead
                waiting.completion(.failure(CoreItemStatusReader.failure(for: denial)))
                return true
            }
        }
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
    func shutDown() {
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
