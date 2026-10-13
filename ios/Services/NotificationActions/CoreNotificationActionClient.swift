import Foundation
import OhAndCoreBridge

/// Carries notification-action commands to the Rust core over a `CoreHandle`: the production `NotificationActionCore`.
/// The core owns every rule (stale generations, deleted items, duplicate callbacks, the distinction between
/// acknowledging and completing); this client only carries each request and hands the answer back to the awaiting
/// caller.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Operation IDs start at
/// `firstOperationID`, far from the ranges the other services use. Call `shutDown()` before closing the core so no
/// caller is left waiting.
public final class CoreNotificationActionClient: NotificationActionCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 60

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the notification action client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreNotificationActionClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    // MARK: NotificationActionCore

    public func recordEvidence(
        notificationID: String,
        kind: NotificationActionCommand.Evidence,
        observedAt: Date,
        itemID: String?
    ) async throws -> NotificationActionAnswer {
        try await ask(
            .recordEvidence(
                notificationID: notificationID, kind: kind,
                observedAtUnixMilliseconds: Int64((observedAt.timeIntervalSince1970 * 1000).rounded()),
                itemID: itemID))
    }

    public func acknowledge(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        try await ask(.acknowledge(notificationID: notificationID, itemID: itemID))
    }

    public func complete(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        try await ask(.complete(notificationID: notificationID, itemID: itemID))
    }

    public func resolve(notificationID: String) async throws -> NotificationActionAnswer {
        try await ask(.resolve(notificationID: notificationID))
    }

    // MARK: Event routing

    /// Consumes the event if it answers one of this client's operations.
    @discardableResult
    public func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let continuation = pending.removeValue(forKey: event.operationID)
        lock.unlock()
        guard let continuation else { return false }
        continuation.resume(returning: event.outcome)
        return true
    }

    /// Fails every operation still waiting and refuses later ones. Idempotent.
    public func shutDown() {
        lock.lock()
        isShutDown = true
        let waiting = pending
        pending = [:]
        lock.unlock()
        for continuation in waiting.values {
            continuation.resume(returning: .failure(Self.shutDownFailure))
        }
    }

    // MARK: private

    private func ask(_ command: NotificationActionCommand) async throws -> NotificationActionAnswer {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            submit(command, continuation)
        }
        let data = try outcome.get()
        do {
            return try JSONDecoder().decode(NotificationActionAnswer.self, from: data)
        } catch {
            throw Self.unreadableAnswer
        }
    }

    private func submit(
        _ command: NotificationActionCommand,
        _ continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>
    ) {
        lock.lock()
        if isShutDown {
            lock.unlock()
            continuation.resume(returning: .failure(Self.shutDownFailure))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = continuation
        lock.unlock()
        do {
            try core.startNotificationAction(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
