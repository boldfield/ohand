import Foundation
import OhandCoreC

/// One deletion-cleanup request. The raw `command` values are the core's `ohand_core_start_deletion_cleanup` commands.
public enum DeletionCleanupCommand: Equatable, Sendable {
    case pendingItems
    case pendingWork(itemID: String)
    case cleanupTarget(itemID: String)
    case reminderNotifications(itemID: String)
    case recordNotificationCancelled(notificationID: String)
    case completeWork(workID: String)
    case failWork(workID: String)

    fileprivate var fields: [String: String] {
        switch self {
        case .pendingItems: return ["command": "pending_items"]
        case .pendingWork(let itemID): return ["command": "pending_work", "item_id": itemID]
        case .cleanupTarget(let itemID): return ["command": "cleanup_target", "item_id": itemID]
        case .reminderNotifications(let itemID): return ["command": "reminder_notifications", "item_id": itemID]
        case .recordNotificationCancelled(let notificationID):
            return ["command": "record_notification_cancelled", "notification_id": notificationID]
        case .completeWork(let workID): return ["command": "complete_work", "work_id": workID]
        case .failWork(let workID): return ["command": "fail_work", "work_id": workID]
        }
    }
}

/// The answer to `pendingItems`.
public struct DeletionPendingItemsAnswer: Decodable, Equatable, Sendable {
    public let itemIDs: [String]

    enum CodingKeys: String, CodingKey { case itemIDs = "item_ids" }
}

/// One pending cleanup row. `workType` is the core's `deletion_work.work_type` value.
public struct DeletionPendingWorkRow: Decodable, Equatable, Sendable {
    public let workID: String
    public let itemID: String
    public let workType: String

    enum CodingKeys: String, CodingKey {
        case workID = "work_id"
        case itemID = "item_id"
        case workType = "work_type"
    }
}

/// The answer to `pendingWork`.
public struct DeletionPendingWorkAnswer: Decodable, Equatable, Sendable {
    public let work: [DeletionPendingWorkRow]
}

/// The answer to `cleanupTarget`: the tombstoned capture's identity, or `nil` when the core has none for the item.
public struct DeletionCleanupTargetAnswer: Decodable, Equatable, Sendable {
    public struct Target: Decodable, Equatable, Sendable {
        public let captureID: String
        public let audioReference: String?

        enum CodingKeys: String, CodingKey {
            case captureID = "capture_id"
            case audioReference = "audio_reference"
        }
    }

    public let target: Target?
}

/// The answer to `reminderNotifications`.
public struct DeletionReminderNotificationsAnswer: Decodable, Equatable, Sendable {
    public let notificationIDs: [String]

    enum CodingKeys: String, CodingKey { case notificationIDs = "notification_ids" }
}

/// The answer to `recordNotificationCancelled`. `recorded` is false when the core issued no cancel for that identifier.
public struct DeletionNotificationCancelRecorded: Decodable, Equatable, Sendable {
    public let recorded: Bool
}

/// The answer to `completeWork` and `failWork`.
public struct DeletionWorkRecorded: Decodable, Equatable, Sendable {}

extension CoreHandle {
    /// Queues one deletion-cleanup command. The outcome event decodes as the answer type for that command, or is a
    /// failure: `deletion_work_refused` means the core will not record that transition now (for example completing the
    /// notification row while a cancel is unrecorded) and the row stays pending. Throws when the request could not be
    /// queued, in which case nothing ran.
    public func startDeletionCleanup(operationID: UInt64, command: DeletionCleanupCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.fields))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_deletion_cleanup(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
