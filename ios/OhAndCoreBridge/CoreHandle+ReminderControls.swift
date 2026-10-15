import Foundation
import OhandCoreC

/// Which read of an item's reminder to start. The raw values are the core's command names.
public enum ReminderReadKind: String, Equatable, Sendable {
    case reminder = "get_reminder"
    case history = "reminder_history"
}

/// One user command on an item's reminder, as `ohand_core_start_reminder_control` takes it. `expectedRevision` is the
/// item revision and `expectedReminderVersion` the reminder's `stateVersion` (0 when it has none) that the user was
/// looking at; the core refuses the command when either has moved on. `commandID` makes the request idempotent.
public struct ReminderControlCommand: Equatable, Sendable {
    public enum Change: Equatable, Sendable {
        /// Remind at exactly `instant`, floored to a whole second, in the zone named by an IANA identifier.
        case correctTime(instant: Date, timeZoneID: String)
        case cancel
    }

    public let commandID: String
    public let itemID: String
    public let expectedRevision: Int
    public let expectedReminderVersion: Int
    public let change: Change

    public init(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int, change: Change
    ) {
        self.commandID = commandID
        self.itemID = itemID
        self.expectedRevision = expectedRevision
        self.expectedReminderVersion = expectedReminderVersion
        self.change = change
    }

    fileprivate var request: Request {
        var request = Request(
            command: "cancel", commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
            expectedReminderVersion: expectedReminderVersion)
        if case .correctTime(let instant, let timeZoneID) = change {
            request.command = "correct_time"
            request.instant = ReminderInstant.string(from: instant)
            request.timezoneID = timeZoneID
        }
        return request
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        var command: String
        let commandID: String
        let itemID: String
        let expectedRevision: Int
        let expectedReminderVersion: Int
        var instant: String?
        var timezoneID: String?

        enum CodingKeys: String, CodingKey {
            case command
            case commandID = "command_id"
            case itemID = "item_id"
            case expectedRevision = "expected_revision"
            case expectedReminderVersion = "expected_reminder_version"
            case instant
            case timezoneID = "timezone_id"
        }
    }
}

/// The whole-second RFC 3339 form the core takes and reports reminder instants in.
public enum ReminderInstant {
    /// `instant` floored to a whole second, in UTC (`2026-10-15T09:30:00Z`).
    public static func string(from instant: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        formatter.timeZone = TimeZone(identifier: "UTC")
        return formatter.string(from: Date(timeIntervalSince1970: instant.timeIntervalSince1970.rounded(.down)))
    }

    public static func date(from string: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: string)
    }
}

/// An item's reminder as the core stores it. States use the core's names (the ones the item status reports):
/// `requestState` is `not_requested` | `resolved` | `not_scheduled_yet` | `unsupported_recurrence` | `unschedulable` |
/// `cancelled`, with `unschedulableReason` set for `unschedulable`.
public struct ReminderRecordAnswer: Decodable, Equatable, Sendable {
    public let reminderID: String
    /// The request as the user or the model made it.
    public let sourcePhrase: String?
    public let requestState: String
    public let unschedulableReason: String?
    public let scheduleState: String
    public let deliveryState: String
    public let acknowledgmentState: String
    /// The instant the request resolved to (RFC 3339), kept for an elapsed or unschedulable request.
    public let resolvedInstant: String?
    public let timezoneID: String?
    public let ambiguityReason: String?
    public let unsupportedReason: String?
    public let scheduleGeneration: Int
    /// The version a later edit or cancel must name as `expectedReminderVersion`.
    public let stateVersion: Int
    public let updatedAt: String

    public var resolvedDate: Date? { resolvedInstant.flatMap(ReminderInstant.date(from:)) }

