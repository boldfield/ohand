import Foundation
import OhAndCoreBridge

/// The core's reminder reads and user commands as native code uses them (`core/src/ffi/reminder_controls.rs`,
/// `ohand_core_start_reminder_read` and `ohand_core_start_reminder_control`). The core owns the compare-and-set rules,
/// the idempotency and the refusal of a time that is not in the future.
protocol ReminderControlCore: Sendable {
    /// Reads the item's reminder through the private read gate and calls `completion` once.
    func readReminder(
        itemID: String, completion: @escaping @Sendable (Result<ReminderReadAnswer, CoreFailure>) -> Void)
    /// Reads the item's bounded reminder history through the private read gate and calls `completion` once.
    func readHistory(
        itemID: String, completion: @escaping @Sendable (Result<ReminderHistoryAnswer, CoreFailure>) -> Void)
    /// Queues `command` and calls `completion` once with the core's answer. Commands reach the core in call order.
    func submit(
        _ command: ReminderControlCommand,
        completion: @escaping @Sendable (Result<ReminderControlAnswer, CoreFailure>) -> Void)
}

/// Runs reminder reads and commands over a `CoreHandle`: the production `ReminderControlCore`. Reads go only through
/// the `PrivateReadGate`, so a capture-only session never reaches the core and an answer that arrives after a relock is
/// dropped undecoded. Commands go straight to the core; `ReminderControlService` requires an authenticated session
/// before it sends one. The core answers on its single event handler, so the owner of that handler must forward every
/// event to `handle(_:)`; it returns `true` when the event answered one of this client's operations. Operation IDs start
/// at `firstOperationID`, far from the ranges the other services use. Call `shutDown()` before closing the core so no
/// completion is left waiting.
final class CoreReminderControlClient: ReminderControlCore, @unchecked Sendable {
    static let firstOperationID: UInt64 = 1 << 55

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the reminder control client was shut down")
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
    private var nextOperationID = CoreReminderControlClient.firstOperationID
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    init(core: CoreHandle, gate: PrivateReadGate) {
        self.core = core
        self.gate = gate
    }

    func readReminder(
        itemID: String, completion: @escaping @Sendable (Result<ReminderReadAnswer, CoreFailure>) -> Void
    ) {
        start(isGatedRead: true, completion: completion) { operationID in
            try self.gate.startReminder(operationID: operationID, itemID: itemID)
        }
    }

    func readHistory(
        itemID: String, completion: @escaping @Sendable (Result<ReminderHistoryAnswer, CoreFailure>) -> Void
    ) {
        start(isGatedRead: true, completion: completion) { operationID in
            try self.gate.startReminderHistory(operationID: operationID, itemID: itemID)
        }
    }

    func submit(
        _ command: ReminderControlCommand,
        completion: @escaping @Sendable (Result<ReminderControlAnswer, CoreFailure>) -> Void
    ) {
        start(isGatedRead: false, completion: completion) { operationID in
            try self.core.startReminderControl(operationID: operationID, command: command)
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
