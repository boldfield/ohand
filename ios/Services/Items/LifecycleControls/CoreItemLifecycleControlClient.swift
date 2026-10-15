import Foundation
import OhAndCoreBridge

/// The core's item lifecycle controls as native code uses them (`core/src/ffi/item_lifecycle_controls.rs`,
/// `ohand_core_start_item_lifecycle_control`). The core owns the compare-and-set rule, the idempotency and the reminder
/// consequence of ending an item.
protocol ItemLifecycleControlCore: Sendable {
    /// Queues `command` and calls `completion` once with the core's answer. Commands reach the core in call order.
    func submit(
        _ command: ItemLifecycleControlCommand,
        completion: @escaping @Sendable (Result<ItemLifecycleControlAnswer, CoreFailure>) -> Void)
}

/// Runs item lifecycle controls over a `CoreHandle`: the production `ItemLifecycleControlCore`. The core answers on its
/// single event handler, so the owner of that handler must forward every event to `handle(_:)`; it returns `true` when
/// the event answered one of this client's operations. Operation IDs start at `firstOperationID`, far from the ranges
/// the other services use. Call `shutDown()` before closing the core so no completion is left waiting.
public final class CoreItemLifecycleControlClient: ItemLifecycleControlCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 49

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the item lifecycle control client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreItemLifecycleControlClient.firstOperationID
    private var pending: [UInt64: @Sendable (Result<ItemLifecycleControlAnswer, CoreFailure>) -> Void] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    func submit(
        _ command: ItemLifecycleControlCommand,
        completion: @escaping @Sendable (Result<ItemLifecycleControlAnswer, CoreFailure>) -> Void
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
        lock.unlock()
        do {
            try core.startItemLifecycleControl(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?(.failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let completion = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let completion else { return false }
        switch event.outcome {
        case .failure(let failure):
            completion(.failure(failure))
        case .success(let data):
            if let answer = try? JSONDecoder().decode(ItemLifecycleControlAnswer.self, from: data) {
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
        lock.unlock()
        for completion in waiting.values {
            completion(.failure(Self.shutDownFailure))
        }
    }
}
