import Foundation
import OhAndCoreBridge

/// Why a private read was refused. Content-free by construction: it never reports whether the requested record exists.
enum PrivateReadDenied: Error, Equatable {
    /// The session is capture-only, so the read was never sent to the core.
    case sessionNotAuthenticated
    /// The session relocked after the read was sent; the core's answer was dropped undelivered.
    case relockedBeforeDelivery
    /// The event does not belong to a read this gate issued.
    case notAGatedRead
}

/// The core operations that return stored history or state derived from it, including the store check, whose report
/// carries the stored capture count. Saving a capture is deliberately absent: capture never needs, and
/// never grants, read access. The core handle is adapted to this protocol where the app is assembled, so this module
/// does not depend on the bridge's C module.
protocol PrivateReadSource: AnyObject {
    func startStoreCheck(operationID: UInt64) throws
    func startGetCapture(operationID: UInt64, captureID: String) throws
    func startItemStatus(operationID: UInt64, itemID: String) throws
    func startRetrieval(operationID: UInt64, request: CoreRetrievalRequest) throws
    func startReminderRead(
        operationID: UInt64, itemID: String, kind: ReminderReadKind, readScopes: [CoreExportScope]) throws
    func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws
    func startRetentionRead(operationID: UInt64, kind: RetentionReadKind, readScopes: [CoreExportScope]) throws
    func startJobConfigurationRead(operationID: UInt64, command: JobConfigurationCommand) throws
    func startRecentItems(operationID: UInt64, readScopes: [CoreExportScope], limit: Int?, cursor: String?) throws
}

/// Raised by a read source that does not implement a read added after it was written.
struct PrivateReadNotSupportedBySource: Error, Equatable {}

extension PrivateReadSource {
    /// Default so that read sources written before the reminder reads existed (test doubles) keep conforming; the
    /// core-backed source in the composition root overrides it.
    func startReminderRead(
        operationID: UInt64, itemID: String, kind: ReminderReadKind, readScopes: [CoreExportScope]
    ) throws {
        throw PrivateReadNotSupportedBySource()
    }

    /// Default for the same reason; the core-backed source in the composition root overrides it.
    func startRetentionRead(operationID: UInt64, kind: RetentionReadKind, readScopes: [CoreExportScope]) throws {
        throw PrivateReadNotSupportedBySource()
    }

    func startJobConfigurationRead(operationID: UInt64, command: JobConfigurationCommand) throws {
        throw PrivateReadNotSupportedBySource()
    }

    func startRecentItems(operationID: UInt64, readScopes: [CoreExportScope], limit: Int?, cursor: String?) throws {
        throw PrivateReadNotSupportedBySource()
    }
}

/// The filters a module may put on a search or list. It has no way to name a read authorization: the gate states the
/// session's authorized scopes itself, and `scopes` can only narrow them.
public struct PrivateRetrievalFilters: Equatable, Sendable {
    /// Item scopes to restrict the retrieval to. `nil` means every scope the session is authorized for. A scope the
    /// session is not authorized for is dropped, and if none remain the core refuses the request as `invalid_request`.
    public var scopes: Set<CoreExportScope>?
    public var itemTypes: [String]
    public var lifecycleStates: [String]
    public var routeIDs: [String]
    public var sessionTopics: [String]
    public var includeNoSessionTopic: Bool
    public var requireSessionTopic: Bool
    /// RFC 3339 instants, both inclusive.
    public var capturedAfter: String?
    public var capturedBefore: String?
    /// The core accepts 1 through 100 and uses 25 when `nil`.
    public var limit: Int?
    public var offset: Int?

