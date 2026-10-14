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
    /// Claims, for the latest reset generation, every reference in `credentialReferences` that no profile outside
    /// `boundProfileVersions` holds. Storage then refuses any profile that would take a claimed reference until the
    /// claims are released. An existing claim set is returned as it is.
    case claimCredentials(credentialReferences: [String], boundProfileVersions: [String])
    case credentialClaims
    case releaseCredentialClaims

    /// One JSON field of a command request.
    fileprivate enum Field: Encodable {
        case text(String)
        case list([String])

        func encode(to encoder: Encoder) throws {
            var container = encoder.singleValueContainer()
            switch self {
            case .text(let value): try container.encode(value)
            case .list(let values): try container.encode(values)
            }
        }
    }

    fileprivate var fields: [String: Field] {
        switch self {
        case .start: return ["command": .text("start")]
        case .advance: return ["command": .text("advance")]
        case .status: return ["command": .text("status")]
        case .currentGeneration: return ["command": .text("current_generation")]
        case .itemGeneration(let itemID): return ["command": .text("item_generation"), "item_id": .text(itemID)]
        case .profileCredentials: return ["command": .text("profile_credentials")]
        case .revokeProfile(let profileVersion):
            return ["command": .text("revoke_profile"), "profile_version": .text(profileVersion)]
        case .claimCredentials(let credentialReferences, let boundProfileVersions):
            return [
                "command": .text("claim_credentials"),
                "credential_refs": .list(credentialReferences),
                "bound_profile_versions": .list(boundProfileVersions),
            ]
        case .credentialClaims: return ["command": .text("credential_claims")]
        case .releaseCredentialClaims: return ["command": .text("release_credential_claims")]
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

/// The answer to `claimCredentials`: the persisted claim set, sorted.
public struct ResetCredentialsClaimedAnswer: Decodable, Equatable, Sendable {
    public let claimed: [String]
}

/// The answer to `credentialClaims`: the persisted claim set, sorted.
public struct ResetCredentialClaimsAnswer: Decodable, Equatable, Sendable {
    public let claims: [String]
}

/// The answer to `releaseCredentialClaims`: how many claims were released.
public struct ResetCredentialClaimsReleasedAnswer: Decodable, Equatable, Sendable {
    public let released: Int
}

extension CoreHandle {
    /// Queues one reset command. The outcome event decodes as the answer type for that command, or is a failure;
    /// `reset_profile_refused` means the profile version is unknown and nothing was revoked. Throws when the request
    /// could not be queued, in which case nothing ran. `reset_claim_refused` means no reset generation has started, so
    /// nothing was claimed.
    public func startReset(operationID: UInt64, command: ResetCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.fields))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_reset(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
