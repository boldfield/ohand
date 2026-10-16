import Foundation
import OhandCoreC

/// Which read of the audio retention setting to start.
public enum RetentionReadKind: Equatable, Sendable {
    /// The stored policy with the live items whose audio is their only source and so never expires.
    case status
    /// One item's audio reference and whether that audio is the item's only source.
    case audio(itemID: String)
}

/// One user command on audio retention, as `ohand_core_start_retention_control` takes it.
public enum RetentionControlCommand: Equatable, Sendable {
    /// Stores the retention period in seconds. The core accepts one second through 3650 days.
    case setPeriod(seconds: Int64)
    /// Re-queues on-device transcription for the item's audio.
    case retryTranscription(itemID: String)

    fileprivate var request: Request {
        switch self {
        case .setPeriod(let seconds):
            return Request(command: "set_period", periodSeconds: seconds)
        case .retryTranscription(let itemID):
            return Request(command: "retry_transcription", itemID: itemID)
        }
    }

    /// Absent optional fields are omitted from the JSON, which is what the core's strict decoder expects.
    fileprivate struct Request: Encodable {
        let command: String
        var itemID: String?
        var periodSeconds: Int64?

        enum CodingKeys: String, CodingKey {
            case command
            case itemID = "item_id"
            case periodSeconds = "period_seconds"
        }
    }
}

/// The stored retention period. `isDefault` is true until the user stores one.
public struct RetentionPolicyAnswer: Decodable, Equatable, Sendable {
    public let periodSeconds: Int64
    public let isDefault: Bool

    enum CodingKeys: String, CodingKey {
        case periodSeconds = "period_seconds"
        case isDefault = "is_default"
    }

    public init(periodSeconds: Int64, isDefault: Bool) {
        self.periodSeconds = periodSeconds
        self.isDefault = isDefault
    }
}

/// One live item whose audio is its only source, so it never expires. `reason` is an `AudioUnresolvedReason` raw
/// value and `options` are the explicit actions the core offers (`retry_transcription`, `delete_audio`).
public struct RetentionUnresolvedAnswer: Decodable, Equatable, Sendable {
    public let itemID: String
    public let capturedAt: String
    public let reason: String
    public let options: [String]

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case capturedAt = "captured_at"
        case reason
        case options
    }

    public init(itemID: String, capturedAt: String, reason: String, options: [String]) {
        self.itemID = itemID
        self.capturedAt = capturedAt
        self.reason = reason
        self.options = options
    }
}

/// The answer to a `retention_status` read.
public struct RetentionStatusAnswer: Decodable, Equatable, Sendable {
    public let policy: RetentionPolicyAnswer
    public let unresolved: [RetentionUnresolvedAnswer]

    public init(policy: RetentionPolicyAnswer, unresolved: [RetentionUnresolvedAnswer]) {
        self.policy = policy
        self.unresolved = unresolved
    }
}

/// The answer to a `get_audio` read: `audioReference` is `nil` when the item holds no audio.
public struct RetentionAudioAnswer: Decodable, Equatable, Sendable {
    public let itemID: String
    public let audioReference: String?
    public let soleSource: Bool

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case audioReference = "audio_reference"
        case soleSource = "sole_source"
    }

    public init(itemID: String, audioReference: String?, soleSource: Bool) {
        self.itemID = itemID
        self.audioReference = audioReference
        self.soleSource = soleSource
    }
}

/// The core's answer to every retention control command. `outcome` is the core's own word:
/// - `stored`: `periodSeconds` is the period now in force;
/// - `requeued`: `jobID` is the transcription job queued;
/// - `item_deleted`, `audio_removed`, `job_in_flight`, and `not_retryable` (`transcriptionState`): nothing changed.
/// An unknown item is a failure with code `not_found`.
public struct RetentionControlAnswer: Decodable, Equatable, Sendable {
    public let outcome: String
    public let periodSeconds: Int64?
    public let jobID: String?
    public let transcriptionState: String?

    enum CodingKeys: String, CodingKey {
        case outcome
        case periodSeconds = "period_seconds"
        case jobID = "job_id"
        case transcriptionState = "transcription_state"
    }

    public init(
        outcome: String, periodSeconds: Int64? = nil, jobID: String? = nil, transcriptionState: String? = nil
    ) {
        self.outcome = outcome
        self.periodSeconds = periodSeconds
        self.jobID = jobID
        self.transcriptionState = transcriptionState
    }
}

extension CoreHandle {
    /// Queues a read of the retention setting or of one item's audio. The core answers only for items inside
    /// `readScopes` (a non-empty list). App code must call this only through the private read gate. The outcome event
    /// decodes as `RetentionStatusAnswer` or `RetentionAudioAnswer`.
    public func startRetentionRead(
        operationID: UInt64, kind: RetentionReadKind, readScopes: [CoreExportScope]
    ) throws {
        struct Request: Encodable {
            let command: String
            let itemID: String?
            let readScopes: [String]

            enum CodingKeys: String, CodingKey {
                case command
                case itemID = "item_id"
                case readScopes = "read_scopes"
            }
        }
        let request: Request
        switch kind {
        case .status:
            request = Request(command: "retention_status", itemID: nil, readScopes: readScopes.map(\.rawValue))
        case .audio(let itemID):
            request = Request(command: "get_audio", itemID: itemID, readScopes: readScopes.map(\.rawValue))
        }
        let requestBytes = Array(try JSONEncoder().encode(request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_retention_read(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Queues one user command on audio retention. The outcome event decodes as `RetentionControlAnswer`, or is a
    /// failure. Throws when the request could not be queued, in which case nothing ran.
    public func startRetentionControl(operationID: UInt64, command: RetentionControlCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_retention_control(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
