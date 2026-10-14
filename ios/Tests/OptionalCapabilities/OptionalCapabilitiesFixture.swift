import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private let storeBusyTimeoutMilliseconds: Int32 = 5_000

/// Synthetic stored state for one queued interpretation job on a self-hosted profile, on a route with or without its
/// grant. Fixture data only: the host is a reserved example name and the credential reference is an opaque label.
struct SelfHostedScenario {
    static let profileVersion = "9d3f6b20-1c7e-4a58-8e2b-6a0c4d9e7f11"
    static let origin = "https://spark.example.test"
    static let endpoint = "https://spark.example.test/v1"
    static let credentialReference = "credential-ref/synthetic-self-hosted"
    static let routeID = "route-optional"
    static let captureID = "5a1c0000-0000-4000-8000-0000000000c1"
    static let itemID = "5a1c0000-0000-4000-8000-0000000000e1"
    static let requestVersion = "5a1c0000-0000-4000-8000-0000000000a1"
    static let jobID = "job-optional-1"
    static let captureText = "call the roofer tomorrow at 9"

    var model = SelfHostedVerifiedBoundary.current.model
    var capabilitySupport = "supported"
    var isRevoked = false
    var interpretationGranted = true
    var includesProfile = true
}

enum SelfHostedSeed {
    private static let retryPolicy = #"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#

    /// Creates the store with the real core so its migrations run, closes it, then inserts the rows that import and
    /// profile installation would create.
    static func makeStore(in directory: URL, scenario: SelfHostedScenario) throws -> String {
        let path = directory.appendingPathComponent("core.sqlite").path
        try CoreHandle(path: path).close()
        try execute(path: path, statements: statements(scenario))
        return path
    }

    private static func capabilities(_ scenario: SelfHostedScenario) -> String {
        let evidence = scenario.capabilitySupport == "supported" ? #""optional-capabilities-tests""# : "null"
        let reason = scenario.capabilitySupport == "supported" ? "null" : #""not verified""#
        return #"{"text_interpretation":{"capability":"text_interpretation","support":""#
            + scenario.capabilitySupport + #"","evidence":"# + evidence + #","reason":"# + reason
            + #","input_size_limit":500,"structured_output":"json_schema"}}"#
    }

    private static func statements(_ scenario: SelfHostedScenario) -> [String] {
        let origins = quote("[\"\(SelfHostedScenario.origin)\"]")
        let revokedAt = scenario.isRevoked ? "'2026-10-08T09:10:00Z'" : "NULL"
        var statements = [
            """
            INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, \
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) VALUES \
            (\(quote(SelfHostedScenario.captureID)), \(quote(SelfHostedScenario.captureText)), NULL, \
            '2026-10-08T09:30:00Z', 'America/Chicago', -300, 'en_US', 'gregorian', 'personal', \
            \(quote(SelfHostedScenario.routeID)), 0, '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, \
            processing_state, transcription_state, created_at, updated_at) VALUES \
            (\(quote(SelfHostedScenario.itemID)), \(quote(SelfHostedScenario.captureID)), 0, 'active', 'saved_local', \
            'not_configured', 'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            (\(quote(SelfHostedScenario.routeID)), 'synthetic', 'personal', \(origins), '2026-10-08T09:00:00Z')
            """,
        ]
        if scenario.includesProfile {
            statements.append(
                """
                INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, \
                credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at, \
                revoked_at) VALUES (\(quote(SelfHostedScenario.profileVersion)), 'synthetic-self-hosted-profile', \
                'self_hosted', \(quote(SelfHostedScenario.endpoint)), \(quote(scenario.model)), \
                \(quote(SelfHostedScenario.credentialReference)), 30, \(quote(retryPolicy)), \(origins), \
                \(quote(capabilities(scenario))), '2026-10-08T09:00:00Z', \(revokedAt))
                """)
            statements.append(
                """
                INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
                request_version, status, attempt_count, created_at) VALUES (\(quote(SelfHostedScenario.jobID)), 1, \
                \(quote(SelfHostedScenario.itemID)), 'interpret', 0, \(quote(SelfHostedScenario.profileVersion)), \
                \(quote(SelfHostedScenario.requestVersion)), 'queued', 0, '2026-10-08T09:30:02Z')
                """)
        }
        if scenario.interpretationGranted {
            statements.append(
                """
                INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) VALUES \
                (\(quote(SelfHostedScenario.routeID)), 'text_interpretation', \(origins), '2026-10-08T09:00:00Z')
                """)
        }
        return statements
    }

    private static func execute(path: String, statements: [String]) throws {
        var database: OpaquePointer?
        guard sqlite3_open(path, &database) == SQLITE_OK else { throw SeedFailure.open }
        sqlite3_busy_timeout(database, storeBusyTimeoutMilliseconds)
        defer { sqlite3_close(database) }
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw SeedFailure.statementFailed }
        }
    }

    enum SeedFailure: Error {
        case open
        case statementFailed
    }

    private static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "''") + "'"
    }
}

/// Answers each send by host and records every request the core built. A host with no scripted reply gets a native
/// refusal, so a request to an unexpected destination is visible both in `requests` and as a failure.
final class RecordingProviderSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private let repliesByHost: [String: Data]
    private var recorded: [ProviderHTTPRequest] = []

    init(repliesByHost: [String: Data] = [:]) {
        self.repliesByHost = repliesByHost
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
        guard let host = request.url.host, let body = repliesByHost[host] else {
            throw ProviderTransportError.destinationNotAuthorized
        }
        return ProviderHTTPResponse(status: 200, headers: ["content-type": "application/json"], body: body)
    }
}

