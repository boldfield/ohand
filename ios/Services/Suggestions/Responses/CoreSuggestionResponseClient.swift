import Foundation
import OhAndCoreBridge

/// The core's prompt activation and responses as native code uses them (`core/src/ffi/suggestion_responses.rs`,
/// `ohand_core_start_suggestion_response`). The core owns eligibility, revision comparison and idempotency.
protocol SuggestionResponseCore: Sendable {
    /// Queues `command` and calls `completion` once with the core's answer. Commands reach the core in call order.
    func submit(
        _ command: SuggestionResponseCommand,
        completion: @escaping @Sendable (Result<SuggestionResponseAnswer, CoreFailure>) -> Void)
}

/// The private read gate's verb for an activation, which returns stored text. The gate checks the session when the
/// activation starts and again when its answer arrives, so an answer that lands after a relock is dropped undecoded.
protocol SuggestionActivationGate: AnyObject {
    func startSuggestionActivation(operationID: UInt64, start: () throws -> Void) throws
    func admit(operationID: UInt64) throws
}

extension PrivateReadGate: SuggestionActivationGate {}

/// Runs suggestion commands over a `CoreHandle`: the production `SuggestionResponseCore`. An activation goes through
/// the read gate; a response is a write and relies on the service's session check. The core answers on its single
/// event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when the
/// event answered one of this client's operations. Operation IDs start at `firstOperationID`, far from the ranges the
/// other services use. Call `shutDown()` before closing the core so no completion is left waiting.
public final class CoreSuggestionResponseClient: SuggestionResponseCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 55

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the suggestion response client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let gate: SuggestionActivationGate
    private let lock = NSLock()
    private var nextOperationID = CoreSuggestionResponseClient.firstOperationID
    private var pending: [UInt64: @Sendable (Result<SuggestionResponseAnswer, CoreFailure>) -> Void] = [:]
    private var gatedOperations: Set<UInt64> = []
    private var isShutDown = false

    init(core: CoreHandle, gate: SuggestionActivationGate) {
        self.core = core
        self.gate = gate
    }

    func submit(
        _ command: SuggestionResponseCommand,
        completion: @escaping @Sendable (Result<SuggestionResponseAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = completion
        if case .activatePrompt = command {
            gatedOperations.insert(operationID)
        }
        lock.unlock()
        do {
            switch command {
            case .activatePrompt:
                try gate.startSuggestionActivation(operationID: operationID) {
                    try core.startSuggestionResponse(operationID: operationID, command: command)
                }
            case .respond:
                try core.startSuggestionResponse(operationID: operationID, command: command)
            }
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            gatedOperations.remove(operationID)
            lock.unlock()
            if let denial = error as? PrivateReadDenied {
                removed?(.failure(CoreItemStatusReader.failure(for: denial)))
            } else {
                removed?(.failure((error as? CoreFailure) ?? Self.unreadableAnswer))
            }
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let completion = pending.removeValue(forKey: event.operationID)
        let wasGated = gatedOperations.remove(event.operationID) != nil
        lock.unlock()
        guard let completion else { return false }
        if wasGated {
            do {
                try gate.admit(operationID: event.operationID)
            } catch {
                let denial = (error as? PrivateReadDenied) ?? .notAGatedRead
                completion(.failure(CoreItemStatusReader.failure(for: denial)))
                return true
            }
        }
        switch event.outcome {
        case .failure(let failure):
            completion(.failure(failure))
        case .success(let data):
            if let answer = try? JSONDecoder().decode(SuggestionResponseAnswer.self, from: data) {
                completion(.success(answer))
            } else {
                completion(.failure(Self.unreadableAnswer))
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
        gatedOperations = []
        lock.unlock()
        for completion in waiting.values {
            completion(.failure(Self.shutDownFailure))
        }
    }
}
