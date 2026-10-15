import Foundation
import OhAndCoreBridge

/// The core's item-detail read as native code uses it (`core/src/ffi/item_detail.rs`,
/// `ohand_core_start_item_detail`).
protocol ItemDetailCore: Sendable {
    /// Reads one item and calls `completion` once with the core's answer, or with the gate's refusal.
    func read(itemID: String, completion: @escaping @Sendable (Result<CoreItemDetail, CoreFailure>) -> Void)
}

/// Runs item-detail reads over the private read gate: the production `ItemDetailCore`. A capture-only session never
/// reaches the core, and an answer that arrives after a relock is dropped undecoded. The core answers on its single
/// event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when the event
/// answered one of this client's operations. Operation IDs start at `firstOperationID`, far from the ranges the other
/// services use. Call `shutDown()` before closing the core so no completion is left waiting.
final class CoreItemDetailClient: ItemDetailCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 51

    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreItemDetailClient.firstOperationID
    private var pending: [UInt64: @Sendable (Result<CoreItemDetail, CoreFailure>) -> Void] = [:]
    private var isShutDown = false

    init(gate: PrivateReadGate) {
        self.gate = gate
    }

    func read(itemID: String, completion: @escaping @Sendable (Result<CoreItemDetail, CoreFailure>) -> Void) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            completion(.failure(CoreFailure.handleClosed))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = completion
        lock.unlock()
        do {
            try gate.startItemDetail(operationID: operationID, itemID: itemID)
        } catch {
            lock.lock()
            let claimed = pending.removeValue(forKey: operationID)
            lock.unlock()
            if let denial = error as? PrivateReadDenied {
                claimed?(.failure(CoreItemStatusReader.failure(for: denial)))
            } else {
                claimed?(.failure((error as? CoreFailure) ?? CoreFailure.handleClosed))
            }
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let completion = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let completion else { return false }
        do {
            try gate.admit(operationID: event.operationID)
        } catch {
            let denial = (error as? PrivateReadDenied) ?? .notAGatedRead
            completion(.failure(CoreItemStatusReader.failure(for: denial)))
            return true
        }
        switch event.outcome {
        case .failure(let failure):
            completion(.failure(failure))
        case .success:
            if let detail = try? event.decode(CoreItemDetail.self) {
                completion(.success(detail))
            } else {
                completion(
                    .failure(
                        CoreFailure(
                            errorClass: .permanent, code: "unreadable_event",
                            message: "the core answer could not be read")))
            }
        }
        return true
    }

    /// Fails every operation still waiting and refuses later ones. Idempotent.
    func shutDown() {
        lock.lock()
        isShutDown = true
        let abandoned = pending
        pending = [:]
        lock.unlock()
        for completion in abandoned.values {
            completion(.failure(CoreFailure.handleClosed))
        }
    }
}
