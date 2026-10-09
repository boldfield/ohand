import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The complete provider boundary on the simulator: the real Rust core authorizes and builds each request from
/// stored state, the real `ProviderExchangeCoordinator` hands it to the real `ProviderHTTPTransport`, and the
/// answer travels back through the core's provider adapter. Only the vendor URL is replaced, by a fixture server
/// whose certificate the transport trusts.
final class ProviderExchangeBoundaryTests: ProviderTransportTestCase {
    private var storeDirectory: URL!
    private var session: ProviderExchangeSession?
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-provider-exchange-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        session?.close()
        session = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core was released")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was released")
        try? FileManager.default.removeItem(at: storeDirectory)
        super.tearDown()
    }

    // MARK: helpers

    private func openSession(
        server: FixtureTLSServer,
        scenario: ProviderExchangeScenario,
        base: ProviderHTTPTransport? = nil
    ) throws -> (ProviderExchangeSession, FixtureRoutingSender) {
        let storePath = try ProviderExchangeSeed.makeStore(in: storeDirectory, scenario: scenario)
        let sender = FixtureRoutingSender(underlying: base ?? transport, fixtureServer: server)
        let opened = try ProviderExchangeSession(storePath: storePath, sender: sender)
        session = opened
        return (opened, sender)
    }

    private func anthropicReply() throws -> Data {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": 4]],
        ]
        let reply: [String: Any] = [
            "id": "msg_synthetic_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_synthetic_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }

    private func failure(of event: CoreEvent?) -> CoreFailure? {
        guard let event, case .failure(let failure) = event.outcome else { return nil }
        return failure
    }

    private func payloadText(of event: CoreEvent) -> String {
        switch event.outcome {
        case .success(let bytes):
            return String(decoding: bytes, as: UTF8.self)
        case .failure(let failure):
            return "\(failure.errorClass.rawValue) \(failure.code) \(failure.message)"
        }
    }

    // MARK: valid request

    func testValidRequestCrossesTheWholeBoundaryWithCoreOwnedDestinationAndCredentialReference() throws {
        let reply = try anthropicReply()
        let server = try startServer { _ in .complete(status: 200, headers: ["Content-Type": "application/json"], body: reply) }
        let reference = try storeSecret()
        let (session, sender) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        let final = try XCTUnwrap(session.finalEvent(for: operationID))

        let phases = try session.events(for: operationID).map { try $0.decode(ProviderExchangeEvent.self).phase }
        XCTAssertEqual(phases, [.dispatched, .completed])
        let completed = try final.decode(ProviderExchangeEvent.self)
        guard case .object(let proposal)? = completed.output?.proposal else { return XCTFail("expected a proposal object") }
        XCTAssertEqual(proposal["item_type"], .string("action"))
        XCTAssertEqual(proposal["operation"], .object(["kind": .string("annotate")]))

        let fromCore = try XCTUnwrap(sender.requestsFromCore.first)
        XCTAssertEqual(sender.requestsFromCore.count, 1)
        XCTAssertEqual(fromCore.url.absoluteString, "https://api.anthropic.com/v1/messages")
        XCTAssertEqual(fromCore.method, .post)
        XCTAssertEqual(fromCore.authorization.authorizedOrigins, [ProviderExchangeScenario.vendorOrigin])
        XCTAssertEqual(
            fromCore.credential,
            ProviderCredentialAttachment(reference: reference, headerName: "x-api-key", scheme: nil))
        let lowercasedHeaderNames = Set(fromCore.headers.keys.map { $0.lowercased() })
        XCTAssertFalse(lowercasedHeaderNames.contains("x-api-key"), "the core never places a secret in the request")
        XCTAssertFalse(lowercasedHeaderNames.contains("authorization"))
        XCTAssertFalse(String(decoding: fromCore.body ?? Data(), as: UTF8.self).contains(syntheticSecret))

        let received = try XCTUnwrap(server.requests.first)
        XCTAssertEqual(server.requests.count, 1)
        XCTAssertEqual(received.method, "POST")
        XCTAssertEqual(received.target, "/v1/messages")
        XCTAssertEqual(received.headers["x-api-key"], syntheticSecret, "the transport attached the secret at send time")
        XCTAssertEqual(keychain.callCount(.read), 1)
        XCTAssertFalse(String(decoding: received.body, as: UTF8.self).contains(syntheticSecret))
    }

    func testCredentialRotationBetweenExchangesAppliesToTheNextDispatchWithoutTouchingTheStoredReference() throws {
        let reply = try anthropicReply()
        let server = try startServer { _ in .complete(status: 200, body: reply) }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let first = try session.startExchange()
        XCTAssertNotNil(session.finalEvent(for: first))
        try credentials.updateCredential(Data(replacementSecret.utf8), reference: reference)
        let second = try session.startExchange()
        XCTAssertNotNil(session.finalEvent(for: second))

        XCTAssertEqual(server.requests.map { $0.headers["x-api-key"] }, [syntheticSecret, replacementSecret])
    }

    // MARK: denied and revoked

    func testRouteThatDoesNotPermitTheProviderIsDeniedBeforeAnyNetworkOrKeychainEffect() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let scenario = ProviderExchangeScenario(
            credentialReference: reference,
            routeDestinations: [ProviderExchangeScenario.otherVendorOrigin],
            authorizedDestinations: [ProviderExchangeScenario.otherVendorOrigin])
        let (session, sender) = try openSession(server: server, scenario: scenario)

        let operationID = try session.startExchange()
        let denial = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(denial?.code, "destination_not_in_route")
        XCTAssertEqual(denial?.errorClass, .unauthorized)
        XCTAssertEqual(session.events(for: operationID).count, 1, "a denied job is never dispatched")
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
    }

    func testRevokedProfileIsDeniedAtDispatchWithoutNetworkEffect() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        var scenario = ProviderExchangeScenario(credentialReference: reference)
        scenario.profileRevoked = true
        let (session, sender) = try openSession(server: server, scenario: scenario)

        let operationID = try session.startExchange()
        let denial = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(denial?.code, "profile_revoked")
        XCTAssertEqual(denial?.errorClass, .unauthorized)
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
    }

    func testCapabilityWithoutRouteAuthorizationIsDenied() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        var scenario = ProviderExchangeScenario(credentialReference: reference)
        scenario.authorizedDestinations = [ProviderExchangeScenario.otherVendorOrigin]
        let (session, sender) = try openSession(server: server, scenario: scenario)

        let operationID = try session.startExchange()
        let denial = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(denial?.code, "destination_not_authorized")
        XCTAssertEqual(denial?.errorClass, .unauthorized)
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    func testUnknownJobIsNotFoundAndSendsNothing() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let (session, sender) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange(jobID: "job-that-does-not-exist")

        XCTAssertEqual(failure(of: session.finalEvent(for: operationID))?.code, "not_found")
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: cancellation

    func testCancellingThroughTheCoreStopsTheInFlightExchangeAndEndsCancelled() throws {
        let server = try startServer { _ in .hang }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        XCTAssertTrue(session.pump(until: { server.requests.count == 1 }), "the request reached the server")
        try session.core.cancelProviderExchange(operationID: operationID)

        let final = failure(of: session.finalEvent(for: operationID))
        XCTAssertEqual(final?.errorClass, .cancelled)
        try session.core.cancelProviderExchange(operationID: operationID)
    }

    func testShuttingDownTheTransportCancelsInFlightExchanges() throws {
        let server = try startServer { _ in .hang }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        XCTAssertTrue(session.pump(until: { server.requests.count == 1 && !session.events(for: operationID).isEmpty }))
        session.close()

        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        XCTAssertEqual(
            session.events(for: operationID).count, 1,
            "only the dispatched event arrived; the closed core delivers nothing further")
    }

    // MARK: response and error mapping

    func testProviderHTTPStatusesKeepTheExistingErrorContract() throws {
        let statuses = ["401": 401, "429": 429, "400": 400]
        var nextStatus = 401
        let server = try startServer { _ in .complete(status: nextStatus, body: Data("{}".utf8)) }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        var classes: [String: CoreErrorClass] = [:]
        for (name, status) in statuses.sorted(by: { $0.key < $1.key }) {
            nextStatus = status
            let operationID = try session.startExchange()
            classes[name] = failure(of: session.finalEvent(for: operationID))?.errorClass
        }

        XCTAssertEqual(classes["401"], .unauthorized)
        XCTAssertEqual(classes["429"], .transient)
        XCTAssertEqual(classes["400"], .permanent)
    }

    func testMalformedProviderBodyIsAPermanentInvalidOutput() throws {
        let server = try startServer { _ in .complete(status: 200, body: Data("<html>gateway</html>".utf8)) }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        let rejection = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(rejection?.errorClass, .permanent)
        XCTAssertEqual(rejection?.code, "invalid_output")
    }

    func testMissingCredentialIsUnauthorizedWithoutNetworkEffect() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        try credentials.deleteCredential(reference: reference)
        let (session, sender) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        let rejection = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(rejection?.errorClass, .unauthorized)
        XCTAssertEqual(sender.requestsFromCore.count, 1, "the core dispatched; the transport refused")
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    func testLockedKeychainIsTransient() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        keychain.forcedStatus[.read] = errSecInteractionNotAllowed
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()

        XCTAssertEqual(failure(of: session.finalEvent(for: operationID))?.errorClass, .transient)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
    }

    // MARK: redirects and diagnostics

    func testCrossOriginRedirectIsRefusedAndNeverCarriesTheCredentialToTheOtherOrigin() throws {
        let otherServer = try startServer { _ in .complete(status: 200) }
        let server = try startServer { _ in
            .complete(status: 307, headers: ["Location": otherServer.baseURL.appendingPathComponent("capture").absoluteString])
        }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        let rejection = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(rejection?.errorClass, .permanent)
        XCTAssertEqual(otherServer.acceptedConnectionCount, 0, "the redirect target is never contacted")
        XCTAssertTrue(otherServer.requests.isEmpty)
    }

    func testUntrustedCertificateIsRefusedAndDiagnosticsStayProtected() throws {
        let server = try startServer { _ in .complete(status: 200) }
        let reference = try storeSecret()
        let (session, _) = try openSession(
            server: server, scenario: ProviderExchangeScenario(credentialReference: reference),
            base: systemTrustTransport())

        let operationID = try session.startExchange()
        let rejection = failure(of: session.finalEvent(for: operationID))

        XCTAssertEqual(rejection?.errorClass, .permanent)
        XCTAssertTrue(server.requests.isEmpty, "no request was sent over an unvalidated connection")
        for event in session.events(for: operationID) {
            let text = payloadText(of: event)
            XCTAssertFalse(text.contains(syntheticSecret))
            XCTAssertFalse(text.contains(reference), "the credential reference never reaches diagnostics")
            XCTAssertFalse(text.contains("localhost"))
            XCTAssertFalse(text.contains("api.anthropic.com"))
            XCTAssertFalse(text.contains("roofer"), "capture text never reaches diagnostics")
        }
    }

    func testNoEventOrFailureEverContainsTheSecretOrCaptureText() throws {
        let server = try startServer { _ in .complete(status: 401, body: Data(#"{"error":"bad key"}"#.utf8)) }
        let reference = try storeSecret()
        let (session, _) = try openSession(server: server, scenario: ProviderExchangeScenario(credentialReference: reference))

        let operationID = try session.startExchange()
        _ = session.finalEvent(for: operationID)

        for event in session.events {
            let text = payloadText(of: event)
            XCTAssertFalse(text.contains(syntheticSecret))
            XCTAssertFalse(text.contains(reference))
            XCTAssertFalse(text.contains("roofer"))
        }
    }
}
