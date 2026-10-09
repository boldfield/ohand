import Foundation

public enum ProviderHTTPMethod: String, Equatable {
    case get = "GET"
    case post = "POST"
    case put = "PUT"
    case patch = "PATCH"
    case delete = "DELETE"
}

/// Remote capabilities a destination can be authorized for. On-device processing (transcription) never reaches
/// this transport, so it has no case here.
public enum ProviderTransportCapability: String, Equatable {
    case textInterpretation = "text_interpretation"
    case review
}

/// The native view of the core's destination authorization (`privacy::routing::Authorization`).
///
/// Core produces the decision from stored route policy immediately before dispatch; this transport never
/// widens it. `authorizedOrigins` are the bare https origins core allows (for example `https://api.example.com`).
/// An empty list, or a destination outside the list, means the request is refused before any network effect.
public struct ProviderTransportAuthorization: Equatable {
    public let jobID: String
    public let capability: ProviderTransportCapability
    public let authorizedOrigins: [String]

    public init(jobID: String, capability: ProviderTransportCapability, authorizedOrigins: [String]) {
        self.jobID = jobID
        self.capability = capability
        self.authorizedOrigins = authorizedOrigins
    }
}

/// Which header carries the secret and with which scheme. `scheme` of `Bearer` yields `Bearer <secret>`; a nil
/// scheme sends the secret as the whole header value (for example `x-api-key`).
public struct ProviderCredentialAttachment: Equatable {
    public let reference: String
    public let headerName: String
    public let scheme: String?

    public init(reference: String, headerName: String, scheme: String? = nil) {
        self.reference = reference
        self.headerName = headerName
        self.scheme = scheme
    }
}

public struct ProviderHTTPRequest {
    public let url: URL
    public let method: ProviderHTTPMethod
    public let headers: [String: String]
    public let body: Data?
    /// Total time allowed for the whole exchange, including redirects and reading the body.
    public let timeout: TimeInterval
    /// Largest response body accepted. A larger body fails the call; it is never truncated.
    public let maxResponseBytes: Int
    public let credential: ProviderCredentialAttachment?
    public let authorization: ProviderTransportAuthorization

    public init(
        url: URL,
        method: ProviderHTTPMethod,
        headers: [String: String] = [:],
        body: Data? = nil,
        timeout: TimeInterval,
        maxResponseBytes: Int,
        credential: ProviderCredentialAttachment? = nil,
        authorization: ProviderTransportAuthorization
    ) {
        self.url = url
        self.method = method
        self.headers = headers
        self.body = body
        self.timeout = timeout
        self.maxResponseBytes = maxResponseBytes
        self.credential = credential
        self.authorization = authorization
    }
}

/// Any HTTP status is returned as a response; interpreting statuses is the protocol adapter's job. Header names are
/// lowercased and `set-cookie` is dropped.
public struct ProviderHTTPResponse: Equatable {
    public let status: Int
    public let headers: [String: String]
    public let body: Data

    public init(status: Int, headers: [String: String], body: Data) {
        self.status = status
        self.headers = headers
        self.body = body
    }
}

public enum ProviderTransportErrorClass: String, Equatable {
    case transient
    case permanent
    case unauthorized
    case cancelled
}

public enum ProviderInvalidRequestReason: String, Equatable {
    case malformedURL
    case insecureScheme
    case embeddedCredentials
    case forbiddenHeader
    case malformedHeader
    case bodyNotAllowed
    case invalidLimits
}

/// Normalized transport failure. Cases carry only fixed reasons: never a URL, header, body, reference or secret, so
/// any rendering of an error is safe to log or show. The transport never retries; retry policy belongs to core.
public enum ProviderTransportError: Error, Equatable, CustomStringConvertible, LocalizedError {
    case invalidRequest(ProviderInvalidRequestReason)
    case destinationNotAuthorized
    /// The referenced secret is missing, invalidated or unusable. Waits for the user; no fallback.
    case credentialUnavailable
    /// The secret exists but could not be read right now (for example device locked). Retry later.
    case credentialTemporarilyUnavailable
    case timeout
    case cancelled
    case connectionFailed
    case tlsValidationFailed
    case redirectRefused
    case responseTooLarge
    case invalidResponse

    public var errorClass: ProviderTransportErrorClass {
        switch self {
        case .invalidRequest, .tlsValidationFailed, .redirectRefused, .responseTooLarge, .invalidResponse:
            return .permanent
        case .destinationNotAuthorized, .credentialUnavailable:
            return .unauthorized
        case .credentialTemporarilyUnavailable, .timeout, .connectionFailed:
            return .transient
        case .cancelled:
            return .cancelled
        }
    }

    /// Snake-case name of the matching core `TransportError` variant, for the production binding.
    public var coreTransportError: String {
        switch self {
        case .invalidRequest, .tlsValidationFailed, .redirectRefused:
            return "rejected"
        case .destinationNotAuthorized, .credentialUnavailable:
            return "unauthorized"
        case .credentialTemporarilyUnavailable, .connectionFailed:
            return "unavailable"
        case .timeout:
            return "timeout"
        case .cancelled:
            return "cancelled"
        case .responseTooLarge, .invalidResponse:
            return "invalid_output"
        }
    }

    public var description: String {
        switch self {
        case .invalidRequest(let reason):
            return "provider request is not valid (\(reason.rawValue))"
        case .destinationNotAuthorized:
            return "destination is not authorized for this request"
        case .credentialUnavailable:
            return "provider credential is missing or unusable"
        case .credentialTemporarilyUnavailable:
            return "provider credential is temporarily unavailable"
        case .timeout:
            return "provider request timed out"
        case .cancelled:
            return "provider request was cancelled"
        case .connectionFailed:
            return "provider connection failed"
        case .tlsValidationFailed:
            return "provider certificate validation failed"
        case .redirectRefused:
            return "provider redirect was refused"
        case .responseTooLarge:
            return "provider response exceeded the size bound"
        case .invalidResponse:
            return "provider response was not valid HTTP"
        }
    }

    public var errorDescription: String? { description }
}
