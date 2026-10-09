import XCTest
import Security
@testable import OhAndServices

/// Behavior of `ProviderHTTPTransport` against a loopback TLS fixture server.
final class ProviderHTTPTransportTests: ProviderTransportTestCase {
    private let json = Data(#"{"ok":true}"#.utf8)

    // MARK: success path

    func testPostAttachesCredentialAtDispatchAndReturnsStatusHeadersAndBody() async throws {
        let server = try startServer { _ in
            .complete(
                status: 201, headers: ["Content-Type": "application/json", "Set-Cookie": "session=abc", "X-Request-Id": "r-1"],
                body: Data(#"{"ok":true}"#.utf8))
        }
        let reference = try storeSecret()
        let body = Data(#"{"prompt":"synthetic"}"#.utf8)

        let response = try await transport.send(
            makeRequest(
                server.baseURL.appendingPathComponent("v1/interpret"), method: .post, headers: ["X-Test": "yes"], body: body,
                credential: ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer"),
                authorizedOrigins: [server.origin]))

        XCTAssertEqual(response.status, 201)
        XCTAssertEqual(response.body, json)
        XCTAssertEqual(response.headers["content-type"], "application/json")
        XCTAssertEqual(response.headers["x-request-id"], "r-1")
        XCTAssertNil(response.headers["set-cookie"])

        let received = try XCTUnwrap(server.requests.first)
        XCTAssertEqual(received.method, "POST")
        XCTAssertEqual(received.target, "/v1/interpret")
        XCTAssertEqual(received.body, body)
        XCTAssertEqual(received.headers["x-test"], "yes")
        XCTAssertEqual(received.headers["authorization"], "Bearer \(syntheticSecret)")
        XCTAssertEqual(keychain.callCount(.read), 1)
    }

    func testSchemelessCredentialSendsRawSecretInNamedHeader() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()

        _ = try await transport.send(
            makeRequest(
                server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "x-api-key"),
                authorizedOrigins: [server.origin]))

        XCTAssertEqual(server.requests.first?.headers["x-api-key"], syntheticSecret)
        XCTAssertNil(server.requests.first?.headers["authorization"])
    }

    func testHttpErrorStatusesAreReturnedForTheAdapterToInterpret() async throws {
        let server = try startServer { _ in .complete(status: 429, headers: ["Retry-After": "7"]) }

        let response = try await transport.send(makeRequest(server.baseURL, authorizedOrigins: [server.origin]))

        XCTAssertEqual(response.status, 429)
        XCTAssertEqual(response.headers["retry-after"], "7")
        XCTAssertEqual(server.requests.count, 1, "the transport never retries")
    }

    func testCookiesAreNeverStoredOrReplayed() async throws {
        let server = try startServer { _ in .complete(status: 200, headers: ["Set-Cookie": "session=abc; Path=/"]) }
        let request = makeRequest(server.baseURL, authorizedOrigins: [server.origin])

        _ = try await transport.send(request)
        _ = try await transport.send(request)

        XCTAssertEqual(server.requests.count, 2)
        XCTAssertTrue(server.requests.allSatisfy { $0.headers["cookie"] == nil })
    }

    func testCredentialIsResolvedAtDispatchSoAnUpdateAppliesToTheNextCall() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let attachment = ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer")
        let request = makeRequest(server.baseURL, credential: attachment, authorizedOrigins: [server.origin])

        _ = try await transport.send(request)
        try credentials.updateCredential(Data(replacementSecret.utf8), reference: reference)
        _ = try await transport.send(request)

        XCTAssertEqual(server.requests.map { $0.headers["authorization"] }, ["Bearer \(syntheticSecret)", "Bearer \(replacementSecret)"])
    }

    // MARK: credential failures

    func testDeletedInvalidOrMalformedCredentialsFailUnauthorizedWithoutNetworkEffect() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let deleted = try storeSecret()
        try credentials.deleteCredential(reference: deleted)
        let multiline = try storeSecret("line-one\r\nX-Injected: 1")
        let attachmentFor = { (reference: String) in
            ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer")
        }

        for reference in [deleted, multiline, "not-a-credential-reference"] {
            await assertThrows(.credentialUnavailable) {
                try await transport.send(
                    makeRequest(server.baseURL, credential: attachmentFor(reference), authorizedOrigins: [server.origin]))
            }
        }
        XCTAssertEqual(ProviderTransportError.credentialUnavailable.errorClass, .unauthorized)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    func testLockedKeychainIsTransientAndSendsNothing() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        keychain.forcedStatus[.read] = errSecInteractionNotAllowed

        await assertThrows(.credentialTemporarilyUnavailable) {
            try await transport.send(
                makeRequest(
                    server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "x-api-key"),
                    authorizedOrigins: [server.origin]))
        }
        XCTAssertEqual(ProviderTransportError.credentialTemporarilyUnavailable.errorClass, .transient)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: timeout and cancellation

