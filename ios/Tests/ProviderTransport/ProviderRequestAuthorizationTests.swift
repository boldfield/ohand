import XCTest
@testable import OhAndServices

/// The checks that must hold before any network or Keychain effect: destination authorization, https only and
/// header rules. The server-backed tests assert the "no effect" half with a live listener that must see nothing.
final class ProviderRequestAuthorizationTests: ProviderTransportTestCase {
    // MARK: origin parsing

    func testBareOriginParsingMatchesTheCoreFormat() {
        XCTAssertEqual(ProviderOrigin(bareOrigin: "https://api.example.com"), ProviderOrigin(host: "api.example.com", port: 443))
        XCTAssertEqual(ProviderOrigin(bareOrigin: "https://API.Example.com:8443"), ProviderOrigin(host: "api.example.com", port: 8443))
        XCTAssertEqual(ProviderOrigin(bareOrigin: "https://api.example.com:443"), ProviderOrigin(bareOrigin: "https://api.example.com"))
        XCTAssertEqual(ProviderOrigin(bareOrigin: "https://[::1]:9000"), ProviderOrigin(host: "::1", port: 9000))
    }

    func testMalformedOrWiderOriginsCannotAuthorizeAnything() {
        let rejected = [
            "", "*", "https://", "http://api.example.com", "api.example.com", "https://api.example.com/",
            "https://api.example.com/v1", "https://api.example.com?x=1", "https://api.example.com#frag",
            "https://user@api.example.com", "https://api.example.com:0", "https://api.example.com:99999",
            "https://api.example.com:", "https://api.example.com:44a", "https://api .example.com",
            "https://-bad.example.com", "https://api..example.com", "https://api.example.com%2f@evil.com",
            "https://*.example.com", "https://api.example.com\\@evil.com",
        ]
        for text in rejected {
            XCTAssertNil(ProviderOrigin(bareOrigin: text), "\(text) must not parse as an authorized origin")
        }
    }

    func testRequestURLOriginRequiresHttpsWithoutUserinfo() {
        XCTAssertEqual(ProviderOrigin(url: URL(string: "https://Api.Example.com/v1/x?y=1")!), ProviderOrigin(host: "api.example.com", port: 443))
        XCTAssertNil(ProviderOrigin(url: URL(string: "http://api.example.com/")!))
        XCTAssertNil(ProviderOrigin(url: URL(string: "https://user:pw@api.example.com/")!))
        XCTAssertNil(ProviderOrigin(url: URL(string: "ftp://api.example.com/")!))
        XCTAssertNotEqual(
            ProviderOrigin(url: URL(string: "https://api.example.com/")!), ProviderOrigin(url: URL(string: "https://api.example.com:8443/")!))
    }

    // MARK: no network effect without authorization

    func testNoAuthorizedOriginsMeansNoEffect() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let readsBefore = keychain.callCount(.read)