    public init(
        scopes: Set<CoreExportScope>? = nil,
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
        self.scopes = scopes
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
}

/// One item a retrieval returned. Show `text`, never `originalText`, as the current text.
public struct PrivateRetrievalHit: Decodable, Equatable, Sendable {
    public let itemID: String
    public let captureID: String
    /// `personal` or `work`.
    public let scope: String
    /// The item's current type, or `nil` while it has not been classified.
    public let itemType: String?
    public let text: String
    public let textBasis: CoreRetrievalTextBasis
    /// The text as captured; present only when `textBasis` is `corrected`.
    public let originalText: String?
    public let matched: CoreRetrievalMatchedText
    /// Unicode scalar offsets into `text`; `nil` for a list or when only a replaced original matched.
    public let matchSpan: CoreRetrievalMatchSpan?
    /// The capture instant as stored (RFC 3339).
    public let capturedAt: String

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

/// The answer to a gated search or list. Decode the core event as this type only after the event was admitted.
public struct PrivateRetrievalAnswer: Decodable, Equatable, Sendable {
    /// Items the session may read that satisfy the filters, before paging.
    public let totalAccessible: Int
    public let hits: [PrivateRetrievalHit]

    enum CodingKeys: String, CodingKey {
        case totalAccessible = "total_accessible"
        case hits
    }
}

/// Gates private reads on the session read scope, both when a read starts and when its answer arrives.
///
/// The core answers asynchronously, so checking the scope only at request time would let a read that began while
/// authenticated deliver stored content after a relock. Callers therefore pass every event for a gated operation
/// through `admit`, which approves delivery only while the authorization that issued the read is still current.
///
/// The gate is the only sanctioned path to the core's read verbs: `CoreHandle` itself stays ungated, so app assembly
/// must route every history, status and store-check read through this gate and must not call those verbs directly.
final class PrivateReadGate {
    private let session: ReadAuthenticationSession
    private let source: PrivateReadSource
    private let authorizedScopes: [CoreExportScope]
    private let lock = NSLock()
    private var pendingReads: [UInt64: ReadAuthorization] = [:]

    /// `authorizedScopes` is every item scope an authenticated session may read; a retrieval never asks the core for
    /// more.
    init(
        session: ReadAuthenticationSession, source: PrivateReadSource,
        authorizedScopes: [CoreExportScope] = [.personal, .work]
    ) {
        self.session = session
        self.source = source
        self.authorizedScopes = authorizedScopes
    }

    func startStoreCheck(operationID: UInt64) throws {
        try issue(operationID: operationID) {
            try source.startStoreCheck(operationID: operationID)
        }
    }

    func startGetCapture(operationID: UInt64, captureID: String) throws {
        try issue(operationID: operationID) {
            try source.startGetCapture(operationID: operationID, captureID: captureID)
        }
    }

    func startItemStatus(operationID: UInt64, itemID: String) throws {
        try issue(operationID: operationID) {
            try source.startItemStatus(operationID: operationID, itemID: itemID)
        }
    }

    func startReminder(operationID: UInt64, itemID: String) throws {
        try startReminderRead(operationID: operationID, itemID: itemID, kind: .reminder)
    }

    func startReminderHistory(operationID: UInt64, itemID: String) throws {
        try startReminderRead(operationID: operationID, itemID: itemID, kind: .history)
    }

    private func startReminderRead(operationID: UInt64, itemID: String, kind: ReminderReadKind) throws {
        try issue(operationID: operationID) {
            try source.startReminderRead(
                operationID: operationID, itemID: itemID, kind: kind, readScopes: authorizedScopes)
        }
    }

    func startItemDetail(operationID: UInt64, itemID: String) throws {
        try issue(operationID: operationID) {
            try source.startItemDetail(operationID: operationID, itemID: itemID, readScopes: authorizedScopes)
        }
    }

    func startRecentItems(operationID: UInt64, limit: Int?, cursor: String?) throws {
        try issue(operationID: operationID) {
            try source.startRecentItems(
                operationID: operationID, readScopes: authorizedScopes, limit: limit, cursor: cursor)
        }
    }

    func startRetentionStatus(operationID: UInt64) throws {
        try startRetentionRead(operationID: operationID, kind: .status)
    }

