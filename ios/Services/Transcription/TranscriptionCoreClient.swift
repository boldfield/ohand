import Foundation
import OhAndCoreBridge

/// The two core operations the transcription handler needs. The core owns every rule about what an outcome may
/// change; this seam only carries the request and the answer.
public protocol TranscriptionCoreOperations: AnyObject, Sendable {
    /// Reads the running job under its lease.
    func readSource(jobID: String, attempt: Int) async -> Result<TranscriptionJobSource, CoreFailure>
    /// Applies one recognizer outcome through the core's guarded transcript attachment. A success means the core's
    /// transaction committed (or an earlier identical delivery had).
    func apply(_ outcome: TranscriptionOutcomeRequest) async -> Result<TranscriptionApplyAcknowledgment, CoreFailure>
}

/// Runs the transcription core operations over a `CoreHandle` and hands each answer back to the awaiting caller.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to
/// `handle(_:)`; it returns `true` when the event answered one of this client's operations. Operation IDs start at
/// `firstOperationID`, far from the ranges the other services use. Call `shutDown()` before closing the core so no
/// caller is left waiting.
public final class TranscriptionCoreClient: TranscriptionCoreOperations, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 50

    private enum Pending {
        case read(CheckedContinuation<Result<TranscriptionJobSource, CoreFailure>, Never>)
        case apply(CheckedContinuation<Result<TranscriptionApplyAcknowledgment, CoreFailure>, Never>)
    }

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = TranscriptionCoreClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    public func readSource(jobID: String, attempt: Int) async -> Result<TranscriptionJobSource, CoreFailure> {
        await withCheckedContinuation { continuation in
            submit(.read(continuation)) { operationID in
                try self.core.startReadTranscriptionJob(operationID: operationID, jobID: jobID, leaseAttempt: attempt)
            }
        }
    }

    public func apply(_ outcome: TranscriptionOutcomeRequest) async -> Result<TranscriptionApplyAcknowledgment, CoreFailure> {
        await withCheckedContinuation { continuation in
            submit(.apply(continuation)) { operationID in
                try self.core.startApplyTranscriptionOutcome(operationID: operationID, outcome: outcome)
            }
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let operation = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let operation else { return false }
        switch operation {
        case .read(let continuation):
            continuation.resume(returning: Self.result(of: event, as: TranscriptionJobSource.self))
        case .apply(let continuation):
            continuation.resume(returning: Self.result(of: event, as: TranscriptionApplyAcknowledgment.self))
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
        for operation in waiting.values {
            Self.fail(operation, with: Self.shutDownFailure)
        }
    }

    // MARK: private

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the transcription client was shut down")
    private static let unreadableEvent = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private func submit(_ operation: Pending, start: (UInt64) throws -> Void) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            Self.fail(operation, with: Self.shutDownFailure)
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = operation
        lock.unlock()
        do {
            try start(operationID)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            if let removed {
                Self.fail(removed, with: (error as? CoreFailure) ?? Self.unreadableEvent)
            }
        }
    }

    private static func fail(_ operation: Pending, with failure: CoreFailure) {
        switch operation {
        case .read(let continuation):
            continuation.resume(returning: .failure(failure))
        case .apply(let continuation):
            continuation.resume(returning: .failure(failure))
        }
    }

    private static func result<Payload: Decodable>(of event: CoreEvent, as type: Payload.Type) -> Result<Payload, CoreFailure> {
        switch event.outcome {
        case .failure(let failure):
            return .failure(failure)
        case .success(let data):
            do {
                return .success(try JSONDecoder().decode(type, from: data))
            } catch {
                return .failure(unreadableEvent)
            }
        }
    }
}