    enum CodingKeys: String, CodingKey {
        case reminderID = "reminder_id"
        case sourcePhrase = "source_phrase"
        case requestState = "request_state"
        case unschedulableReason = "unschedulable_reason"
        case scheduleState = "schedule_state"
        case deliveryState = "delivery_state"
        case acknowledgmentState = "acknowledgment_state"
        case resolvedInstant = "resolved_instant"
        case timezoneID = "timezone_id"
        case ambiguityReason = "ambiguity_reason"
        case unsupportedReason = "unsupported_reason"
        case scheduleGeneration = "schedule_generation"
        case stateVersion = "state_version"
        case updatedAt = "updated_at"
    }

    public init(
        reminderID: String, sourcePhrase: String? = nil, requestState: String, unschedulableReason: String? = nil,
        scheduleState: String, deliveryState: String = "unknown", acknowledgmentState: String = "not_acknowledged",
        resolvedInstant: String? = nil, timezoneID: String? = nil, ambiguityReason: String? = nil,
        unsupportedReason: String? = nil, scheduleGeneration: Int = 0, stateVersion: Int, updatedAt: String
    ) {
        self.reminderID = reminderID
        self.sourcePhrase = sourcePhrase
        self.requestState = requestState
        self.unschedulableReason = unschedulableReason
        self.scheduleState = scheduleState
        self.deliveryState = deliveryState
        self.acknowledgmentState = acknowledgmentState
        self.resolvedInstant = resolvedInstant
        self.timezoneID = timezoneID
        self.ambiguityReason = ambiguityReason
        self.unsupportedReason = unsupportedReason
        self.scheduleGeneration = scheduleGeneration
        self.stateVersion = stateVersion
        self.updatedAt = updatedAt
    }
}

/// The answer to a `get_reminder` read: the item's revision (what an edit names as `expectedRevision`) and its reminder,
/// or `nil` when it has none.
public struct ReminderReadAnswer: Decodable, Equatable, Sendable {
    public let itemID: String
    public let itemRevision: Int
    public let itemType: String?
    public let lifecycleState: String
    public let reminder: ReminderRecordAnswer?

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case itemRevision = "item_revision"
        case itemType = "item_type"
        case lifecycleState = "lifecycle_state"
        case reminder
    }

    public init(
        itemID: String, itemRevision: Int, itemType: String?, lifecycleState: String, reminder: ReminderRecordAnswer?
    ) {
        self.itemID = itemID
        self.itemRevision = itemRevision
        self.itemType = itemType
        self.lifecycleState = lifecycleState
        self.reminder = reminder
    }
}

/// The standing of the reminder's live generation in a history answer.
public struct ReminderHistorySummaryAnswer: Decodable, Equatable, Sendable {
    public let requestState: String
    public let unschedulableReason: String?
    public let scheduleState: String
    public let dueAt: String?
    public let timezoneID: String?
    /// `not_applicable` | `upcoming` | `elapsed`.
    public let due: String
    public let deliveryState: String
    public let acknowledgmentState: String
    /// True when the due time passed and nothing records a delivery, so "it fired" must not be claimed.
    public let elapsedWithoutDeliveryEvidence: Bool

    enum CodingKeys: String, CodingKey {
        case requestState = "request_state"
        case unschedulableReason = "unschedulable_reason"
        case scheduleState = "schedule_state"
        case dueAt = "due_at"
        case timezoneID = "timezone_id"
        case due
        case deliveryState = "delivery_state"
        case acknowledgmentState = "acknowledgment_state"
        case elapsedWithoutDeliveryEvidence = "elapsed_without_delivery_evidence"
    }
}

/// One recorded fact about the reminder. `kind` is `time_requested` | `repeat_requested` | `not_schedulable` |
/// `cancelled` | `delivered` | `opened` | `acknowledged`; the other fields are present where the kind has them.
public struct ReminderHistoryEntryAnswer: Decodable, Equatable, Sendable {
    public let occurredAt: String
    public let kind: String
    public let generation: Int?
    public let dueAt: String?
    /// For `time_requested`: whether the notification was installed (`confirmed`, `attempted_unconfirmed`, ...).
    public let installation: String?
    public let reason: String?
    public let requestState: String?
    public let unschedulableReason: String?
    public let nativeCancel: String?

