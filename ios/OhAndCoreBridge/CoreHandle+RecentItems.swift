import Foundation
import OhandCoreC

/// Whether a recent item was typed or spoken. Raw values are the core's labels.
public enum CoreRecentItemSourceKind: String, Decodable, Equatable, Sendable {
    case text
    case voice
}

/// One row of the recent-captures read.
public struct CoreRecentItem: Decodable, Equatable, Sendable {
    public let itemID: String
    /// The item's current revision.
    public let revision: Int
    /// The item's type, or `nil` while it has not been classified.
    public let itemType: String?
    /// The interpreted title when the item has one, otherwise its current source text; `nil` for a voice capture that
    /// has no transcript yet.
    public let displayText: String?
    /// The capture instant as stored (RFC 3339).
    public let capturedAt: String
    public let sourceKind: CoreRecentItemSourceKind

    public init(
        itemID: String, revision: Int, itemType: String?, displayText: String?, capturedAt: String,
        sourceKind: CoreRecentItemSourceKind
    ) {
        self.itemID = itemID
        self.revision = revision
        self.itemType = itemType
        self.displayText = displayText
        self.capturedAt = capturedAt
        self.sourceKind = sourceKind
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case revision
        case itemType = "item_type"
        case displayText = "display_text"
        case capturedAt = "captured_at"
        case sourceKind = "source_kind"
    }
}

/// The outcome event of a recent-captures read: a page, newest capture first.
public struct CoreRecentItemsPage: Decodable, Equatable, Sendable {
    public let items: [CoreRecentItem]
    /// Pass this as the next request's cursor; `nil` on the last page.
    public let nextCursor: String?

    public init(items: [CoreRecentItem], nextCursor: String?) {
        self.items = items
        self.nextCursor = nextCursor
    }

    enum CodingKeys: String, CodingKey {
        case items
        case nextCursor = "next_cursor"
    }
}

extension CoreHandle {
    /// Queues the read of the most recent captures. `readScopes` is what the caller is authorized to read; the core never
    /// widens it, it has no default, and an empty list is refused as `invalid_request`. Native authenticates the session
    /// before calling and passes only that session's scopes. `limit` is 1 through 100 (30 when `nil`) and `cursor` is the
    /// previous page's `nextCursor`. The outcome event decodes as `CoreRecentItemsPage`, or is a normalized failure.
    /// Throws `invalid_request` when the request is out of bounds and when it could not be queued; in both cases nothing
    /// ran.
    public func startRecentItems(
        operationID: UInt64, readScopes: [CoreExportScope], limit: Int?, cursor: String?
    ) throws {
        let requestBytes = Array(
            try JSONEncoder().encode(Request(readScopes: readScopes, limit: limit, cursor: cursor)))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_recent_items(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    private struct Request: Encodable {
        let readScopes: [CoreExportScope]
        let limit: Int?
        let cursor: String?

        enum CodingKeys: String, CodingKey {
            case readScopes = "read_scopes"
            case limit
            case cursor
        }
    }
}
