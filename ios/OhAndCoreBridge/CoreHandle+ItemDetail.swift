import Foundation
import OhandCoreC

/// The provenance of a transcript attached to an item, as the core records it.
public struct CoreItemDetailTranscript: Decodable, Equatable, Sendable {
    public let transcriptRecordID: String
    public let text: String
    public let recognizer: String
    public let detectedLanguage: String?
    public let confidence: Double?
    /// When the transcript was produced (RFC 3339).
    public let transcribedAt: String
    /// The item revision the transcript was attached at.
    public let attachedRevision: Int

    public init(
        transcriptRecordID: String, text: String, recognizer: String, detectedLanguage: String?, confidence: Double?,
        transcribedAt: String, attachedRevision: Int
    ) {
        self.transcriptRecordID = transcriptRecordID
        self.text = text
        self.recognizer = recognizer
        self.detectedLanguage = detectedLanguage
        self.confidence = confidence
        self.transcribedAt = transcribedAt
        self.attachedRevision = attachedRevision
    }

    enum CodingKeys: String, CodingKey {
        case transcriptRecordID = "transcript_record_id"
        case text
        case recognizer
        case detectedLanguage = "detected_language"
        case confidence
        case transcribedAt = "transcribed_at"
        case attachedRevision = "attached_revision"
    }
}

/// What a correction event changed.
public struct CoreItemDetailCorrection: Decodable, Equatable, Sendable {
    /// `text`, `type` or `session_topic`.
    public let kind: String
    public let oldValue: String?
    public let newValue: String

    public init(kind: String, oldValue: String?, newValue: String) {
        self.kind = kind
        self.oldValue = oldValue
        self.newValue = newValue
    }

    enum CodingKeys: String, CodingKey {
        case kind
        case oldValue = "old_value"
        case newValue = "new_value"
    }
}

/// One entry of an item's history.
public struct CoreItemDetailEvent: Decodable, Equatable, Sendable {
    public let eventID: String
    /// The item revision the event was applied to.
    public let revision: Int
    public let eventType: String
    /// When the event happened (RFC 3339).
    public let happenedAt: String
    /// Set for a correction event.
    public let correction: CoreItemDetailCorrection?
    /// `not_now` or `stop_suggesting` for a suggestion-control event.
    public let suggestionControl: String?

    public init(
        eventID: String, revision: Int, eventType: String, happenedAt: String,
        correction: CoreItemDetailCorrection? = nil, suggestionControl: String? = nil
    ) {
        self.eventID = eventID
        self.revision = revision
        self.eventType = eventType
        self.happenedAt = happenedAt
        self.correction = correction
        self.suggestionControl = suggestionControl
    }

    enum CodingKeys: String, CodingKey {
        case eventID = "event_id"
        case revision
        case eventType = "event_type"
        case happenedAt = "happened_at"
        case correction
        case suggestionControl = "suggestion_control"
    }
}

/// The outcome event of an item-detail read.
public struct CoreItemDetail: Decodable, Equatable, Sendable {
    public let itemID: String
    public let captureID: String
    public let revision: Int
    public let itemType: String?
    /// `personal` or `work`.
    public let scope: String
    public let sessionTopic: String?
    public let lifecycleState: String
    /// The capture instant as stored (RFC 3339).
    public let capturedAt: String
    /// The current text to show; `nil` for an audio capture with no transcript yet.
    public let text: String?
    public let textBasis: CoreRetrievalTextBasis
    /// The capture's own text; `nil` for an audio capture.
    public let originalText: String?
    /// The user's latest text correction; present only when `textBasis` is `corrected`.
    public let correctedText: String?
    public let audioReference: String?
    public let transcript: CoreItemDetailTranscript?
    /// Newest first, bounded by the core.
    public let history: [CoreItemDetailEvent]
    /// Whether older events were left out of `history`.
    public let historyTruncated: Bool

    public init(
        itemID: String, captureID: String, revision: Int, itemType: String?, scope: String, sessionTopic: String?,
        lifecycleState: String, capturedAt: String, text: String?, textBasis: CoreRetrievalTextBasis,
        originalText: String?, correctedText: String?, audioReference: String?,
        transcript: CoreItemDetailTranscript?, history: [CoreItemDetailEvent], historyTruncated: Bool
    ) {
        self.itemID = itemID
        self.captureID = captureID
        self.revision = revision
        self.itemType = itemType
        self.scope = scope
        self.sessionTopic = sessionTopic
        self.lifecycleState = lifecycleState
        self.capturedAt = capturedAt
        self.text = text
        self.textBasis = textBasis
        self.originalText = originalText
        self.correctedText = correctedText
        self.audioReference = audioReference
        self.transcript = transcript
        self.history = history
        self.historyTruncated = historyTruncated
    }

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case captureID = "capture_id"
        case revision
        case itemType = "item_type"
        case scope
        case sessionTopic = "session_topic"
        case lifecycleState = "lifecycle_state"
        case capturedAt = "captured_at"
        case text
        case textBasis = "text_basis"
        case originalText = "original_text"
        case correctedText = "corrected_text"
        case audioReference = "audio_reference"
        case transcript
        case history
        case historyTruncated = "history_truncated"
    }
}

extension CoreHandle {
    /// Queues the read of one item. `readScopes` is what the caller is authorized to read; the core never widens it, it
    /// has no default, and an empty list is refused as `invalid_request`. Native authenticates the session before
    /// calling and passes only that session's scopes. The outcome event decodes as `CoreItemDetail`, or is a
    /// normalized failure: an unknown, deleted or out-of-scope item is the same fixed `not_found`, and no failure echoes
    /// the identifier. Throws `invalid_request` when the request is out of bounds (an empty or over-long identifier, no
    /// read scope) and when it could not be queued; in both cases nothing ran.
    public func startItemDetail(operationID: UInt64, itemID: String, readScopes: [CoreExportScope]) throws {
        let requestBytes = Array(try JSONEncoder().encode(Request(itemID: itemID, readScopes: readScopes)))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_item_detail(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    private struct Request: Encodable {
        let itemID: String
        let readScopes: [CoreExportScope]

        enum CodingKeys: String, CodingKey {
            case itemID = "item_id"
            case readScopes = "read_scopes"
        }
    }
}
