import Foundation
import OhAndCoreBridge

/// The core's recent-captures read as native code uses it (`core/src/ffi/recent_items.rs`,
/// `ohand_core_start_recent_items`).
protocol RecentItemsCore: Sendable {
    /// Reads one page and calls `completion` once with the core's answer, or with the gate's refusal.
    func read(
        limit: Int?, cursor: String?,
        completion: @escaping @Sendable (Result<CoreRecentItemsPage, CoreFailure>) -> Void)
}

/// Runs recent-captures reads over the private read gate: the production `RecentItemsCore`. A capture-only session
/// never reaches the core, and an answer that arrives after a relock is dropped undecoded. The core answers on its
/// single event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when
/// the event answered one of this client's operations. Call `shutDown()` before closing the core so no completion is
/// left waiting.
final class CoreRecentItemsClient: RecentItemsCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = (1 << 43) | (1 << 41)

    private let gate: PrivateReadGate
    private let lock = NSLock()
    private var nextOperationID = CoreRecentItemsClient.firstOperationID
    private var pending: [UInt64: @Sendable (Result<CoreRecentItemsPage, CoreFailure>) -> Void] = [:]
    private var isShutDown = false

    init(gate: PrivateReadGate) {
        self.gate = gate
    }

    func read(
        limit: Int?, cursor: String?,
        completion: @escaping @Sendable (Result<CoreRecentItemsPage, CoreFailure>) -> Void
    ) {
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
            try gate.startRecentItems(operationID: operationID, limit: limit, cursor: cursor)
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
            if let page = try? event.decode(CoreRecentItemsPage.self) {
                completion(.success(page))
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