    func testTimeoutBoundsTheWholeExchange() async throws {
        let server = try startServer { _ in .hang }
        let started = Date()

        await assertThrows(.timeout) {
            try await transport.send(makeRequest(server.baseURL, timeout: 1, authorizedOrigins: [server.origin]))
        }

        let elapsed = Date().timeIntervalSince(started)
        XCTAssertGreaterThanOrEqual(elapsed, 0.9)
        XCTAssertLessThan(elapsed, 5)
        XCTAssertEqual(server.requests.count, 1)
    }

    func testCancellingTheTaskStopsAnInFlightExchange() async throws {
        let server = try startServer { _ in .hang }
        let request = makeRequest(server.baseURL, timeout: 30, authorizedOrigins: [server.origin])
        let started = Date()
        let transport = self.transport!

        let task = Task { try await transport.send(request) }
        let reached = await waitUntil { server.requests.count == 1 }
        XCTAssertTrue(reached, "request never reached the server")
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("expected cancellation")
        } catch {
            XCTAssertEqual(error as? ProviderTransportError, .cancelled)
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 10)
    }

    func testAlreadyCancelledTaskHasNoNetworkEffect() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let request = makeRequest(server.baseURL, authorizedOrigins: [server.origin])
        let transport = self.transport!

        let task = Task { () -> ProviderHTTPResponse in
            while !Task.isCancelled { await Task.yield() }
            return try await transport.send(request)
        }
        task.cancel()

        do {
            _ = try await task.value
            XCTFail("expected cancellation")
        } catch {
            XCTAssertEqual(error as? ProviderTransportError, .cancelled)
        }
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: response bounds

    func testResponseExactlyAtTheBoundIsDelivered() async throws {
        let payload = Data(repeating: 0x61, count: 1000)
        let server = try startServer { _ in .complete(status: 200, body: payload) }

        let response = try await transport.send(makeRequest(server.baseURL, maxResponseBytes: 1000, authorizedOrigins: [server.origin]))

        XCTAssertEqual(response.body, payload)
    }

    func testDeclaredOversizeResponseIsRejectedNotTruncated() async throws {
        let server = try startServer { _ in .complete(status: 200, body: Data(repeating: 0x61, count: 5000)) }

        await assertThrows(.responseTooLarge) {
            try await transport.send(makeRequest(server.baseURL, maxResponseBytes: 1000, authorizedOrigins: [server.origin]))
        }
    }

    func testUndeclaredOversizeStreamIsCutOffAtTheBound() async throws {
        let server = try startServer { _ in .endlessBody(chunk: Data(repeating: 0x62, count: 4096)) }
        let started = Date()

        await assertThrows(.responseTooLarge) {
            try await transport.send(makeRequest(server.baseURL, maxResponseBytes: 10_000, authorizedOrigins: [server.origin]))
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 5)
    }

    // MARK: TLS and connection failures

    func testUntrustedCertificateIsRefusedUnderSystemTrust() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()

        await assertThrows(.tlsValidationFailed) {
            try await systemTrustTransport().send(
                makeRequest(
                    server.baseURL, credential: ProviderCredentialAttachment(reference: reference, headerName: "x-api-key"),
                    authorizedOrigins: [server.origin]))
        }
        XCTAssertTrue(server.requests.isEmpty, "no HTTP request, and so no credential, may follow a failed handshake")
    }

    func testHostnameMismatchIsRefusedEvenWhenTheCertificateIsTrusted() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let ipURL = URL(string: "https://127.0.0.1:\(server.port)")!

        await assertThrows(.tlsValidationFailed) {
            try await transport.send(makeRequest(ipURL, authorizedOrigins: ["https://127.0.0.1:\(server.port)"]))
        }
        XCTAssertTrue(server.requests.isEmpty)
    }

    func testRefusedConnectionIsNormalizedAsTransient() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let origin = server.origin
        let url = server.baseURL
        server.stop()

        await assertThrows(.connectionFailed) {
            try await transport.send(makeRequest(url, timeout: 5, authorizedOrigins: [origin]))
        }
        XCTAssertEqual(ProviderTransportError.connectionFailed.errorClass, .transient)
    }

    // MARK: redirects

    func testCrossOriginRedirectIsRefusedAndNeverReachesTheOtherOrigin() async throws {
        let other = try startServer { _ in .complete(status: 200) }
        let origin = try startServer { _ in
            .complete(status: 302, headers: ["Location": "\(other.origin)/capture"])
        }
        let reference = try storeSecret()

        // Even an origin core has authorized for this job is not followed: the redirect target never receives the
        // credential or body, and core can dispatch a fresh, separately authorized request instead.
        await assertThrows(.redirectRefused) {
            try await transport.send(
                makeRequest(
                    origin.baseURL, method: .post, body: Data("payload".utf8),
                    credential: ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer"),
                    authorizedOrigins: [origin.origin, other.origin]))
        }
        XCTAssertEqual(origin.requests.count, 1)
        XCTAssertTrue(other.requests.isEmpty)
        XCTAssertEqual(other.acceptedConnectionCount, 0)
    }

    func testRedirectToCleartextIsRefused() async throws {
        let other = try startServer { _ in .complete(status: 200) }
        let origin = try startServer { _ in
            .complete(status: 307, headers: ["Location": "http://localhost:\(other.port)/capture"])
        }

        await assertThrows(.redirectRefused) {
            try await transport.send(makeRequest(origin.baseURL, authorizedOrigins: [origin.origin]))
        }
        XCTAssertEqual(other.acceptedConnectionCount, 0)
    }

    func testSameOriginRedirectIsFollowedWithTheCredential() async throws {
        let server = try startServer { request in
            if request.target == "/start" {
                return .complete(status: 302, headers: ["Location": "/final"])
            }
            return .complete(status: 200, body: Data("done".utf8))
        }
        let reference = try storeSecret()

        let response = try await transport.send(
            makeRequest(
                server.baseURL.appendingPathComponent("start"),
                credential: ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer"),
                authorizedOrigins: [server.origin]))

        XCTAssertEqual(response.body, Data("done".utf8))
        XCTAssertEqual(server.requests.map(\.target), ["/start", "/final"])
        XCTAssertTrue(server.requests.allSatisfy { $0.headers["authorization"] == "Bearer \(syntheticSecret)" })
    }

    func testRedirectLoopIsBounded() async throws {
        let server = try startServer { _ in .complete(status: 302, headers: ["Location": "/again"]) }

        await assertThrows(.redirectRefused) {
            try await transport.send(makeRequest(server.baseURL, authorizedOrigins: [server.origin]))
        }
        XCTAssertLessThanOrEqual(server.requests.count, ProviderTransportOperation.maximumRedirects + 1)
    }

    // MARK: diagnostics

    func testErrorsNeverRenderRequestDetailsOrSecrets() async throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let secretURL = server.baseURL.appendingPathComponent("private/path")
        var components = URLComponents(url: secretURL, resolvingAgainstBaseURL: false)!
        components.queryItems = [URLQueryItem(name: "token", value: "query-secret-value")]
        let url = try XCTUnwrap(components.url)
        let attachment = ProviderCredentialAttachment(reference: reference, headerName: "Authorization", scheme: "Bearer")

        var errors: [ProviderTransportError] = []
        let scenarios: [ProviderHTTPRequest] = [
            makeRequest(url, credential: attachment, authorizedOrigins: []),
            makeRequest(url, timeout: 0, credential: attachment, authorizedOrigins: [server.origin]),
            makeRequest(url, maxResponseBytes: 0, credential: attachment, authorizedOrigins: [server.origin]),
        ]
        for scenario in scenarios {
            do { _ = try await transport.send(scenario) } catch let error as ProviderTransportError { errors.append(error) }
        }
        do {
            _ = try await systemTrustTransport().send(makeRequest(url, credential: attachment, authorizedOrigins: [server.origin]))
        } catch let error as ProviderTransportError { errors.append(error) }

        XCTAssertEqual(errors.count, 4)
        let forbidden = [syntheticSecret, reference, "query-secret-value", "private/path", "localhost", String(server.port), "Bearer"]
        for error in errors {
            let renderings = [String(describing: error), String(reflecting: error), error.localizedDescription]
            for rendering in renderings {
                for fragment in forbidden {
                    XCTAssertFalse(rendering.contains(fragment), "\(rendering) leaks \(fragment)")
                }
            }
        }
    }
}
