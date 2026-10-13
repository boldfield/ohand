import Foundation
import OhandCoreC

/// One reset request. The raw `command` values are the core's `ohand_core_start_reset` commands.
public enum ResetCommand: Equatable, Sendable {
    case start
    case advance
    case status
    case currentGeneration
    case itemGeneration(itemID: String)
    case profileCredentials
    case revokeProfile(profileVersion: String)

    fileprivate var fields: [String: String] {
        switch self {
        case .start: return ["command": "start"]
        case .advance: return ["command": "advance"]
        case .status: return ["command": "status"]
        case .currentGeneration: return ["command": "current_generation"]
        case .itemGeneration(let itemID): return ["command": "item_generation", "item_id": itemID]
        case .profileCredentials: return ["command": "profile_credentials"]
        case .revokeProfile(let profileVersion):
            return ["command": "revoke_profile", "profile_version": profileVersion]
        }
    }
}

/// One reset generation as the core derives it from durable rows. `phase` is `pending` or `completed`.
public struct ResetProgressAnswer: Decodable, Equatable, Sendable {
    public let generation: Int64
    public let phase: String
    public let boundaryRowID: Int64
    public let itemsAwaitingTombstone: Int
    public let cleanupPending: Int
    public let cleanupFailed: Int

    enum CodingKeys: String, CodingKey {
        case generation
        case phase
        case boundaryRowID = "boundary_rowid"
        case itemsAwaitingTombstone = "items_awaiting_tombstone"
        case cleanupPending = "cleanup_pending"
        case cleanupFailed = "cleanup_failed"
    }
}

/// The answer to `start`, `advance` and `status`. `status` is `nil` when no reset has ever started (never for `start`).
public struct ResetStatusAnswer: Decodable, Equatable, Sendable {
    public let status: ResetProgressAnswer?
}

/// The answer to `currentGeneration` and `itemGeneration`. `generation` is `nil` for an unknown item.
public struct ResetGenerationAnswer: Decodable, Equatable, Sendable {
    public let generation: Int64?
}

/// The answer to `profileCredentials`.
public struct ResetProfileCredentialsAnswer: Decodable, Equatable, Sendable {
    public struct Row: Decodable, Equatable, Sendable {
        public let profileVersion: String
        public let credentialReference: String?
        public let revoked: Bool

        enum CodingKeys: String, CodingKey {
            case profileVersion = "profile_version"
            case credentialReference = "credential_ref"
            case revoked
        }
    }

    public let profiles: [Row]
}

/// The answer to `revokeProfile`.
public struct ResetProfileRevoked: Decodable, Equatable, Sendable {}

extension CoreHandle {
    /// Queues one reset command. The outcome event decodes as the answer type for that command, or is a failure;
    /// `reset_profile_refused` means the profile version is unknown and nothing was revoked. Throws when the request
    /// could not be queued, in which case nothing ran.
    public func startReset(operationID: UInt64, command: ResetCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.fields))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_reset(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
