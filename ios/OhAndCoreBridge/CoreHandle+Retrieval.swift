import Foundation
import OhandCoreC

/// Which text a retrieval hit shows as current. Raw values are the core's labels.
public enum CoreRetrievalTextBasis: String, Decodable, Equatable, Sendable {
    case original
    case transcribed
    case corrected
}

/// Which stored text a search matched. Raw values are the core's labels.
public enum CoreRetrievalMatchedText: String, Decodable, Equatable, Sendable {
    case current
    /// Only the original text, since replaced by a correction, matched. Show `text`, never the original, as current.
    case supersededOriginal = "superseded_original"
}

/// Unicode scalar offsets `[start, end)` into a hit's `text`.
public struct CoreRetrievalMatchSpan: Decodable, Equatable, Sendable {
    public let start: Int
    public let end: Int

    public init(start: Int, end: Int) {
        self.start = start
        self.end = end
    }
}

/// One retrieval request for `ohand_core_start_retrieval`. `readScopes` is what the caller is authorized to read and
/// the core never widens it; it has no default, and an empty list is refused as `invalid_request`. Native
/// authenticates the session before calling and passes only that session's scopes.
public struct CoreRetrievalRequest: Encodable, Equatable, Sendable {
    /// `search` runs the full-text index; `list` returns every accessible item satisfying the filters.
    public enum Operation: Equatable, Sendable {
        case search(query: String)
        case list
    }

    public let operation: Operation
    public let readScopes: [CoreExportScope]
    public let itemTypes: [String]
    public let lifecycleStates: [String]
    public let routeIDs: [String]
    public let sessionTopics: [String]
    public let includeNoSessionTopic: Bool
    public let requireSessionTopic: Bool
    /// RFC 3339 instants, both inclusive.
    public let capturedAfter: String?
    public let capturedBefore: String?
    /// The core accepts 1 through 100 and uses 25 when `nil`.
    public let limit: Int?
    public let offset: Int?

    public init(
        operation: Operation,
        readScopes: [CoreExportScope],
        itemTypes: [String] = [],
        lifecycleStates: [String] = [],
        routeIDs: [String] = [],
        sessionTopics: [String] = [],
        includeNoSessionTopic: Bool = false,
        requireSessionTopic: Bool = false,
        capturedAfter: String? = nil,
        capturedBefore: String? = nil,
        limit: Int? = nil,
        offset: Int? = nil
    ) {
        self.operation = operation
        self.readScopes = readScopes
        self.itemTypes = itemTypes
        self.lifecycleStates = lifecycleStates
        self.routeIDs = routeIDs
        self.sessionTopics = sessionTopics
        self.includeNoSessionTopic = includeNoSessionTopic
        self.requireSessionTopic = requireSessionTopic
        self.capturedAfter = capturedAfter
        self.capturedBefore = capturedBefore
        self.limit = limit
        self.offset = offset
    }

    enum CodingKeys: String, CodingKey {
        case operation = "op"
        case query
        case readScopes = "read_scopes"
        case itemTypes = "item_types"
        case lifecycleStates = "lifecycle_states"
        case routeIDs = "route_ids"
        case sessionTopics = "session_topics"
        case includeNoSessionTopic = "include_no_session_topic"
        case requireSessionTopic = "require_session_topic"
        case capturedAfter = "captured_after"
        case capturedBefore = "captured_before"
        case limit
        case offset
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch operation {
        case .search(let query):
            try container.encode("search", forKey: .operation)
            try container.encode(query, forKey: .query)
        case .list:
            try container.encode("list", forKey: .operation)
        }
        try container.encode(readScopes, forKey: .readScopes)
        try container.encode(itemTypes, forKey: .itemTypes)
        try container.encode(lifecycleStates, forKey: .lifecycleStates)
        try container.encode(routeIDs, forKey: .routeIDs)
        try container.encode(sessionTopics, forKey: .sessionTopics)
        try container.encode(includeNoSessionTopic, forKey: .includeNoSessionTopic)
        try container.encode(requireSessionTopic, forKey: .requireSessionTopic)
        try container.encodeIfPresent(capturedAfter, forKey: .capturedAfter)
        try container.encodeIfPresent(capturedBefore, forKey: .capturedBefore)
        try container.encodeIfPresent(limit, forKey: .limit)
        try container.encodeIfPresent(offset, forKey: .offset)
    }
}

/// One item the caller may read.
public struct CoreRetrievalHit: Decodable, Equatable, Sendable {
    public let itemID: String
    public let captureID: String
    /// `personal` or `work`.
    public let scope: String
    /// The item's current type, or `nil` while it has not been classified.
    public let itemType: String?
    /// The current text to show and quote.
    public let text: String
    public let textBasis: CoreRetrievalTextBasis
    /// The text as captured; present only when `textBasis` is `corrected`.
    public let originalText: String?
    public let matched: CoreRetrievalMatchedText
    /// Where the first query word begins in `text`; `nil` for a list or when only the replaced original matched.
    public let matchSpan: CoreRetrievalMatchSpan?
    /// The capture instant as stored (RFC 3339).
    public let capturedAt: String

    public init(
        itemID: String, captureID: String, scope: String, text: String, textBasis: CoreRetrievalTextBasis,
        originalText: String?, matched: CoreRetrievalMatchedText, matchSpan: CoreRetrievalMatchSpan?,
        capturedAt: String, itemType: String? = nil
    ) {
        self.itemID = itemID
        self.captureID = captureID
        self.scope = scope
        self.itemType = itemType
        self.text = text
        self.textBasis = textBasis
        self.originalText = originalText
        self.matched = matched
        self.matchSpan = matchSpan
        self.capturedAt = capturedAt
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case captureID = "capture_id"
        case scope
        case itemType = "item_type"
        case text
        case textBasis = "text_basis"
        case originalText = "original_text"
        case matched
        case matchSpan = "match_span"
        case capturedAt = "captured_at"
    }
}

/// The outcome event of a retrieval. `totalAccessible` counts the items the caller may read that satisfy the filters,
/// before paging.
public struct CoreRetrievalAnswer: Decodable, Equatable, Sendable {
    public let totalAccessible: Int
    public let hits: [CoreRetrievalHit]

    public init(totalAccessible: Int, hits: [CoreRetrievalHit]) {
        self.totalAccessible = totalAccessible
        self.hits = hits
    }

    enum CodingKeys: String, CodingKey {
        case totalAccessible = "total_accessible"
        case hits
    }
}

extension CoreHandle {
    /// Queues one scoped retrieval. The outcome event decodes as `CoreRetrievalAnswer`, or is a normalized failure
    /// that never echoes the query or filters. Throws `invalid_request` when the request is out of bounds (no read
    /// scope, an unknown scope, a malformed date, a page outside 1 through 100) and when it could not be queued; in
    /// both cases nothing ran.
    public func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws {
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_retrieval(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
