import Foundation

/// A request that passed every check that needs no I/O: https only, destination inside the authorized origins,
/// safe headers and sane limits. Building a plan never touches the network or the Keychain, so a request that fails
/// here has no effect at all.
struct ProviderRequestPlan {
    static let maximumTimeout: TimeInterval = 300
    static let maximumResponseBytesCeiling = 16 * 1024 * 1024

    /// Headers a caller may never supply: they would put a credential, cookie or framing decision in the hands of
    /// protocol code instead of the native transport.
    private static let forbiddenHeaderNames: Set<String> = [
        "authorization", "proxy-authorization", "cookie", "host", "content-length", "transfer-encoding",
        "connection", "upgrade", "te", "trailer",
    ]

    let url: URL
    let origin: ProviderOrigin
    let method: ProviderHTTPMethod
    let headers: [String: String]
    let body: Data?
    let timeout: TimeInterval
    let maxResponseBytes: Int
    let credential: ProviderCredentialAttachment?

    init(_ request: ProviderHTTPRequest) throws {
        if let scheme = URLComponents(url: request.url, resolvingAgainstBaseURL: false)?.scheme?.lowercased(),
            scheme != "https"
        {
            throw ProviderTransportError.invalidRequest(.insecureScheme)
        }
        if let components = URLComponents(url: request.url, resolvingAgainstBaseURL: false),
            components.user != nil || components.password != nil
        {
            throw ProviderTransportError.invalidRequest(.embeddedCredentials)
        }
        guard let origin = ProviderOrigin(url: request.url) else {
            throw ProviderTransportError.invalidRequest(.malformedURL)
        }

        let authorizedOrigins = request.authorization.authorizedOrigins.compactMap { ProviderOrigin(bareOrigin: $0) }
        guard authorizedOrigins.contains(origin) else {
            throw ProviderTransportError.destinationNotAuthorized
        }

        guard request.timeout > 0, request.timeout <= Self.maximumTimeout,
            (1...Self.maximumResponseBytesCeiling).contains(request.maxResponseBytes)
        else { throw ProviderTransportError.invalidRequest(.invalidLimits) }
        if request.method == .get, request.body != nil {
            throw ProviderTransportError.invalidRequest(.bodyNotAllowed)
        }

        let attachmentHeader = request.credential?.headerName.lowercased()
        for (name, value) in request.headers {
            guard Self.isValidHeaderName(name), Self.isValidHeaderValue(value) else {
                throw ProviderTransportError.invalidRequest(.malformedHeader)
            }
            let lowered = name.lowercased()
            if Self.forbiddenHeaderNames.contains(lowered) || lowered == attachmentHeader {
                throw ProviderTransportError.invalidRequest(.forbiddenHeader)
            }
        }
        if let credential = request.credential {
            guard Self.isValidHeaderName(credential.headerName) else {
                throw ProviderTransportError.invalidRequest(.malformedHeader)
            }
            if Self.forbiddenHeaderNames.subtracting(["authorization"]).contains(credential.headerName.lowercased()) {
                throw ProviderTransportError.invalidRequest(.forbiddenHeader)
            }
            if let scheme = credential.scheme, !Self.isValidHeaderName(scheme) {
                throw ProviderTransportError.invalidRequest(.malformedHeader)
            }
        }

        self.url = request.url
        self.origin = origin
        self.method = request.method
        self.headers = request.headers
        self.body = request.body
        self.timeout = request.timeout
        self.maxResponseBytes = request.maxResponseBytes
        self.credential = request.credential
    }

    private static let tokenCharacters = CharacterSet(
        charactersIn: "!#$%&'*+-.^_`|~0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")

    static func isValidHeaderName(_ name: String) -> Bool {
        !name.isEmpty && name.unicodeScalars.allSatisfy { tokenCharacters.contains($0) }
    }

    /// Visible ASCII, space and tab only: no CR, LF or other control characters that could split a header.
    static func isValidHeaderValue(_ value: String) -> Bool {
        value.unicodeScalars.allSatisfy { ($0.value >= 0x20 && $0.value < 0x7f) || $0.value == 0x09 }
    }
}
