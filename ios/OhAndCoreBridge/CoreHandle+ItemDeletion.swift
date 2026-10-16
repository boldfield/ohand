import Foundation
import OhandCoreC

/// One request to the core's per-item deletion, as `ohand_core_start_item_deletion` takes it.
public enum ItemDeletionCommand: Equatable, Sendable {
    /// Records the deletion intent. `expectedRevision` is the item revision the user was looking at; the core refuses
    /// the deletion when the item has moved on. Asking again with the revision the deletion was first applied against
    /// reports the same intent and writes nothing.
    case markDeleted(itemID: String, expectedRevision: Int)
    /// Reads how far the item's deletion has progressed; writes nothing.
    case progress(itemID: String)

    fileprivate var request: Request {
        switch self {
        case .markDeleted(let itemID, let expectedRevision):
            return Request(command: "mark_deleted", itemID: itemID, expectedRevision: expectedRevision)
        case .progress(let itemID):
            return Request(command: "progress", itemID: itemID, expectedRevision: nil)
        }
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        let command: String
        let itemID: String
        let expectedRevision: Int?

        enum CodingKeys: String, CodingKey {
            case command
            case itemID = "item_id"
            case expectedRevision = "expected_revision"
        }
    }
}

/// The core's report of an item's deletion progress. `state` is `not_requested`, `in_progress`, `failed` or `complete`;
/// `pending` and `failed` name the `remove_audio`, `clear_ingress` and `cancel_notifications` effects in each state.
public struct ItemDeletionProgressPayload: Decodable, Equatable, Sendable {
    public let state: String
    public let pending: [String]
    public let failed: [String]

    public init(state: String, pending: [String] = [], failed: [String] = []) {
        self.state = state
        self.pending = pending
        self.failed = failed
    }
}

/// The core's answer to every item deletion request. `outcome` is the core's own word:
/// - `deletion_recorded`: the intent is durable (`itemID`, `deletionRevision`, `replayed`, `progress` are present);
/// - `stale_revision`: nothing changed; `currentRevision` is the revision the item has now;
/// - `progress`: the answer to a progress read (`itemID`, `progress` are present).
/// An unknown item is a failure with code `not_found`, not an answer.
public struct ItemDeletionAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let itemID: String?
    public let deletionRevision: Int?
    public let replayed: Bool?
    public let currentRevision: Int?
    public let progress: ItemDeletionProgressPayload?

    public init(
        outcome: String, itemID: String? = nil, deletionRevision: Int? = nil, replayed: Bool? = nil,
        currentRevision: Int? = nil, progress: ItemDeletionProgressPayload? = nil
    ) {
        self.outcome = outcome
        self.itemID = itemID
        self.deletionRevision = deletionRevision
        self.replayed = replayed
        self.currentRevision = currentRevision
        self.progress = progress
    }

    enum CodingKeys: String, CodingKey {
        case outcome
        case itemID = "item_id"
        case deletionRevision = "deletion_revision"
        case replayed
        case currentRevision = "current_revision"
        case progress
    }
}

extension CoreHandle {
    /// Queues one item deletion request. The outcome event decodes as `ItemDeletionAnswer`, or is a failure. Throws when
    /// the request could not be queued, in which case nothing ran.
    public func startItemDeletion(operationID: UInt64, command: ItemDeletionCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_item_deletion(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
