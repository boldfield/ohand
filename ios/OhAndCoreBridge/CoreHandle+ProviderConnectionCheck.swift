import Foundation
import OhandCoreC

/// How a provider connection check ended. The names are the core's fixed set; none of them carries an endpoint,
/// header, response body, credential reference or secret.
public enum ProviderConnectionOutcome: String, Decodable, Equatable, Sendable {
    case ok
    case credentialMissing = "credential_missing"
    case credentialRejected = "credential_rejected"
    case unreachable
    /// Also covers a refused redirect, which the native transport reports the same way.
    case tlsFailure = "tls_failure"
    case timeout
    case capabilityMismatch = "capability_mismatch"
    case protocolNotEnabled = "protocol_not_enabled"
    case rateLimited = "rate_limited"
    case providerError = "provider_error"
    case unexpectedResponse = "unexpected_response"
    case cancelled
}

/// Success events of a connection check: `dispatched` once the request was built and handed to the native
/// transport, then `completed` with the outcome. A check that sends nothing (`protocol_not_enabled`) has only the
/// `completed` event. A failure arrives as a normal failed `CoreEvent` instead.
public struct ProviderConnectionCheckEvent: Decodable, Equatable, Sendable {
    public let operationID: UInt64
    public let phase: ProviderExchangePhase
    /// Present only on the `completed` event.
    public let outcome: ProviderConnectionOutcome?

    enum CodingKeys: String, CodingKey {
        case operationID = "operation_id"
        case phase
        case outcome
    }
}

extension CoreHandle {
    /// Sends one fixed synthetic request, containing no capture or user text, to the stored profile version's own
    /// destinations and reports a redacted outcome. Only an explicit call runs a check: nothing here is background
    /// or scheduled, and no route grant is needed. The send goes through the registered provider transport, so the
    /// provider exchange answers apply and `cancelProviderExchange(operationID:)` cancels it.
    public func startProviderConnectionCheck(operationID: UInt64, profileVersion: String) throws {
        let requestBytes = Array(try JSONEncoder().encode(["profile_version": profileVersion]))
        let rawResult = requestBytes.withUnsafeBufferPointer { buffer in
            ohand_core_start_provider_connection_check(handleIdentifier, operationID, buffer.baseAddress, buffer.count)
        }
        _ = try consumeCoreResult(rawResult)
    }
}
