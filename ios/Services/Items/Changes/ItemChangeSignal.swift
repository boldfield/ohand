import Foundation
import OhAndCoreBridge

/// What changed. It names the item and nothing else: no text, type, state or revision, so a subscriber must read
/// whatever it shows again through the normal gated reads.
public enum ItemChange: Equatable, Sendable {
    /// One item was deleted, corrected, completed, cancelled, quieted or had its reminder changed.
    case item(String)
    /// A reset completed: every item is gone.
    case allItems
}

/// Where services say that something they stored changed an item.
protocol ItemChangePublishing: Sendable {
    func publish(_ change: ItemChange)
}

/// The in-process signal that an item changed, so screens that kept a list can read it again.
///
/// * **Published after the core committed.** Services publish once the core reported the outcome as stored (a replay
///   counts); a refusal, a stale revision or a failure never publishes.
/// * **Carries no content.** See `ItemChange`.
/// * **Delivered on the main thread.** A publish from the main thread reaches subscribers before it returns; from any
///   other thread it is queued to the main queue, in publish order.
public final class ItemChangeSignal: ItemChangePublishing, @unchecked Sendable {
    private let lock = NSLock()
    private var subscribers: [UUID: @Sendable (ItemChange) -> Void] = [:]

    public init() {}

    /// Calls `handler` for every later change. Returns the closure that removes the registration.
    public func observe(_ handler: @escaping @Sendable (ItemChange) -> Void) -> () -> Void {
        let token = UUID()
        lock.lock()
        subscribers[token] = handler
        lock.unlock()
        return { [weak self] in
            guard let self else { return }
            self.lock.lock()
            self.subscribers[token] = nil
            self.lock.unlock()
        }
    }

    func publish(_ change: ItemChange) {
        if Thread.isMainThread {
            deliver(change)
        } else {
            DispatchQueue.main.async { [self] in deliver(change) }
        }
    }

    private func deliver(_ change: ItemChange) {
        lock.lock()
        let current = Array(subscribers.values)
        lock.unlock()
        current.forEach { $0(change) }
    }
}

/// Wraps the notification-action core so a completion the core committed reaches the signal like any other change.
/// Acknowledgement and evidence change no item, so they pass through unchanged.
struct ChangePublishingNotificationActionCore: NotificationActionCore {
    let base: NotificationActionCore
    let changes: ItemChangePublishing

    func recordEvidence(
        notificationID: String, kind: NotificationActionCommand.Evidence, observedAt: Date, itemID: String?
    ) async throws -> NotificationActionAnswer {
        try await base.recordEvidence(
            notificationID: notificationID, kind: kind, observedAt: observedAt, itemID: itemID)
    }

    func acknowledge(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        try await base.acknowledge(notificationID: notificationID, itemID: itemID)
    }

    func complete(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        let answer = try await base.complete(notificationID: notificationID, itemID: itemID)
        if answer.outcome == "completed" {
            // The answer does not name the item; when the tap did not either, any item may have changed.
            changes.publish(itemID.map(ItemChange.item) ?? .allItems)
        }
        return answer
    }

    func resolve(notificationID: String) async throws -> NotificationActionAnswer {
        try await base.resolve(notificationID: notificationID)
    }
}