    enum CodingKeys: String, CodingKey {
        case occurredAt = "occurred_at"
        case kind
        case generation
        case dueAt = "due_at"
        case installation
        case reason
        case requestState = "request_state"
        case unschedulableReason = "unschedulable_reason"
        case nativeCancel = "native_cancel"
    }
}

/// The answer to a `reminder_history` read: the newest entries first, bounded by the core. `totalEntries` counts all
/// recorded entries and `truncated` says some older ones are not included. An item without a reminder answers a `nil`
/// `reminderID` and `summary` and no entries.
public struct ReminderHistoryAnswer: Decodable, Equatable, Sendable {
    public let itemID: String
    public let reminderID: String?
    public let summary: ReminderHistorySummaryAnswer?
    public let entries: [ReminderHistoryEntryAnswer]
    public let totalEntries: Int
    public let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case reminderID = "reminder_id"
        case summary
        case entries
        case totalEntries = "total_entries"
        case truncated
    }
}

/// The core's answer to every reminder control command. `outcome` is the core's own word:
/// - `corrected` / `cancelled`: stored (`reminder`, `replayed`);
/// - `no_reminder`: a cancel with nothing to cancel;
/// - `stale_revision` (`currentRevision`) / `stale_reminder_version` (`currentReminderVersion`);
/// - `time_in_past`: the instant is not in the future, so nothing was stored;
/// - `command_conflict`, `item_deleted`, `item_not_active` (`lifecycleState`), `item_cannot_carry_reminder`.
/// Every outcome except `corrected` and `cancelled` changed nothing. An unknown item is a failure with code `not_found`.
public struct ReminderControlAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let reminder: ReminderRecordAnswer?
    public let replayed: Bool?
    public let currentRevision: Int?
    public let currentReminderVersion: Int?
    public let lifecycleState: String?

    public init(
        outcome: String, itemID: String? = nil, reminder: ReminderRecordAnswer? = nil, replayed: Bool? = nil,
        currentRevision: Int? = nil, currentReminderVersion: Int? = nil, lifecycleState: String? = nil
    ) {
        self.outcome = outcome
        self.itemID = itemID
        self.reminder = reminder
        self.replayed = replayed
        self.currentRevision = currentRevision
        self.currentReminderVersion = currentReminderVersion
        self.lifecycleState = lifecycleState
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case reminder
        case replayed
        case currentRevision = "current_revision"
        case currentReminderVersion = "current_reminder_version"
        case lifecycleState = "lifecycle_state"
    }
}

extension CoreHandle {
    /// Queues a read of one item's reminder or reminder history. The core answers only for an item inside
    /// `readScopes` (a non-empty list); any other item is a `not_found` failure. App code must call this only through
    /// the private read gate. The outcome event decodes as `ReminderReadAnswer` or `ReminderHistoryAnswer`.
    public func startReminderRead(
        operationID: UInt64, itemID: String, kind: ReminderReadKind, readScopes: [CoreExportScope]
    ) throws {
        struct Request: Encodable {
            let command: String
            let itemID: String
            let readScopes: [String]

            enum CodingKeys: String, CodingKey {
                case command
                case itemID = "item_id"
                case readScopes = "read_scopes"
            }
        }
        let requestBytes = Array(
            try JSONEncoder().encode(
                Request(command: kind.rawValue, itemID: itemID, readScopes: readScopes.map(\.rawValue))))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_reminder_read(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues one user command on an item's reminder. The outcome event decodes as `ReminderControlAnswer`, or is a
    /// failure. Throws when the request could not be queued, in which case nothing ran.
    public func startReminderControl(operationID: UInt64, command: ReminderControlCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_reminder_control(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
