import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers every provider request with one scripted reply and records the requests the core built.
private final class RecordingProviderSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private let reply: Data
    private var recorded: [ProviderHTTPRequest] = []

    init(reply: Data) {
        self.reply = reply
    }

    var requests: [ProviderHTTPRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        recorded.append(request)
        lock.unlock()
        return ProviderHTTPResponse(status: 200, headers: ["Content-Type": "application/json"], body: reply)
    }
}

/// Sends each request the core built to a loopback TLS fixture instead of the vendor's host, through the real native
/// transport, so the credential the core names is resolved from the Keychain and attached exactly as in production.
private final class FixtureRedirectingSender: ProviderRequestSender, @unchecked Sendable {
    private let transport: ProviderHTTPTransport
    private let fixtureOrigin: String
    private let fixturePort: Int

    init(transport: ProviderHTTPTransport, fixturePort: UInt16) {
        self.transport = transport
        self.fixturePort = Int(fixturePort)
        self.fixtureOrigin = "https://localhost:\(fixturePort)"
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        guard var components = URLComponents(url: request.url, resolvingAgainstBaseURL: false) else {
            throw ProviderTransportError.invalidRequest(.malformedURL)
        }
        components.scheme = "https"
        components.host = "localhost"
        components.port = fixturePort
        guard let redirected = components.url else { throw ProviderTransportError.invalidRequest(.malformedURL) }
        return try await transport.send(
            ProviderHTTPRequest(
                url: redirected, method: request.method, headers: request.headers, body: request.body,
                timeout: request.timeout, maxResponseBytes: request.maxResponseBytes,
                credential: request.credential,
                authorization: ProviderTransportAuthorization(
                    jobID: request.authorization.jobID, capability: request.authorization.capability,
                    authorizedOrigins: [fixtureOrigin])))
    }
}

