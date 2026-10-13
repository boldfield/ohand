import Foundation
import OhandCoreC

/// One notification-action request. The raw `command` values are the core's `ohand_core_start_notification_action`
/// commands. `notificationID` is the core-derived `reminder_id#generation`; `itemID`, when given, is the opaque target
/// the notification carried and must agree with the item the core stores for that reminder.
public enum NotificationActionCommand: Equatable, Sendable {
    public enum Evidence: String, Equatable, Sendable {
        case delivered
        case opened
    }

    case recordEvidence(notificationID: String, kind: Evidence, observedAtUnixMilliseconds: Int64, itemID: String?)
    case acknowledge(notificationID: String, itemID: String?)
    case complete(notificationID: String, itemID: String?)
    case resolve(notificationID: String)

    fileprivate var request: Request {
        switch self {
        case .recordEvidence(let notificationID, let kind, let observedAt, let itemID):
            return Request(
                command: "record_evidence", notificationID: notificationID, itemID: itemID, kind: kind.rawValue,
                observedAtUnixMilliseconds: observedAt)
        case .acknowledge(let notificationID, let itemID):
            return Request(command: "acknowledge", notificationID: notificationID, itemID: itemID)
        case .complete(let notificationID, let itemID):
            return Request(command: "complete", notificationID: notificationID, itemID: itemID)
        case .resolve(let notificationID):
            return Request(command: "resolve", notificationID: notificationID)
        }
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        let command: String
        let notificationID: String
        var itemID: String?
        var kind: String?
        var observedAtUnixMilliseconds: Int64?

        enum CodingKeys: String, CodingKey {
            case command
            case notificationID = "notification_id"
            case itemID = "item_id"
            case kind
            case observedAtUnixMilliseconds = "observed_at_unix_ms"
        }
    }
}

/// The answer to every notification-action command. `outcome` is the core's own word for what happened; outcomes that
/// mean "nothing was changed" (`unknown_notification`, `not_installed`, `item_mismatch`, `item_deleted`,
/// `future_observation`, `stale_notification`, `already_recorded`, `already_ended`) are answers, not failures.
/// `itemID`, `lifecycle` and `currentGeneration` are present only on a `resolve` answer with outcome `item`.
public struct NotificationActionAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let lifecycle: String?
    public let currentGeneration: Bool?

    public init(outcome: String, itemID: String? = nil, lifecycle: String? = nil, currentGeneration: Bool? = nil) {
        self.outcome = outcome
        self.itemID = itemID
        self.lifecycle = lifecycle
        self.currentGeneration = currentGeneration
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case lifecycle
        case currentGeneration = "current_generation"
    }
}

extension CoreHandle {
    /// Queues one notification-action command. The outcome event decodes as `NotificationActionAnswer`, or is a
    /// failure. Throws when the request could not be queued, in which case nothing ran.
    public func startNotificationAction(operationID: UInt64, command: NotificationActionCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_notification_action(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
