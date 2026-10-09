import XCTest
import Security
@testable import OhAndServices

/// Shared fixture for transport tests: an ephemeral certificate that the transport under test trusts as its only
/// anchor, a fake Keychain behind a real `CredentialService`, and helpers to start loopback servers.
class ProviderTransportTestCase: XCTestCase {
    let syntheticSecret = "synthetic-provider-secret-one"
    let replacementSecret = "synthetic-provider-secret-two"

    private(set) var tlsIdentity: FixtureTLSIdentity!
    private(set) var keychain = FakeKeychain()
    private(set) var credentials: CredentialService!
    private(set) var transport: ProviderHTTPTransport!
    private var servers: [FixtureTLSServer] = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        tlsIdentity = try FixtureTLSIdentity()
        keychain = FakeKeychain()
        credentials = CredentialService(serviceName: "com.boldfield.ohand.tests.transport.\(UUID().uuidString)", keychain: keychain)
        transport = ProviderHTTPTransport(credentials: credentials, trustAnchors: [tlsIdentity.certificate])
    }

    override func tearDown() {
        servers.forEach { $0.stop() }
        servers = []
        tlsIdentity?.remove()
        super.tearDown()
    }

    func startServer(_ handler: @escaping (FixtureTLSServer.RecordedRequest) -> FixtureTLSServer.Response) throws -> FixtureTLSServer {
        let server = try FixtureTLSServer(identity: tlsIdentity.identity, handler: handler)
        try server.start()
        servers.append(server)
        return server
    }

    func storeSecret(_ secret: String? = nil) throws -> String {
        try credentials.addCredential(Data((secret ?? syntheticSecret).utf8))
    }

    /// A transport that trusts nothing beyond the system roots, as in production.
    func systemTrustTransport() -> ProviderHTTPTransport {
        ProviderHTTPTransport(credentials: credentials)
    }

    func makeRequest(
        _ url: URL,
        method: ProviderHTTPMethod = .get,
        headers: [String: String] = [:],
        body: Data? = nil,
        timeout: TimeInterval = 10,
        maxResponseBytes: Int = 64 * 1024,
        credential: ProviderCredentialAttachment? = nil,
        authorizedOrigins: [String]
    ) -> ProviderHTTPRequest {
        ProviderHTTPRequest(
            url: url, method: method, headers: headers, body: body, timeout: timeout,
            maxResponseBytes: maxResponseBytes, credential: credential,
            authorization: ProviderTransportAuthorization(
                jobID: UUID().uuidString, capability: .textInterpretation, authorizedOrigins: authorizedOrigins))
    }

    func assertThrows<Result>(
        _ expected: ProviderTransportError, file: StaticString = #filePath, line: UInt = #line,
        _ operation: () async throws -> Result
    ) async {
        do {
            _ = try await operation()
            XCTFail("expected \(expected) but the call succeeded", file: file, line: line)
        } catch let error as ProviderTransportError {
            XCTAssertEqual(error, expected, file: file, line: line)
        } catch {
            XCTFail("expected \(expected) but got an unnormalized error", file: file, line: line)
        }
    }

    func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }
}
