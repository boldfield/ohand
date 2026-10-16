import Foundation
import OhAndCoreBridge

/// The core's item deletions as native code uses them (`core/src/ffi/item_deletion.rs`,
/// `ohand_core_start_item_deletion`). The core owns the compare-and-set rule, the idempotency, the tombstone and the
/// durable cleanup rows; it never touches native files.
protocol ItemDeletionCore: Sendable {
    /// Queues `command` and calls `completion` once with the core's answer. Commands reach the core in call order.
    func submit(
        _ command: ItemDeletionCommand,
        completion: @escaping @Sendable (Result<ItemDeletionAnswer, CoreFailure>) -> Void)
}

/// Runs item deletions over a `CoreHandle`: the production `ItemDeletionCore`. A progress read reports state derived
/// from the item's stored history, so it goes only through the `PrivateReadGate`: a capture-only session never reaches
/// the core and an answer that arrives after a relock is dropped undecoded. Marking an item deleted is a write and goes
/// straight to the core; `ItemDeletionService` requires an authenticated session before it sends one, and its answer is
/// delivered even if the session relocked meanwhile. The core answers on its single event handler, so the owner of that
/// handler must forward every event to `handle(_:)`; it returns `true` when the event answered one of this client's
/// operations. Operation IDs start at `firstOperationID`, far from the ranges the other services use. Call `shutDown()`
/// before closing the core so no completion is left waiting.
public final class CoreItemDeletionClient: ItemDeletionCore, @unchecked Sendable {
    /// Sits above the reminder control range, which starts at `1 << 59`.
    public static let firstOperationID: UInt64 = (1 << 59) | (1 << 58)

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the item deletion client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private struct Pending {
        let isGatedRead: Bool
        let completion: @Sendable (Result<ItemDeletionAnswer, CoreFailure>) -> Void
    }

    private let core: CoreHandle
    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreItemDeletionClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    init(core: CoreHandle, gate: PrivateReadGate) {
        self.core = core
        self.gate = gate
    }

    func submit(
        _ command: ItemDeletionCommand,
        completion: @escaping @Sendable (Result<ItemDeletionAnswer, CoreFailure>) -> Void
    ) {
        let isGatedRead: Bool
        switch command {
        case .markDeleted: isGatedRead = false
        case .progress: isGatedRead = true
        }
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = Pending(isGatedRead: isGatedRead, completion: completion)
        lock.unlock()
        do {
            if isGatedRead {
                try gate.startDeletionProgress(operationID: operationID) {
                    try core.startItemDeletion(operationID: operationID, command: command)
                }
            } else {
                try core.startItemDeletion(operationID: operationID, command: command)
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

    /// Consumes the event if it answers one of this client's operations. The answer to a progress read is delivered
    /// only if the gate admits it; otherwise it is dropped without being decoded and the caller is told why.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
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
            if let answer = try? JSONDecoder().decode(ItemDeletionAnswer.self, from: data) {
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
        for operation in waiting.values {
            operation.completion(.failure(Self.shutDownFailure))
        }
    }
}
