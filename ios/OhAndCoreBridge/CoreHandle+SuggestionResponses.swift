import Foundation
import OhandCoreC

/// The responses a user can give to an offered item. The raw values are the core's names.
public enum SuggestionResponseKind: String, Equatable, Sendable, CaseIterable {
    case notNow = "not_now"
    case stopSuggesting = "stop_suggesting"
    case done
}

/// The item scopes an activation can be limited to. The raw values are the core's names.
public enum SuggestionScope: String, Equatable, Sendable {
    case personal
    case work
}

/// One command to `ohand_core_start_suggestion_response`. Both carry `read_authentication: "authenticated"`: build one
/// only after the session has been checked, because the core trusts that label and refuses `capture_only`.
public enum SuggestionResponseCommand: Equatable, Sendable {
    /// Opens the daily prompt named by `notificationID` and asks the core which item, if any, to offer now.
    case activatePrompt(notificationID: String, scope: SuggestionScope?)
    /// Applies `response` to the offered item. `expectedRevision` is the revision the offer showed; `commandID` makes
    /// the request idempotent.
    case respond(commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind)

    fileprivate var request: Request {
        switch self {
        case .activatePrompt(let notificationID, let scope):
            return Request(command: "activate_prompt", notificationID: notificationID, scope: scope?.rawValue)
        case .respond(let commandID, let itemID, let expectedRevision, let response):
            return Request(
                command: "respond", commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
                response: response.rawValue)
        }
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        let command: String
        let readAuthentication = "authenticated"
        var notificationID: String?
        var scope: String?
        var commandID: String?
        var itemID: String?
        var expectedRevision: Int?
        var response: String?

        enum CodingKeys: String, CodingKey {
            case command
            case readAuthentication = "read_authentication"
            case notificationID = "notification_id"
            case scope
            case commandID = "command_id"
            case itemID = "item_id"
            case expectedRevision = "expected_revision"
            case response
        }
    }
}

/// The core's answer to every suggestion command. `outcome` is the core's own word:
/// - `offer`: `itemID`, `revision`, `scope`, `selectionReason` and `responses` are present; `text` is the text that may
///   be displayed and is `nil` when the item has none (an untranscribed voice capture);
/// - `nothing_to_offer`: no item is eligible any more; the answer carries no count or backlog;
/// - `applied`: the response is stored (`replayed` says an earlier identical command had already stored it);
/// - `stale_revision`: nothing changed; `currentRevision` is the revision the item has now;
/// - `no_longer_current`: the item is deleted, completed, cancelled or gone; nothing changed;
/// - `command_conflict`: the command ID was already used for a different response; nothing changed.
public struct SuggestionResponseAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let revision: Int?
    public let text: String?
    public let scope: String?
    public let selectionReason: String?
    public let responses: [String]?
    public let replayed: Bool?
    public let currentRevision: Int?

    public init(
        outcome: String, itemID: String? = nil, revision: Int? = nil, text: String? = nil, scope: String? = nil,
        selectionReason: String? = nil, responses: [String]? = nil, replayed: Bool? = nil,
        currentRevision: Int? = nil
    ) {
        self.outcome = outcome
        self.itemID = itemID
        self.revision = revision
        self.text = text
        self.scope = scope
        self.selectionReason = selectionReason
        self.responses = responses
        self.replayed = replayed
        self.currentRevision = currentRevision
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case revision
        case text
        case scope
        case selectionReason = "selection_reason"
        case responses
        case replayed
        case currentRevision = "current_revision"
    }
}

extension CoreHandle {
    /// Queues one suggestion command. The outcome event decodes as `SuggestionResponseAnswer`, or is a failure. Throws
    /// when the request could not be queued, in which case nothing ran.
    public func startSuggestionResponse(operationID: UInt64, command: SuggestionResponseCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_suggestion_response(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