        await assertThrows(.destinationNotAuthorized) {
            try await transport.send(
                makeRequest(
                    server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer"),
                    authorizedOrigins: []))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), readsBefore, "the secret must not be read for an unapproved request")
        XCTAssertEqual(ProviderTransportError.destinationNotAuthorized.errorClass, .unauthorized)
    }

    func testDifferentHostPortOrWiderEntriesDoNotAuthorize() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let notAuthorizing = [
            "https://localhost:\(server.port &+ 1)",
            "https://example.com:\(server.port)",
            "https://localhost:\(server.port)/",
            "https://localhost:\(server.port)/v1",
            "http://localhost:\(server.port)",
            "*",
        ]

        for entry in notAuthorizing {
            await assertThrows(.destinationNotAuthorized) {
                try await transport.send(makeRequest(server.baseURL, authorizedOrigins: [entry]))
            }
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    func testAuthorizationMatchesOriginCaseInsensitivelyAmongSeveralEntries() async throws {
        let server = try startServer { _ in .complete(status: 200) }

        let response = try await transport.send(
            makeRequest(
                server.baseURL.appendingPathComponent("anything"),
                authorizedOrigins: ["https://other.example.com", "HTTPS://LOCALHOST:\(server.port)"]))

        XCTAssertEqual(response.status, 200)
    }

    // MARK: cleartext and malformed URLs

    func testCleartextIsRefusedEvenWhenTheOriginIsAuthorized() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let cleartext = URL(string: "http://localhost:\(server.port)/v1")!

        await assertThrows(.invalidRequest(.insecureScheme)) {
            try await transport.send(
                makeRequest(
                    cleartext, credential: ProviderCredentialAttachment(reference: reference, headerName: "x-api-key"),
                    authorizedOrigins: [server.origin, "http://localhost:\(server.port)"]))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
    }

    func testEmbeddedCredentialsAndNonHttpURLsAreRefused() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let withUserinfo = URL(string: "https://user:pw@localhost:\(server.port)/")!
        let fileURL = URL(string: "file:///etc/hosts")!

        await assertThrows(.invalidRequest(.embeddedCredentials)) {
            try await transport.send(makeRequest(withUserinfo, authorizedOrigins: [server.origin]))
        }
        await assertThrows(.invalidRequest(.insecureScheme)) {
            try await transport.send(makeRequest(fileURL, authorizedOrigins: [server.origin]))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: header and limit rules

    func testCallerCannotSupplyCredentialCarryingOrFramingHeaders() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let attachment = ProviderCredentialAttachment(reference: reference, headerName: "x-api-key")

        for name in ["Authorization", "authorization", "Cookie", "Proxy-Authorization", "Host", "Content-Length", "Transfer-Encoding"] {
            await assertThrows(.invalidRequest(.forbiddenHeader)) {
                try await transport.send(makeRequest(server.baseURL, headers: [name: "x"], authorizedOrigins: [server.origin]))
            }
        }
        await assertThrows(.invalidRequest(.forbiddenHeader)) {
            try await transport.send(
                makeRequest(server.baseURL, headers: ["X-Api-Key": "caller-supplied"], credential: attachment, authorizedOrigins: [server.origin]))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
    }

    func testMalformedHeadersAreRefused() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()

        await assertThrows(.invalidRequest(.malformedHeader)) {
            try await transport.send(makeRequest(server.baseURL, headers: ["X-Ok": "a\r\nX-Injected: b"], authorizedOrigins: [server.origin]))
        }
        await assertThrows(.invalidRequest(.malformedHeader)) {
            try await transport.send(makeRequest(server.baseURL, headers: ["Bad Name": "a"], authorizedOrigins: [server.origin]))
        }
        await assertThrows(.invalidRequest(.malformedHeader)) {
            try await transport.send(
                makeRequest(
                    server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "X-Key\r\nX: y"),
                    authorizedOrigins: [server.origin]))
        }
        await assertThrows(.invalidRequest(.forbiddenHeader)) {
            try await transport.send(
                makeRequest(
                    server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "Cookie"),
                    authorizedOrigins: [server.origin]))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    func testInvalidLimitsAndBodiesAreRefused() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let origins = [server.origin]

        for timeout in [0, -1, ProviderRequestPlan.maximumTimeout + 1] {
            await assertThrows(.invalidRequest(.invalidLimits)) {
                try await transport.send(makeRequest(server.baseURL, timeout: timeout, authorizedOrigins: origins))
            }
        }
        for bound in [0, -5, ProviderRequestPlan.maximumResponseBytesCeiling + 1] {
            await assertThrows(.invalidRequest(.invalidLimits)) {
                try await transport.send(makeRequest(server.baseURL, maxResponseBytes: bound, authorizedOrigins: origins))
            }
        }
        await assertThrows(.invalidRequest(.bodyNotAllowed)) {
            try await transport.send(makeRequest(server.baseURL, method: .get, body: Data("x".utf8), authorizedOrigins: origins))
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: normalization

    func testEveryFailureHasAClassAndACoreTransportName() {
        let all: [ProviderTransportError] = [
            .invalidRequest(.malformedURL), .destinationNotAuthorized, .credentialUnavailable, .credentialTemporarilyUnavailable,
            .timeout, .cancelled, .connectionFailed, .tlsValidationFailed, .redirectRefused, .responseTooLarge, .invalidResponse,
        ]
        let coreNames: Set<String> = ["timeout", "cancelled", "unavailable", "rate_limited", "unauthorized", "rejected", "invalid_output"]
        for error in all {
            XCTAssertTrue(coreNames.contains(error.coreTransportError), "\(error) maps outside the core TransportError set")
            XCTAssertFalse(error.description.isEmpty)
        }
        XCTAssertEqual(ProviderTransportError.timeout.errorClass, .transient)
        XCTAssertEqual(ProviderTransportError.cancelled.errorClass, .cancelled)
        XCTAssertEqual(ProviderTransportError.tlsValidationFailed.errorClass, .permanent)
        XCTAssertEqual(ProviderTransportError.tlsValidationFailed.coreTransportError, "rejected")
        XCTAssertEqual(ProviderTransportError.credentialUnavailable.coreTransportError, "unauthorized")
    }

    func testURLErrorsNormalizeWithoutLeakingTheOriginalError() {
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.timedOut)), .timeout)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.cancelled)), .cancelled)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.serverCertificateUntrusted)), .tlsValidationFailed)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.secureConnectionFailed)), .tlsValidationFailed)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.appTransportSecurityRequiresSecureConnection)), .tlsValidationFailed)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.notConnectedToInternet)), .connectionFailed)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.cannotFindHost)), .connectionFailed)
        XCTAssertEqual(ProviderTransportOperation.normalized(URLError(.httpTooManyRedirects)), .redirectRefused)
        XCTAssertEqual(ProviderTransportOperation.normalized(NSError(domain: "synthetic", code: 7)), .connectionFailed)
    }
}
