import Foundation
import OhandCoreC

/// How a stored profile retries a failed call. Every value is bounded by the core.
public struct ProviderProfileRetryPolicy: Encodable, Equatable, Sendable {
    public let maxAttempts: Int
    public let initialBackoffMilliseconds: Int
    public let maxBackoffMilliseconds: Int

    public init(maxAttempts: Int, initialBackoffMilliseconds: Int, maxBackoffMilliseconds: Int) {
        self.maxAttempts = maxAttempts
        self.initialBackoffMilliseconds = initialBackoffMilliseconds
        self.maxBackoffMilliseconds = maxBackoffMilliseconds
    }

    enum CodingKeys: String, CodingKey {
        case maxAttempts = "max_attempts"
        case initialBackoffMilliseconds = "initial_backoff_ms"
        case maxBackoffMilliseconds = "max_backoff_ms"
    }
}

/// One provider profile as the caller states it. There is no destination field: the core derives the processing
/// destination from the protocol (or the self-hosted endpoint's origin), so a caller cannot name one. `protocol` is
/// `open_ai`, `anthropic` or `self_hosted`; `endpoint` is only for `self_hosted`.
public struct ProviderProfileDraft: Encodable, Equatable, Sendable {
    public let profileID: String
    public let providerProtocol: String
    public let endpoint: String?
    public let model: String
    public let credentialReference: String
    public let timeoutSeconds: Int
    public let retryPolicy: ProviderProfileRetryPolicy?
    public let capabilities: [String]

    public init(
        profileID: String,
        providerProtocol: String,
        endpoint: String? = nil,
        model: String,
        credentialReference: String,
        timeoutSeconds: Int,
        retryPolicy: ProviderProfileRetryPolicy? = nil,
        capabilities: [String] = ["text_interpretation"]
    ) {
        self.profileID = profileID
        self.providerProtocol = providerProtocol
        self.endpoint = endpoint
        self.model = model
        self.credentialReference = credentialReference
        self.timeoutSeconds = timeoutSeconds
        self.retryPolicy = retryPolicy
        self.capabilities = capabilities
    }

    enum CodingKeys: String, CodingKey {
        case profileID = "profile_id"
        case providerProtocol = "protocol"
        case endpoint
        case model
        case credentialReference = "credential_ref"
        case timeoutSeconds = "timeout_seconds"
        case retryPolicy = "retry_policy"
        case capabilities
    }
}

/// One provider-profile request. The raw `command` values are the core's `ohand_core_start_provider_profiles` commands.
public enum ProviderProfileCommand: Equatable, Sendable {
    case createProfile(ProviderProfileDraft)
    /// Points the capability's default at one unrevoked stored version. `capability` is `text_interpretation` or
    /// `review`.
    case setDefault(capability: String, profileVersion: String)
    /// Removes the capability's default. Allowed when none is set; queued jobs keep the version they were pinned to.
    case clearDefault(capability: String)
    case list
    /// Reads the credential reference bound to one stored version. Not part of the listing: only native credential
    /// management in `ios/Services` issues it, and the answer type lives there.
    case credentialReference(profileVersion: String)

    fileprivate struct Wire: Encodable {
        let command: String
        var profile: ProviderProfileDraft?
        var capability: String?
        var profileVersion: String?

        enum CodingKeys: String, CodingKey {
            case command
            case profile
            case capability
            case profileVersion = "profile_version"
        }
    }

    fileprivate var wire: Wire {
        switch self {
        case .createProfile(let draft): return Wire(command: "create_profile", profile: draft)
        case .setDefault(let capability, let profileVersion):
            return Wire(command: "set_default", capability: capability, profileVersion: profileVersion)
        case .clearDefault(let capability): return Wire(command: "clear_default", capability: capability)
        case .list: return Wire(command: "list")
        case .credentialReference(let profileVersion):
            return Wire(command: "credential_reference", profileVersion: profileVersion)
        }
    }
}

/// The answer to `createProfile`: the new immutable version.
public struct ProviderProfileCreatedAnswer: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let profileID: String
    public let profileVersion: String

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case profileID = "profile_id"
        case profileVersion = "profile_version"
    }
}

/// The answer to `setDefault`.
public struct ProviderProfileDefaultSetAnswer: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let capability: String
    public let profileVersion: String

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case capability
        case profileVersion = "profile_version"
    }
}

/// The answer to `clearDefault`.
public struct ProviderProfileDefaultClearedAnswer: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let capability: String

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case capability
    }
}

/// The answer to `list`. A credential is only ever reported as bound or not; the reference never leaves the core.
public struct ProviderProfileListAnswer: Decodable, Equatable, Sendable {
    public struct Capability: Decodable, Equatable, Sendable {
        public let capability: String
        public let support: String
    }

    public struct Version: Decodable, Equatable, Sendable {
        public let profileVersion: String
        public let providerProtocol: String
        public let endpoint: String?
        public let model: String
        public let capabilities: [Capability]
        public let credentialBound: Bool
        public let revoked: Bool
        public let defaultFor: [String]
        public let createdAt: String

        enum CodingKeys: String, CodingKey {
            case profileVersion = "profile_version"
            case providerProtocol = "protocol"
            case endpoint
            case model
            case capabilities
            case credentialBound = "credential_bound"
            case revoked
            case defaultFor = "default_for"
            case createdAt = "created_at"
        }
    }

    public struct Profile: Decodable, Equatable, Sendable {
        public let profileID: String
        /// Oldest version first.
        public let versions: [Version]

        enum CodingKeys: String, CodingKey {
            case profileID = "profile_id"
            case versions
        }
    }

    public struct Default: Decodable, Equatable, Sendable {
        public let capability: String
        public let profileVersion: String

        enum CodingKeys: String, CodingKey {
            case capability
            case profileVersion = "profile_version"
        }
    }

    public let operationID: UInt64
    public let profiles: [Profile]
    public let defaults: [Default]

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case profiles
        case defaults
    }
}

extension CoreHandle {
    /// Queues one provider-profile command. The outcome event for `operationID` decodes as the answer type for that
    /// command, or is a failure whose `code` is fixed and never echoes the request: for example `unknown_protocol`,
    /// `endpoint_invalid`, `credential_required`, `credential_pending_removal`, `timeout_out_of_bounds`,
    /// `profile_not_found`, `profile_revoked` or `capability_not_configurable`. Throws when the request could not be
    /// queued, in which case nothing ran.
    public func startProviderProfiles(operationID: UInt64, command: ProviderProfileCommand) throws {
        let requestBytes = Array(try JSONEncoder().encode(command.wire))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_provider_profiles(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
