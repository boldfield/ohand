import Foundation
import OhAndCoreBridge

/// What changed. It names the item and nothing else: no text, type, state or revision, so a subscriber must read
/// whatever it shows again through the normal gated reads.
public enum ItemChange: Equatable, Sendable {
    /// One item was deleted, corrected, completed, cancelled, quieted or had its reminder changed.
    case item(String)
    /// A reset completed: every item is gone.
    case allItems

    /// True when this change may have changed the item named `itemID`.
    public func concerns(_ itemID: String) -> Bool {
        switch self {
        case .allItems: return true
        case .item(let changed): return changed.caseInsensitiveCompare(itemID) == .orderedSame
        }
    }
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
/// * **Also posted as `didChangeNotification`**, after the subscribers, for a screen built without access to the root
///   (an item's detail) that must read its own item again. The notification carries the same `ItemChange` and nothing
///   else, under `changeKey`.
public final class ItemChangeSignal: ItemChangePublishing, @unchecked Sendable {
    /// Posted on the main thread, with `object` the signal, for every change.
    public static let didChangeNotification = Notification.Name("OhAnd.ItemChangeSignal.didChange")
    /// The `userInfo` key holding the `ItemChange`.
    public static let changeKey = "change"

    private let lock = NSLock()
    private var subscribers: [UUID: @Sendable (ItemChange) -> Void] = [:]
    private let notificationCenter: NotificationCenter

    public init(notificationCenter: NotificationCenter = .default) {
        self.notificationCenter = notificationCenter
    }

    /// The change a `didChangeNotification` carries, or nil for any other notification.
    public static func change(in notification: Notification) -> ItemChange? {
        guard notification.name == didChangeNotification else { return nil }
        return notification.userInfo?[changeKey] as? ItemChange
    }

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
        notificationCenter.post(name: Self.didChangeNotification, object: self, userInfo: [Self.changeKey: change])
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
        // A tap whose payload lost its target is still completed by notification id; ask the core which item that is
        // before completing, so one completion announces one item and never the reset-only "all items". A failed lookup
        // throws before anything is completed, so the handler keeps the entry and both steps run again on retry; a
        // completion committed without its item named would otherwise never be announced, since the retry finds it ended.
        var changedItemID = itemID
        if changedItemID == nil {
            changedItemID = try await base.resolve(notificationID: notificationID).itemID
        }
        let answer = try await base.complete(notificationID: notificationID, itemID: itemID)
        if answer.outcome == "completed", let changedItemID {
            changes.publish(.item(changedItemID))
        }
        return answer
    }

    func resolve(notificationID: String) async throws -> NotificationActionAnswer {
        try await base.resolve(notificationID: notificationID)
    }
}
