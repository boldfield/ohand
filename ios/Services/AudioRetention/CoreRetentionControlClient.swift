import Foundation
import OhAndCoreBridge

/// The core's retention reads and user commands as native code uses them (`core/src/ffi/retention_controls.rs`,
/// `ohand_core_start_retention_read` and `ohand_core_start_retention_control`). The core owns the stored period, the
/// bounds and the list of unresolved sole audio; removing a file is not here (see `AudioRetentionService`).
protocol RetentionControlCore: Sendable {
    /// Reads the stored policy and the unresolved sole-audio cases through the private read gate; calls `completion`
    /// once.
    func readStatus(completion: @escaping @Sendable (Result<RetentionStatusAnswer, CoreFailure>) -> Void)
    /// Reads one item's audio reference through the private read gate; calls `completion` once.
    func readAudio(
        itemID: String, completion: @escaping @Sendable (Result<RetentionAudioAnswer, CoreFailure>) -> Void)
    /// Queues `command` and calls `completion` once with the core's answer.
    func submit(
        _ command: RetentionControlCommand,
        completion: @escaping @Sendable (Result<RetentionControlAnswer, CoreFailure>) -> Void)
}

/// Runs retention reads and commands over a `CoreHandle`: the production `RetentionControlCore`. Reads go only through
/// the `PrivateReadGate`, so a capture-only session never reaches the core and an answer that arrives after a relock is
/// dropped undecoded. Commands go straight to the core; `RetentionControlService` requires an authenticated session
/// before it sends one. The core answers on its single event handler, so the owner of that handler must forward every
/// event to `handle(_:)`; it returns `true` when the event answered one of this client's operations. Operation
/// identifiers start at `firstOperationID`, a range no other client uses. Call `shutDown()` before closing the core so
/// no completion is left waiting.
final class CoreRetentionControlClient: RetentionControlCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 47

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the retention control client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private struct Pending {
        let isGatedRead: Bool
        let fail: @Sendable (CoreFailure) -> Void
        let deliver: @Sendable (Data) -> Void
    }

    private let core: CoreHandle
    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreRetentionControlClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    init(core: CoreHandle, gate: PrivateReadGate) {
        self.core = core
        self.gate = gate
    }

    func readStatus(completion: @escaping @Sendable (Result<RetentionStatusAnswer, CoreFailure>) -> Void) {
        start(isGatedRead: true, completion: completion) { operationID in
            try self.gate.startRetentionStatus(operationID: operationID)
        }
    }

    func readAudio(
        itemID: String, completion: @escaping @Sendable (Result<RetentionAudioAnswer, CoreFailure>) -> Void
    ) {
        start(isGatedRead: true, completion: completion) { operationID in
            try self.gate.startRetentionAudio(operationID: operationID, itemID: itemID)
        }
    }

    func submit(
        _ command: RetentionControlCommand,
        completion: @escaping @Sendable (Result<RetentionControlAnswer, CoreFailure>) -> Void
    ) {
        start(isGatedRead: false, completion: completion) { operationID in
            try self.core.startRetentionControl(operationID: operationID, command: command)
        }
    }

    private func start<Answer: Decodable & Sendable>(
        isGatedRead: Bool,
        completion: @escaping @Sendable (Result<Answer, CoreFailure>) -> Void,
        _ begin: (UInt64) throws -> Void
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = Pending(
            isGatedRead: isGatedRead,
            fail: { completion(.failure($0)) },
            deliver: { data in
                if let answer = try? JSONDecoder().decode(Answer.self, from: data) {
                    completion(.success(answer))
                } else {
                    completion(.failure(Self.unreadableAnswer))
                }
            })
        lock.unlock()
        do {
            try begin(operationID)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            if let denial = error as? PrivateReadDenied {
                removed?.fail(CoreItemStatusReader.failure(for: denial))
            } else {
                removed?.fail((error as? CoreFailure) ?? Self.unreadableAnswer)
            }
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
                waiting.fail(CoreItemStatusReader.failure(for: denial))
                return true
            }
        }
        switch event.outcome {
        case .failure(let failure):
            waiting.fail(failure)
        case .success(let data):
            waiting.deliver(data)
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
        for operation in waiting.values {
            operation.fail(Self.shutDownFailure)
        }
    }
}