/// Takes one fixed-range core client on attach, so a test can tell whether another module already holds it.
final class ShadowClientProbeModule: CompositionModule {
    let identifier = "optional.capabilities.probe"
    let firstOperationID: UInt64 = 1 << 45
    let configuration: ShadowReviewConfiguration
    private(set) var service: ShadowReviewService?

    init(configuration: ShadowReviewConfiguration) {
        self.configuration = configuration
    }

    func attach(to context: CompositionContext) throws {
        service = try context.clients.shadowReview(configuration: configuration)
    }

    func handle(_ event: CoreEvent) -> Bool { service?.handle(event) ?? false }

    func shutDown() {}
}

enum OptionalCapabilitiesReplies {
    /// A chat-completions reply carrying a valid proposal, as the verified self-hosted endpoint returns.
    static func selfHosted() throws -> Data {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": 4]],
        ]
        let content = String(decoding: try JSONSerialization.data(withJSONObject: proposal), as: UTF8.self)
        let reply: [String: Any] = [
            "id": "chatcmpl-optional-01",
            "object": "chat.completion",
            "model": SelfHostedVerifiedBoundary.current.model,
            "choices": [
                ["index": 0, "finish_reason": "stop", "message": ["role": "assistant", "content": content]]
            ],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }

    /// An Anthropic reply carrying a review proposal for `itemType`.
    static func review(itemType: String) throws -> Data {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": itemType,
            "source_spans": [["start": 0, "end": 4]],
        ]
        let reply: [String: Any] = [
            "id": "msg_optional_review_01",
            "type": "message",
            "role": "assistant",
            "model": ShadowReviewScenario.reviewModel,
            "content": [["type": "tool_use", "id": "toolu_optional_review_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }
}

/// Builds production roots over a temporary storage root and checks every core handle is closed afterwards.
class OptionalCapabilitiesTestCase: XCTestCase {
    private(set) var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var authenticationBoundaries: [FakeReadAuthenticationBoundary] = []

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("optional-capabilities-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    var layout: ProtectedStorageLayout { ProtectedStorageLayout(rootDirectory: storageRoot) }
    var databasePath: String { layout.databaseURL.path }

    func prepareSelfHostedStore(_ scenario: SelfHostedScenario = SelfHostedScenario()) throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        _ = try SelfHostedSeed.makeStore(in: layout.directory(for: .database), scenario: scenario)
    }

    func prepareShadowReviewStore(
        _ scenario: ShadowReviewScenario = ShadowReviewScenario(credentialReference: "synthetic-credential")
    ) throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        _ = try ShadowReviewSeed.makeStore(in: layout.directory(for: .database), scenario: scenario)
    }

    func makeRoot(sender: ProviderRequestSender, modules: [CompositionModule]) throws -> ServiceCompositionRoot {
        let boundary = FakeReadAuthenticationBoundary()
        authenticationBoundaries.append(boundary)
        let session = ReadAuthenticationSession(boundary: boundary)
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: SelfHostedScenario.routeID,
                jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender,
                attachProviderTransport: { try ProviderExchangeCoordinator.attach(to: $0, sender: $1) },
                readSession: session, notificationCenter: FakeNotificationCenter(),
                reachability: FakeReachability(reachable: true), lifecycleNotifications: NotificationCenter()),
            modules: modules)
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 15) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Lets the job runner finish a full drain after `root.start`, so "nothing was sent" is asserted after the work
    /// was given its chance rather than before it began.
    func waitForDrain(_ root: ServiceCompositionRoot) -> Bool {
        pump(until: { root.jobRunner.finishedDrainCount >= 1 }, timeout: 30)
    }

    func jobStatus() -> String? {
        JobStoreInspector.job(at: databasePath, id: SelfHostedScenario.jobID)?.status
    }

    func shadowReviewConfiguration(profileVersion: String? = ShadowReviewScenario.profileVersion) -> ShadowReviewConfiguration {
        ShadowReviewConfiguration(
            reviewProfileVersion: profileVersion,
            policy: ShadowReviewPolicy(
                isEnabled: true, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 4,
                maxAttemptsPerSample: 2))
    }

    func verifiedRegistration() -> SelfHostedRegistration {
        SelfHostedRegistration(
            profileVersion: SelfHostedScenario.profileVersion, model: SelfHostedVerifiedBoundary.current.model)
    }

    func selectCase(_ service: ShadowReviewService) throws -> String {
        var selection: ShadowReviewSelectionResult?
        service.offer(
            itemID: ShadowReviewScenario.itemID, sourceRevision: 0, requestVersion: ShadowReviewScenario.requestVersion
        ) { selection = $0 }
        XCTAssertTrue(pump(until: { selection != nil }), "the offer is answered")
        guard case .selected(let record)? = selection else {
            XCTFail("expected the case to be selected, got \(String(describing: selection))")
            throw XCTSkip("no selected case")
        }
        return record.jobID
    }
}

extension OptionalCapabilitiesTestCase {
    /// Applies statements to the root's store, the way a profile revocation or route edit would.
    func runSQL(_ statements: [String]) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, storeBusyTimeoutMilliseconds)
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "statement failed: \(detail)")
            if code != SQLITE_OK { throw SelfHostedSeed.SeedFailure.statementFailed }
        }
    }

    /// Gives work that must not happen a bounded chance to happen, so an empty `requests` list means something.
    func settle(_ seconds: TimeInterval = 1) {
        _ = pump(until: { false }, timeout: seconds)
    }
}
