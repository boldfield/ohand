import Foundation
import OhandCoreC

/// Storage condition of the audio volume as `ohand_core_start_audio_retention` takes it. Raw values are the core's.
public enum CoreRetentionPressure: String, Equatable, Sendable {
    case normal
    case low
}

/// One audio-retention request. `sweep` and `authorizeRemoval` only read. `beginRemoval` decides inside a store-wide
/// fence and, when it approves, keeps the store's write lock held (a lease) while native code removes the one approved
/// file; `releaseAudioRemoval` ends the lease. Nothing can commit between the approval and the removal.
public enum AudioRetentionCommand: Equatable, Sendable {
    /// `periodSeconds` 0 selects the core's default retention period.
    case sweep(pressure: CoreRetentionPressure, periodSeconds: UInt64)
    case authorizeRemoval(itemID: String, audioReference: String, periodSeconds: UInt64)
    case beginRemoval(itemID: String, audioReference: String, periodSeconds: UInt64)

    fileprivate var request: Request {
        switch self {
        case .sweep(let pressure, let periodSeconds):
            return Request(command: "sweep", pressure: pressure.rawValue, periodSeconds: periodSeconds)
        case .authorizeRemoval(let itemID, let audioReference, let periodSeconds):
            return Request(
                command: "authorize_removal", itemID: itemID, audioReference: audioReference,
                periodSeconds: periodSeconds)
        case .beginRemoval(let itemID, let audioReference, let periodSeconds):
            return Request(
                command: "begin_removal", itemID: itemID, audioReference: audioReference,
                periodSeconds: periodSeconds)
        }
    }

    fileprivate struct Request: Encodable {
        var command: String
        var pressure: String?
        var itemID: String?
        var audioReference: String?
        var periodSeconds: UInt64

        enum CodingKeys: String, CodingKey {
            case command
            case pressure
            case itemID = "item_id"
            case audioReference = "audio_reference"
            case periodSeconds = "period_seconds"
        }
    }
}

/// Where one item's audio stands, as the core reports it. `kind` is `retained`, `expired` or `unresolved`; the other
/// fields are present for the kinds that carry them. Timestamps are the core's RFC 3339 text.
public struct CoreRetentionState: Decodable, Equatable, Sendable {
    public let kind: String
    public let expiresAt: String?
    public let expiredAt: String?
    public let reason: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case expiresAt = "expires_at"
        case expiredAt = "expired_at"
        case reason
    }
}

/// The core's cleanup intent. `kind` is `none` or `remove_audio`.
public struct CoreRetentionCleanup: Decodable, Equatable, Sendable {
    public let kind: String
    public let audioReference: String?
    public let expiredAt: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case audioReference = "audio_reference"
        case expiredAt = "expired_at"
    }
}

/// One live item's audio retention status.
public struct CoreRetentionAssessment: Decodable, Equatable, Sendable {
    public let itemID: String
    public let audioReference: String
    public let state: CoreRetentionState
    public let soleSource: Bool
    public let options: [String]
    public let cleanup: CoreRetentionCleanup
    public let storagePressureNotice: String?

    enum CodingKeys: String, CodingKey {
        case itemID = "item_id"
        case audioReference = "audio_reference"
        case state
        case soleSource = "sole_source"
        case options
        case cleanup
        case storagePressureNotice = "storage_pressure_notice"
    }
}

/// The answer to `sweep`.
public struct CoreRetentionSweepAnswer: Decodable, Equatable, Sendable {
    public let assessments: [CoreRetentionAssessment]
}

/// The answer to `authorizeRemoval` and `beginRemoval`. `decision` is `approved` or `denied`; a denial carries its
/// `reason`. An approved `beginRemoval` also carries the lease that keeps the store's write lock held: `leaseID` and
/// the milliseconds `leaseMillis` it lasts at most.
public struct CoreRetentionDecisionAnswer: Decodable, Equatable, Sendable {
    public let decision: String
    public let expiredAt: String?
    public let reason: String?
    public let expiresAt: String?
    public let unresolvedReason: String?
    public let leaseID: UInt64?
    public let leaseMillis: UInt64?

    enum CodingKeys: String, CodingKey {
        case decision
        case expiredAt = "expired_at"
        case reason
        case expiresAt = "expires_at"
        case unresolvedReason = "unresolved_reason"
        case leaseID = "lease_id"
        case leaseMillis = "lease_millis"
    }
}

extension CoreHandle {
    /// Queues one audio-retention command. The outcome event decodes as the answer type for that command, or is a
    /// failure. Throws when the request could not be queued, in which case nothing ran.
    public func startAudioRetention(operationID: UInt64, command: AudioRetentionCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.request))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_audio_retention(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }

    /// Ends a removal lease granted by `beginRemoval`, once the file effect is done. Not queued behind the worker, which
    /// may be waiting on the lease's own lock. Throws `lease_expired` when the fence may have lapsed before this call:
    /// the removal must then not be reported as done.
    public func releaseAudioRemoval(leaseIdentifier: UInt64) throws {
        _ = try consumeCoreResult(ohand_core_release_audio_removal(handleIdentifier, leaseIdentifier))
    }
}