private final class ConfigurationResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// Drives `ServiceCompositionRoot.providerConfiguration` and `.providerCredentials` into the real Rust core over an
/// on-disk store: a profile with a credential, a default, a grant, the job a capture then pins, a connection check
/// against a loopback fixture and a revocation.
final class ProviderConfigurationRootIntegrationTests: XCTestCase {
    private static let routeID = "route-provider-configuration"
    private static let captureText = "Remind me 2099-01-15 11:00:00 to call the roofer about the quarterly estimate"
    private static let syntheticKey = "synthetic-provider-key-do-not-leak-0001"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var fixtureIdentity: FixtureTLSIdentity?
    private var fixtureServer: FixtureTLSServer?

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("provider-configuration-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        fixtureServer?.stop()
        fixtureServer = nil
        fixtureIdentity?.remove()
        fixtureIdentity = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testAProfileDefaultAndGrantPinANewCaptureAndItsJobDispatchesToTheProvider() throws {
        let sender = RecordingProviderSender(reply: try anthropicReply())
        let keychain = FakeKeychain()
        let root = try makeRoot(authenticated: true, sender: sender, keychain: keychain)
        defer { root.shutDown() }
        let configuration = root.providerConfiguration

        let reference = try root.providerCredentials.addCredential(Data(Self.syntheticKey.utf8))
        XCTAssertEqual(try root.providerCredentials.credentialStatus(reference: reference), .present)
        let created = try run { try await configuration.createProfile(self.draft(reference: reference)) }
        XCTAssertEqual(created.profileID, "profile-a")
        try run {
            try await configuration.setDefault(capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        let disposition = try run {
            try await configuration.grant(
                routeID: Self.routeID, capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        XCTAssertEqual(disposition, .granted)
        let repeated = try run {
            try await configuration.grant(
                routeID: Self.routeID, capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        XCTAssertEqual(repeated, .unchanged, "the same grant twice changes nothing")

        let listing = try run { try await configuration.listProfiles() }
        XCTAssertEqual(listing.defaults.map(\.profileVersion), [created.profileVersion])
        let storedVersion = try XCTUnwrap(listing.profiles.first?.versions.first)
        XCTAssertTrue(storedVersion.credentialBound)
        XCTAssertEqual(storedVersion.defaultFor, ["text_interpretation"])
        let routes = try run { try await configuration.listRoutePolicy() }
        let granted = try XCTUnwrap(routes.first { $0.routeID == Self.routeID })
        XCTAssertEqual(granted.grantedCapabilities.map(\.capability), ["text_interpretation"])

        let itemID = try saveCapture(root)
        let jobProfile = scalar("SELECT profile_version FROM jobs WHERE item_id = '\(itemID)'")
        XCTAssertEqual(jobProfile, created.profileVersion, "the new capture's job is pinned to the default")

        root.start(isActive: true)
        root.jobRunner.requestDrain()
        XCTAssertTrue(
            pump(until: { !sender.requests.isEmpty }, timeout: 30), "the pinned job dispatches to the provider")
        XCTAssertEqual(sender.requests.first?.url.host, "api.anthropic.com")
        XCTAssertEqual(sender.requests.first?.credential?.reference, reference)
        XCTAssertTrue(
            pump(until: {
                self.scalar("SELECT processing_state FROM items WHERE item_id = '\(itemID)'") == "processed"
            }, timeout: 30), "the provider reply is applied to the item")
        try assertNoSecretOnDisk()
    }

    func testRevokingTheGrantMakesTheQueuedJobFailAuthorizationAtDispatch() throws {
        let sender = RecordingProviderSender(reply: try anthropicReply())
        let root = try makeRoot(authenticated: true, sender: sender, keychain: FakeKeychain())
        defer { root.shutDown() }
        let configuration = root.providerConfiguration
        let reference = try root.providerCredentials.addCredential(Data(Self.syntheticKey.utf8))
        let created = try run { try await configuration.createProfile(self.draft(reference: reference)) }
        try run {
            try await configuration.setDefault(capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        _ = try run {
            try await configuration.grant(
                routeID: Self.routeID, capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        let itemID = try saveCapture(root)
        let jobID = scalar("SELECT job_id FROM jobs WHERE item_id = '\(itemID)'")
        XCTAssertEqual(scalar("SELECT profile_version FROM jobs WHERE job_id = '\(jobID)'"), created.profileVersion)

        let revoked = try run { try await configuration.revoke(routeID: Self.routeID, capability: "text_interpretation") }
        XCTAssertEqual(revoked, .revoked)

        root.start(isActive: true)
        root.jobRunner.requestDrain()
        XCTAssertTrue(
            pump(until: { self.scalar("SELECT failure_reason FROM jobs WHERE job_id = '\(jobID)'") == "unauthorized" }),
            "dispatch fails authorization: \(scalar("SELECT status FROM jobs WHERE job_id = '\(jobID)'"))")
        XCTAssertTrue(sender.requests.isEmpty, "nothing was sent without a grant")
        XCTAssertNotEqual(scalar("SELECT processing_state FROM items WHERE item_id = '\(itemID)'"), "processed")
        XCTAssertEqual(
            scalar("SELECT profile_version FROM jobs WHERE job_id = '\(jobID)'"), created.profileVersion,
            "the job keeps the version it was pinned to")
    }

    func testTheConnectionCheckReportsCredentialRejectedForABadKeyWithoutTheKeyAppearingAnywhere() throws {
        let identity = try FixtureTLSIdentity()
        fixtureIdentity = identity
        let server = try FixtureTLSServer(identity: identity.identity) { _ in
            .complete(
                status: 401, headers: ["Content-Type": "application/json"],
                body: Data(#"{"type":"error","error":{"type":"authentication_error","message":"invalid key"}}"#.utf8))
        }
        try server.start()
        fixtureServer = server
        let keychain = FakeKeychain()
        let credentials = CredentialService(
            serviceName: "com.boldfield.ohand.tests.provider-configuration.\(UUID().uuidString)", keychain: keychain)
        let transport = ProviderHTTPTransport(credentials: credentials, trustAnchors: [identity.certificate])
        let sender = FixtureRedirectingSender(transport: transport, fixturePort: server.port)
        let root = try makeRoot(authenticated: true, sender: sender, keychain: keychain, credentials: credentials)
        defer { root.shutDown() }
        let configuration = root.providerConfiguration

        let reference = try root.providerCredentials.addCredential(Data(Self.syntheticKey.utf8))
        let created = try run { try await configuration.createProfile(self.draft(reference: reference)) }
        let outcome = try run { try await configuration.checkConnection(profileVersion: created.profileVersion) }

        XCTAssertEqual(outcome, .credentialRejected)
        let seen = try XCTUnwrap(server.requests.first, "the fixture received the check")
        XCTAssertEqual(
            seen.headers["x-api-key"], Self.syntheticKey,
            "the transport attached the stored key, so the rejection is the fixture's answer to it")
        XCTAssertFalse(String(data: seen.body, encoding: .utf8)?.contains(Self.captureText) ?? false)
        XCTAssertFalse("\(outcome)".contains(Self.syntheticKey))
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM jobs"), "0", "a check queues no work")
        try assertNoSecretOnDisk()
    }

    func testACaptureOnlySessionCannotWriteGrantCheckOrEnterACredentialAndNothingChanges() throws {
        let sender = RecordingProviderSender(reply: Data())
        let keychain = FakeKeychain()
        let root = try makeRoot(authenticated: false, sender: sender, keychain: keychain)
        defer { root.shutDown() }
        let configuration = root.providerConfiguration

        func assertRefused<Value>(_ operation: @escaping @Sendable () async throws -> Value, _ label: String) throws {
            XCTAssertThrowsError(try run(operation), label) { error in
                XCTAssertEqual(error as? ProviderConfigurationError, .notAuthenticated, label)
            }
        }

        try assertRefused({ try await configuration.createProfile(self.draft(reference: "synthetic-reference")) }, "create")
        try assertRefused(
            { try await configuration.setDefault(capability: "text_interpretation", profileVersion: "version") },
            "default")
        try assertRefused({ try await configuration.checkConnection(profileVersion: "version") }, "check")
        try assertRefused(
            {
                try await configuration.grant(
                    routeID: Self.routeID, capability: "text_interpretation", profileVersion: "version")
            }, "grant")
        try assertRefused(
            { try await configuration.revoke(routeID: Self.routeID, capability: "text_interpretation") }, "revoke")
        try assertRefused({ try await configuration.listProfiles() }, "list profiles")
        try assertRefused({ try await configuration.listRoutePolicy() }, "list policy")
        XCTAssertThrowsError(try root.providerCredentials.addCredential(Data(Self.syntheticKey.utf8))) {
            XCTAssertEqual($0 as? ProviderConfigurationError, .notAuthenticated)
        }

        XCTAssertEqual(scalar("SELECT COUNT(*) FROM provider_profiles"), "0")
        XCTAssertTrue(keychain.items.isEmpty, "no credential was stored")
        XCTAssertTrue(sender.requests.isEmpty)
    }

    func testTheProviderConfigurationRangesAreReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let clients: [(String, UInt64)] = [
            (ServiceCompositionRoot.providerProfileClientName, CoreProviderProfileClient.firstOperationID),
            (ServiceCompositionRoot.providerConnectionCheckClientName, CoreProviderConnectionCheckClient.firstOperationID),
            (ServiceCompositionRoot.routePolicyClientName, CoreRoutePolicyClient.firstOperationID),
        ]
        for (name, firstOperationID) in clients {
            let mine = try XCTUnwrap(ranges.first { $0.name == name }, name)
            XCTAssertEqual(mine.start, firstOperationID)
            let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
            XCTAssertTrue(overlapping.isEmpty, "\(name) overlaps \(overlapping.map(\.name))")
        }
    }

    // MARK: helpers

    private func draft(reference: String) -> ProviderProfileDraft {
        ProviderProfileDraft(
            profileID: "profile-a", providerProtocol: "anthropic", model: "synthetic-model",
            credentialReference: reference, timeoutSeconds: 30)
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func makeRoot(
        authenticated: Bool, sender: ProviderRequestSender, keychain: FakeKeychain,
        credentials: CredentialService? = nil
    ) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        let credentialService = credentials
            ?? CredentialService(
                serviceName: "com.boldfield.ohand.tests.provider-configuration.\(UUID().uuidString)",
                keychain: keychain)
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                credentials: credentialService))
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.captureText, capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = ConfigurationResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    /// The synthetic key lives only in the fake Keychain: no file the core or the root wrote contains it.
    private func assertNoSecretOnDisk(file: StaticString = #filePath, line: UInt = #line) throws {
        let needle = Data(Self.syntheticKey.utf8)
        let enumerator = FileManager.default.enumerator(at: storageRoot, includingPropertiesForKeys: nil)
        var inspected = 0
        while let url = enumerator?.nextObject() as? URL {
            guard let contents = try? Data(contentsOf: url) else { continue }
            inspected += 1
            XCTAssertNil(contents.range(of: needle), "\(url.lastPathComponent) holds the key", file: file, line: line)
        }
        XCTAssertGreaterThan(inspected, 0, "the store was inspected", file: file, line: line)
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    private func anthropicReply() throws -> Data {
        let phraseStart = 10
        let phraseEnd = phraseStart + "2099-01-15 11:00:00".count
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": Self.captureText.count]],
            "reminder_proposal": [
                "instant": "2099-01-15T11:00:00Z",
                "timezone_id": "UTC",
                "quality": "explicit",
                "source_span": ["start": phraseStart, "end": phraseEnd],
            ],
        ]
        let reply: [String: Any] = [
            "id": "msg_provider_configuration_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [
                ["type": "tool_use", "id": "toolu_provider_configuration_01", "name": "interpret", "input": proposal]
            ],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }
}
