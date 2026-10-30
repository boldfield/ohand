import Foundation
import OhandCoreC

/// One user control over an item's lifecycle or suggestions, as `ohand_core_start_item_lifecycle_control` takes it.
/// `expectedRevision` is the item revision the user was looking at; the core refuses the command when the item has
/// moved on. `commandID` makes the request idempotent: asking again with the same ID and the same control reports the
/// first outcome and writes nothing.
public struct ItemLifecycleControlCommand: Equatable, Sendable {
    public enum Control: String, Equatable, Sendable, CaseIterable {
        case complete = "complete_item"
        case cancel = "cancel_item"
        case notNow = "not_now"
        case stopSuggesting = "stop_suggesting"
        case reopen = "reopen"
    }

    public let commandID: String
    public let itemID: String
    public let expectedRevision: Int
    public let control: Control

    public init(commandID: String, itemID: String, expectedRevision: Int, control: Control) {
        self.commandID = commandID
        self.itemID = itemID
        self.expectedRevision = expectedRevision
        self.control = control
    }

    fileprivate struct Request: Encodable {
        let command: String
        let commandID: String
        let itemID: String
        let expectedRevision: Int

        enum CodingKeys: String, CodingKey {
            case command
            case commandID = "command_id"
            case itemID = "item_id"
            case expectedRevision = "expected_revision"
        }
    }

    fileprivate var request: Request {
        Request(command: control.rawValue, commandID: commandID, itemID: itemID, expectedRevision: expectedRevision)
    }
}

/// The core's answer to every item lifecycle control. `outcome` is the core's own word:
/// - `applied`: the control is stored (`itemID`, `revision`, `replayed` are present);
/// - `stale_revision`: nothing changed; `currentRevision` is the revision the item has now;
/// - `command_conflict`: the command ID was already used for a different control; nothing changed;
/// - `item_deleted`: the item is deleted; nothing changed;
/// - `item_ended`: the item was already completed or cancelled (`lifecycle` says which); nothing changed.
/// A reopen answers `reopened` (`itemID`, `revision`, `replayed`, and `formerReminderInstant`, the stored instant of the
/// reminder the ending cancelled, if it had one) or `already_open` (the item was active; nothing changed). Stale,
/// conflicting and deleted reopens answer as above.
/// An unknown item is a failure with code `not_found`, not an answer.
public struct ItemLifecycleControlAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let revision: Int?
    public let replayed: Bool?
    public let currentRevision: Int?
    public let lifecycle: String?
    public let formerReminderInstant: String?

    public init(
        outcome: String, itemID: String? = nil, revision: Int? = nil, replayed: Bool? = nil,
        currentRevision: Int? = nil, lifecycle: String? = nil, formerReminderInstant: String? = nil
    ) {
        self.outcome = outcome
        self.itemID = itemID
        self.revision = revision
        self.replayed = replayed
        self.currentRevision = currentRevision
        self.lifecycle = lifecycle
        self.formerReminderInstant = formerReminderInstant
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case revision
        case replayed
        case currentRevision = "current_revision"
        case lifecycle
        case formerReminderInstant = "former_reminder_instant"
    }
}

extension CoreHandle {
    /// Queues one item lifecycle control. The outcome event decodes as `ItemLifecycleControlAnswer`, or is a failure.
    /// Throws when the request could not be queued, in which case nothing ran.
    public func startItemLifecycleControl(operationID: UInt64, command: ItemLifecycleControlCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_item_lifecycle_control(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
