import Foundation
import OhAndCoreBridge

/// Runs the L01 deletion queue over a `CoreHandle`: the production `DeletionCleanupCore`. The core owns every rule (what
/// is pending, when a row may complete, whether a notification cancel was recorded); this client only carries each
/// request and hands the answer back to the awaiting caller.
///
/// The core answers on its single event handler, so the owner of that handler must forward every event to `handle(_:)`;
/// it returns `true` when the event answered one of this client's operations. Operation IDs start at `firstOperationID`,
/// far from the ranges the other services use. Call `shutDown()` before closing the core so no caller is left waiting.
public final class CoreDeletionCleanupClient: DeletionCleanupCore, @unchecked Sendable {
    public static let firstOperationID: UInt64 = 1 << 52

    static let shutDownFailure = CoreFailure(
        errorClass: .cancelled, code: "client_shut_down", message: "the deletion cleanup client was shut down")
    static let unreadableAnswer = CoreFailure(
        errorClass: .permanent, code: "unreadable_event", message: "the core answer could not be read")

    private let core: CoreHandle
    private let lock = NSLock()
    private var nextOperationID = CoreDeletionCleanupClient.firstOperationID
    private var pending: [UInt64: CheckedContinuation<Result<Data, CoreFailure>, Never>] = [:]
    private var isShutDown = false

    public init(core: CoreHandle) {
        self.core = core
    }

    // MARK: DeletionCleanupCore

    public func itemsWithPendingCleanup() async throws -> [String] {
        try await ask(.pendingItems, as: DeletionPendingItemsAnswer.self).itemIDs
    }

    public func pendingCleanup(itemID: String) async throws -> [DeletionCleanupWork] {
        try Self.cleanupWork(from: try await ask(.pendingWork(itemID: itemID), as: DeletionPendingWorkAnswer.self))
    }

    private static func cleanupWork(from answer: DeletionPendingWorkAnswer) throws -> [DeletionCleanupWork] {
        try answer.work.map { row in
            guard let kind = DeletionCleanupKind(rawValue: row.workType) else { throw unreadableAnswer }
            return DeletionCleanupWork(workID: row.workID, itemID: row.itemID, kind: kind)
        }
    }

    public func terminallyFailedCleanup() async throws -> [DeletionCleanupWork] {
        try Self.cleanupWork(from: try await ask(.failedWork, as: DeletionPendingWorkAnswer.self))
    }

    public func cleanupTarget(itemID: String) async throws -> DeletionCleanupTarget? {
        let answer = try await ask(.cleanupTarget(itemID: itemID), as: DeletionCleanupTargetAnswer.self)
        return answer.target.map { DeletionCleanupTarget(captureID: $0.captureID, audioReference: $0.audioReference) }
    }

    public func reminderNotificationIdentifiers(itemID: String) async throws -> [NotificationIdentifier] {
        let answer = try await ask(.reminderNotifications(itemID: itemID), as: DeletionReminderNotificationsAnswer.self)
        // An identifier the native side cannot parse is never skipped: dropping it would let the notification row
        // complete without that request having been cancelled.
        return try answer.notificationIDs.map { rawValue in
            guard let identifier = NotificationIdentifier(rawValue: rawValue) else { throw Self.unreadableAnswer }
            return identifier
        }
    }

    public func recordNotificationCancelled(_ identifier: NotificationIdentifier) async throws {
        // `recorded == false` means the core never issued a cancel for this identifier (an earlier generation); there is
        // nothing to record and the OS no longer holding it is all that matters.
        _ = try await ask(
            .recordNotificationCancelled(notificationID: identifier.rawValue), as: DeletionNotificationCancelRecorded.self)
    }

    public func completeCleanup(workID: String) async throws {
        _ = try await ask(.completeWork(workID: workID), as: DeletionWorkRecorded.self)
    }

    public func failCleanup(workID: String) async throws {
        _ = try await ask(.failWork(workID: workID), as: DeletionWorkRecorded.self)
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

    private func ask<Answer: Decodable>(_ command: DeletionCleanupCommand, as type: Answer.Type) async throws -> Answer {
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<Result<Data, CoreFailure>, Never>) in
            submit(command, continuation)
        }
        let data = try outcome.get()
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw Self.unreadableAnswer
        }
    }

    private func submit(
        _ command: DeletionCleanupCommand,
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
            try core.startDeletionCleanup(operationID: operationID, command: command)
        } catch {
            lock.lock()
            let removed = pending.removeValue(forKey: operationID)
            lock.unlock()
            removed?.resume(returning: .failure((error as? CoreFailure) ?? Self.unreadableAnswer))
        }
    }
}
