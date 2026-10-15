import Foundation
import OhAndCoreC

/// The item types a user may correct to. The raw values are the core's `ItemType` names.
public enum ItemCorrectionItemType: String, Equatable, Sendable, CaseIterable {
    case broadIntention = "broad_intention"
    case note
    case idea
    case action
}

/// One user correction of an item, as `ohand_core_start_item_correction` takes it. `expectedRevision` is the item
/// revision the user was looking at; the core refuses the correction when the item has moved on. `commandID` makes the
/// request idempotent: asking again with the same ID and the same change reports the first outcome and writes nothing.
public struct ItemCorrectionCommand: Equatable, Sendable {
    public enum Change: Equatable, Sendable {
        case text(String)
        case itemType(ItemCorrectionItemType)
        case sessionTopic(String)
    }

    public let commandID: String
    public let itemID: String
    public let expectedRevision: Int
    public let change: Change

    public init(commandID: String, itemID: String, expectedRevision: Int, change: Change) {
        self.commandID = commandID
        self.itemID = itemID
        self.expectedRevision = expectedRevision
        self.change = change
    }

    fileprivate var request: Request {
        var request = Request(
            command: "", commandID: commandID, itemID: itemID, expectedRevision: expectedRevision)
        switch change {
        case .text(let text):
            request.command = "correct_text"
            request.text = text
        case .itemType(let itemType):
            request.command = "correct_type"
            request.itemType = itemType.rawValue
        case .sessionTopic(let topic):
            request.command = "correct_session_topic"
            request.sessionTopic = topic
        }
        return request
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        var command: String
        let commandID: String
        let itemID: String
        let expectedRevision: Int
        var text: String?
        var itemType: String?
        var sessionTopic: String?

        enum CodingKeys: String, CodingKey {
            case command
            case commandID = "command_id"
            case itemID = "item_id"
            case expectedRevision = "expected_revision"
            case text
            case itemType = "item_type"
            case sessionTopic = "session_topic"
        }
    }
}

/// The core's answer to every item correction. `outcome` is the core's own word:
/// - `corrected`: the correction is stored (`itemID`, `revision`, `replayed`, `reminderCancelled` are present);
/// - `stale_revision`: nothing changed; `currentRevision` is the revision the item has now;
/// - `command_conflict`: the command ID was already used for a different correction; nothing changed;
/// - `item_deleted`: the item is deleted; nothing changed.
/// An unknown item is a failure with code `not_found`, not an answer.
public struct ItemCorrectionAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let revision: Int?
    public let replayed: Bool?
    public let reminderCancelled: Bool?
    public let currentRevision: Int?

    public init(
        outcome: String, itemID: String? = nil, revision: Int? = nil, replayed: Bool? = nil,
        reminderCancelled: Bool? = nil, currentRevision: Int? = nil
    ) {
        self.outcome = outcome
        self.itemID = itemID
        self.revision = revision
        self.replayed = replayed
        self.reminderCancelled = reminderCancelled
        self.currentRevision = currentRevision
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case revision
        case replayed
        case reminderCancelled = "reminder_cancelled"
        case currentRevision = "current_revision"
    }
}

extension CoreHandle {
    /// Queues one item correction. The outcome event decodes as `ItemCorrectionAnswer`, or is a failure. Throws when
    /// the request could not be queued, in which case nothing ran.
    public func startItemCorrection(operationID: UInt64, command: ItemCorrectionCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_item_correction(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
