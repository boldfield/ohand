import Foundation

/// The native cleanup effects the core records as durable work when an item is deleted. Raw values are the core's
/// `deletion_work.work_type` values.
public enum DeletionCleanupKind: String, CaseIterable, Equatable, Sendable {
    case removeAudio = "remove_audio"
    case clearIngress = "clear_ingress"
    case cancelNotifications = "cancel_notifications"
}

/// One pending cleanup row from the core. Only the kind and opaque identifiers cross the boundary.
public struct DeletionCleanupWork: Equatable, Sendable {
    public let workID: String
    public let itemID: String
    public let kind: DeletionCleanupKind

    public init(workID: String, itemID: String, kind: DeletionCleanupKind) {
        self.workID = workID
        self.itemID = itemID
        self.kind = kind
    }
}

/// The non-content identities the native layer needs to find an item's files. The audio reference stays on the
/// tombstoned capture until the core acknowledges that audio removal finished.
public struct DeletionCleanupTarget: Equatable, Sendable {
    public let captureID: String
    public let audioReference: String?

    public init(captureID: String, audioReference: String?) {
        self.captureID = captureID
        self.audioReference = audioReference
    }
}

/// The core's per-item deletion queue as native code uses it (`core/src/lifecycle/delete_intent`). The core remains the
/// authority: it reports deletion complete only after every row is durably completed, and it refuses to complete the
/// notification row while any cancel of the item's reminder is unrecorded. Native code therefore performs an effect
/// first and records completion afterwards, so an interruption between the two only repeats an idempotent effect.
public protocol DeletionCleanupCore: Sendable {
    /// Items that are deleted and still have at least one pending cleanup row.
    func itemsWithPendingCleanup() async throws -> [String]

    func pendingCleanup(itemID: String) async throws -> [DeletionCleanupWork]

    func cleanupTarget(itemID: String) async throws -> DeletionCleanupTarget?

    /// Every notification identifier, from any schedule generation, that the item's reminder may have installed.
    func reminderNotificationIdentifiers(itemID: String) async throws -> [NotificationIdentifier]

    /// Records that the OS no longer holds this notification.
    func recordNotificationCancelled(_ identifier: NotificationIdentifier) async throws

    func completeCleanup(workID: String) async throws

    /// Records that the effect can never succeed. The item stays tombstoned and unreadable.
    func failCleanup(workID: String) async throws
}

/// The two notification operations deletion needs. `NotificationBridge` is the production implementation.
public protocol DeletionNotificationCancelling: Sendable {
    /// Removes the pending request and returns only once the OS no longer lists it. Cancelling an absent request
    /// succeeds.
    func cancel(_ identifier: NotificationIdentifier) async throws
    func pendingNotifications() async throws -> [PendingNotification]
}

extension NotificationBridge: DeletionNotificationCancelling {}

/// Why a step was left pending. Fixed codes only: a step never reports identifiers, paths, content or error text.
public enum DeletionRetryReason: String, Equatable, Sendable {
    case coreUnavailable = "core_unavailable"
    case storageUnavailable = "storage_unavailable"
    case notificationUnavailable = "notification_unavailable"
    case notificationStillPending = "notification_still_pending"
    case cancelled = "cancelled"
}

/// Why a step can never succeed and was recorded as terminally failed.
public enum DeletionPermanentFailure: String, Equatable, Sendable {
    case unsafeCaptureIdentifier = "unsafe_capture_identifier"
    case audioReferenceUnresolvable = "audio_reference_unresolvable"
}

public enum DeletionStepOutcome: Equatable, Sendable {
    /// The effect is verified and the core recorded the row as completed.
    case completed
    /// The row stays pending; the next reconciliation repeats the effect.
    case retryLater(DeletionRetryReason)
    /// The core recorded the row as terminally failed.
    case failedPermanently(DeletionPermanentFailure)
}

public struct DeletionStepResult: Equatable, Sendable {
    public let kind: DeletionCleanupKind
    public let outcome: DeletionStepOutcome
}

public struct DeletionItemResult: Equatable, Sendable {
    public let itemID: String
    /// Set when the item's pending rows could not be read; nothing was attempted.
    public let listingFailure: DeletionRetryReason?
    public let steps: [DeletionStepResult]

    /// True when no pending row remains for the item.
    public var isSettled: Bool {
        listingFailure == nil && steps.allSatisfy { $0.outcome == .completed }
    }
}

public struct DeletionReconcileReport: Equatable, Sendable {
    /// Set when the core could not list the items with pending cleanup. That is "unknown", never "nothing to do".
    public let listingFailure: DeletionRetryReason?
    public let items: [DeletionItemResult]

    public var converged: Bool {
        listingFailure == nil && items.allSatisfy { $0.isSettled }
    }
}
