import Foundation
import Security

/// The native HTTP effect behind provider requests.
///
/// Guarantees, each covered by tests against a loopback TLS fixture server:
/// - Authorization precedes everything: a request whose destination is not inside the core-supplied authorized
///   origins fails before the Keychain is read or a socket is opened.
/// - https only, with the system's certificate and hostname validation. There is no setting, header, redirect or
///   error path that downgrades to cleartext or skips validation.
/// - The secret is resolved from the Keychain at dispatch, attached inside this type and never returned or logged.
/// - Redirects are followed only within the original origin, so a credential or body never reaches another origin.
/// - Time and response size are bounded, cancellation stops the exchange, and nothing is retried here.
/// - Cookies, URL caches and stored URL credentials are not used.
public final class ProviderHTTPTransport {
    private let credentials: CredentialService
    #if DEBUG
    private var fixtureTrustAnchors: [SecCertificate] = []
    #endif

    /// Certificates are validated by the system trust store only.
    public init(credentials: CredentialService) {
        self.credentials = credentials
    }

    #if DEBUG
    /// Debug builds only: substitutes trust anchors so tests can trust a fixture server's ephemeral certificate.
    /// Validation, including hostname checks, still runs against those anchors. Compiled out of release builds, so
    /// no production code path can replace the system roots.
    convenience init(credentials: CredentialService, trustAnchors: [SecCertificate]) {
        self.init(credentials: credentials)
        self.fixtureTrustAnchors = trustAnchors
    }
    #endif

    /// Sends one request. Cancelling the calling task cancels the exchange and throws `.cancelled`. Every failure is
    /// a `ProviderTransportError`.
    public func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        if Task.isCancelled { throw ProviderTransportError.cancelled }
        let plan = try ProviderRequestPlan(request)
        let urlRequest = try makeURLRequest(for: plan)
        if Task.isCancelled { throw ProviderTransportError.cancelled }

        let operation = ProviderTransportOperation(
            urlRequest: urlRequest,
            origin: plan.origin,
            timeout: plan.timeout,
            maxResponseBytes: plan.maxResponseBytes)
        #if DEBUG
        operation.fixtureTrustAnchors = fixtureTrustAnchors
        #endif
        return try await operation.run()
    }

    private func makeURLRequest(for plan: ProviderRequestPlan) throws -> URLRequest {
        var urlRequest = URLRequest(
            url: plan.url, cachePolicy: .reloadIgnoringLocalAndRemoteCacheData, timeoutInterval: plan.timeout)
        urlRequest.httpMethod = plan.method.rawValue
        urlRequest.httpBody = plan.body
        urlRequest.httpShouldHandleCookies = false
        for (name, value) in plan.headers {
            urlRequest.setValue(value, forHTTPHeaderField: name)
        }
        if let credential = plan.credential {
            urlRequest.setValue(try attachmentValue(for: credential), forHTTPHeaderField: credential.headerName)
        }
        return urlRequest
    }

    private func attachmentValue(for credential: ProviderCredentialAttachment) throws -> String {
        let secretBytes: Data
        do {
            secretBytes = try credentials.resolveSecret(reference: credential.reference)
        } catch let error as CredentialError {
            switch error {
            case .invalidReference, .emptySecret, .notFound, .invalidated:
                throw ProviderTransportError.credentialUnavailable
            case .deviceLocked, .storage:
                throw ProviderTransportError.credentialTemporarilyUnavailable
            }
        } catch {
            throw ProviderTransportError.credentialTemporarilyUnavailable
        }
        guard let secret = String(data: secretBytes, encoding: .utf8), !secret.isEmpty,
            ProviderRequestPlan.isValidHeaderValue(secret)
        else { throw ProviderTransportError.credentialUnavailable }
        if let scheme = credential.scheme {
            return "\(scheme) \(secret)"
        }
        return secret
    }
}