    func startRetentionAudio(operationID: UInt64, itemID: String) throws {
        try startRetentionRead(operationID: operationID, kind: .audio(itemID: itemID))
    }

    private func startRetentionRead(operationID: UInt64, kind: RetentionReadKind) throws {
        try issue(operationID: operationID) {
            try source.startRetentionRead(operationID: operationID, kind: kind, readScopes: authorizedScopes)
        }
    }

    func startJobsPinnedToProfile(operationID: UInt64, profileVersion: String, limit: Int?) throws {
        try issue(operationID: operationID) {
            try source.startJobConfigurationRead(
                operationID: operationID,
                command: .jobsPinnedToProfile(profileVersion: profileVersion, limit: limit))
        }
    }

    func startRequeueRecord(operationID: UInt64, jobID: String) throws {
        try issue(operationID: operationID) {
            try source.startJobConfigurationRead(operationID: operationID, command: .requeueRecord(jobID: jobID))
        }
    }

    func startSearch(operationID: UInt64, query: String, filters: PrivateRetrievalFilters) throws {
        try issue(operationID: operationID) {
            try source.startRetrieval(
                operationID: operationID, request: retrievalRequest(.search(query: query), filters: filters))
        }
    }

    func startList(operationID: UInt64, filters: PrivateRetrievalFilters) throws {
        try issue(operationID: operationID) {
            try source.startRetrieval(operationID: operationID, request: retrievalRequest(.list, filters: filters))
        }
    }

    /// Starts a prompt activation, which returns the stored text of the item it offers. `start` queues the core command.
    func startSuggestionActivation(operationID: UInt64, start: () throws -> Void) throws {
        try issue(operationID: operationID, start)
    }

    /// Starts an item's deletion progress read, which reports state derived from the item's stored history. `start`
    /// queues the core command.
    func startDeletionProgress(operationID: UInt64, start: () throws -> Void) throws {
        try issue(operationID: operationID, start)
    }

    /// Returns normally only if the read that `operationID` answers is still authorized; otherwise throws, and the
    /// caller must discard the answer, success or failure, without decoding or logging it. Each answer is admitted at
    /// most once.
    func admit(operationID: UInt64) throws {
        lock.lock()
        let authorization = pendingReads.removeValue(forKey: operationID)
        lock.unlock()
        guard let authorization else { throw PrivateReadDenied.notAGatedRead }
        guard session.isCurrent(authorization) else { throw PrivateReadDenied.relockedBeforeDelivery }
    }

    private func retrievalRequest(
        _ operation: CoreRetrievalRequest.Operation, filters: PrivateRetrievalFilters
    ) -> CoreRetrievalRequest {
        let readScopes = authorizedScopes.filter { filters.scopes?.contains($0) ?? true }
        return CoreRetrievalRequest(
            operation: operation,
            readScopes: readScopes,
            itemTypes: filters.itemTypes,
            lifecycleStates: filters.lifecycleStates,
            routeIDs: filters.routeIDs,
            sessionTopics: filters.sessionTopics,
            includeNoSessionTopic: filters.includeNoSessionTopic,
            requireSessionTopic: filters.requireSessionTopic,
            capturedAfter: filters.capturedAfter,
            capturedBefore: filters.capturedBefore,
            limit: filters.limit,
            offset: filters.offset)
    }

    private func issue(operationID: UInt64, _ start: () throws -> Void) throws {
        guard let authorization = session.currentAuthorization() else {
            throw PrivateReadDenied.sessionNotAuthenticated
        }
        lock.lock()
        pendingReads = pendingReads.filter { session.isCurrent($0.value) }
        pendingReads[operationID] = authorization
        lock.unlock()
        do {
            try start()
        } catch {
            lock.lock()
            pendingReads.removeValue(forKey: operationID)
            lock.unlock()
            throw error
        }
    }
}
