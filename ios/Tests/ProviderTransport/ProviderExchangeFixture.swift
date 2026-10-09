import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Synthetic stored state for one provider job, written the way the import and profile installation will: rows for
/// the profile, capture, item, route, route authorization and job. Everything is fixture data.
struct ProviderExchangeScenario {
    static let profileVersion = "4b7a1bfe-88ff-4ba7-848e-a45069cf9080"
    static let jobID = "job-synthetic-1"
    static let vendorOrigin = "https://api.anthropic.com"
    static let otherVendorOrigin = "https://api.openai.com"

    var credentialReference: String
    var routeDestinations: [String] = [ProviderExchangeScenario.vendorOrigin]
    var authorizedDestinations: [String] = [ProviderExchangeScenario.vendorOrigin]
    var profileRevoked = false
    var captureText = "call the roofer tomorrow at 9"
    /// When set, the item is at revision 1 with this user text correction and the job is pinned to revision 1.
    var correctedText: String?
    var revision: Int { correctedText == nil ? 0 : 1 }
}

enum ProviderExchangeSeed {
    private static let captureID = "5a1c0000-0000-4000-8000-0000000000c1"
    private static let itemID = "5a1c0000-0000-4000-8000-0000000000e1"
    private static let requestVersion = "5a1c0000-0000-4000-8000-0000000000a1"
    private static let routeID = "route-synthetic"
    static let correctionEventID = "5a1c0000-0000-4000-8000-0000000000c9"
    private static let retryPolicy = #"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#
    private static let capabilities =
        #"{"text_interpretation":{"capability":"text_interpretation","support":"supported","# +
        #""evidence":"ffi-fixtures:provider_transport","reason":null,"input_size_limit":500,"# +
        #""structured_output":"json_schema"}}"#

    /// Creates the store with the real core so its migrations run, closes it, then inserts the rows that a
    /// foreground import would create. Returns the store path for a fresh core to open.
    static func makeStore(in directory: URL, scenario: ProviderExchangeScenario) throws -> String {
        let path = directory.appendingPathComponent("core.sqlite").path
        try CoreHandle(path: path).close()
        try seed(path: path, scenario: scenario)
        return path
    }

    private static func seed(path: String, scenario: ProviderExchangeScenario) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        let revokedAt = scenario.profileRevoked ? "'2026-10-08T09:40:00Z'" : "NULL"
        let profileDestinations = quote(json([ProviderExchangeScenario.vendorOrigin]))
        let routeDestinations = quote(json(scenario.routeDestinations))
        let authorizedDestinations = quote(json(scenario.authorizedDestinations))
        let profileVersion = quote(ProviderExchangeScenario.profileVersion)
        let credentialReference = quote(scenario.credentialReference)
        let captureText = quote(scenario.captureText)
        let statements = [
            """
            INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, \
            credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at, \
            revoked_at) VALUES (\(profileVersion), 'synthetic-profile', 'anthropic', NULL, 'synthetic-model', \
            \(credentialReference), 30, \(quote(retryPolicy)), \(profileDestinations), \(quote(capabilities)), \
            '2026-10-08T09:00:00Z', \(revokedAt))
            """,
            """
            INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, \
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) VALUES \
            (\(quote(captureID)), \(captureText), NULL, '2026-10-08T09:30:00Z', 'America/Chicago', -300, \
            'en_US', 'gregorian', 'personal', \(quote(routeID)), 0, '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, \
            processing_state, transcription_state, created_at, updated_at) VALUES (\(quote(itemID)), \
            \(quote(captureID)), \(scenario.revision), 'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', \
            '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            (\(quote(routeID)), 'synthetic', 'personal', \(routeDestinations), '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) VALUES \
            (\(quote(routeID)), 'text_interpretation', \(authorizedDestinations), '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES (\(quote(ProviderExchangeScenario.jobID)), \
            1, \(quote(itemID)), 'interpret', \(scenario.revision), \(profileVersion), \(quote(requestVersion)), 'queued', 0, \
            '2026-10-08T09:30:02Z')
            """,
        ]
        var allStatements = statements
        if let correctedText = scenario.correctedText {
            allStatements.append(
                """
                INSERT INTO corrections (correction_id, item_id, revision, kind, old_value, new_value, created_at) \
                VALUES (\(quote(correctionEventID + "-correction")), \(quote(itemID)), 1, 'text', \
                \(captureText), \(quote(correctedText)), '2026-10-08T09:35:00Z')
                """)
        }
        for statement in allStatements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw SeedFailure.statementFailed }
        }
    }

    enum SeedFailure: Error {
        case statementFailed
    }

    private static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "''") + "'"
    }

    private static func json(_ strings: [String]) -> String {
        let data = (try? JSONEncoder().encode(strings)) ?? Data("[]".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// Stands in for the production URL: the core targets the vendor's real endpoint, which a test cannot reach, so
/// this sender moves the request onto the loopback fixture server and trusts that server's origin instead. Every
/// other property of the request, the credential attachment included, goes through the real
/// `ProviderHTTPTransport` unchanged, and the request as the core described it is recorded first.
final class FixtureRoutingSender: ProviderRequestSender {
    private let underlying: ProviderHTTPTransport
    private let fixtureOrigin: String
    private let fixturePort: UInt16
    private let lock = NSLock()
    private var recorded: [ProviderHTTPRequest] = []

    init(underlying: ProviderHTTPTransport, fixtureServer: FixtureTLSServer) {
        self.underlying = underlying
        self.fixtureOrigin = fixtureServer.origin
        self.fixturePort = fixtureServer.port
    }

    var requestsFromCore: [ProviderHTTPRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        recorded.append(request)
        lock.unlock()

        guard var components = URLComponents(url: request.url, resolvingAgainstBaseURL: false) else {
            throw ProviderTransportError.invalidRequest(.malformedURL)
        }
        components.host = "localhost"
        components.port = Int(fixturePort)
        guard let rewrittenURL = components.url else {
            throw ProviderTransportError.invalidRequest(.malformedURL)
        }
        return try await underlying.send(
            ProviderHTTPRequest(
                url: rewrittenURL, method: request.method, headers: request.headers, body: request.body,
                timeout: request.timeout, maxResponseBytes: request.maxResponseBytes, credential: request.credential,
                authorization: ProviderTransportAuthorization(
                    jobID: request.authorization.jobID, capability: request.authorization.capability,
                    authorizedOrigins: [fixtureOrigin])))
    }
}

/// A real core over a seeded store with the native transport attached. Events arrive on the main queue, where
/// XCTest runs synchronous tests, so waiting pumps the run loop.
final class ProviderExchangeSession {
    let core: CoreHandle
    private var coordinator: ProviderExchangeCoordinator
    private(set) var events: [CoreEvent] = []
    private var nextOperationID: UInt64 = 1
    private var isClosed = false

    init(storePath: String, sender: ProviderRequestSender) throws {
        core = try CoreHandle(path: storePath)
        coordinator = try ProviderExchangeCoordinator.attach(to: core, sender: sender)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            self.events.append(event)
        }
    }

    /// Attaches a second coordinator, which replaces the first as the core's transport, then shuts the first down
    /// so its registration is invalidated after having been superseded.
    func replaceTransportThenShutDownTheOldCoordinator(sender: ProviderRequestSender) throws {
        let superseded = coordinator
        coordinator = try ProviderExchangeCoordinator.attach(to: core, sender: sender)
        superseded.shutDown()
    }

    func startExchange(jobID: String = ProviderExchangeScenario.jobID) throws -> UInt64 {
        let operationID = nextOperationID
        nextOperationID += 1
        try core.startProviderExchange(operationID: operationID, jobID: jobID)
        return operationID
    }

    func events(for operationID: UInt64) -> [CoreEvent] {
        events.filter { $0.operationID == operationID }
    }

    /// Waits for the event that ends the exchange: a failure, or the `completed` phase.
    func finalEvent(for operationID: UInt64, timeout: TimeInterval = 10) -> CoreEvent? {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let final = events(for: operationID).first(where: Self.endsExchange) { return final }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return nil
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 10) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        coordinator.shutDown()
        core.close()
    }

    private static func endsExchange(_ event: CoreEvent) -> Bool {
        switch event.outcome {
        case .failure:
            return true
        case .success:
            return (try? event.decode(ProviderExchangeEvent.self))?.phase == .completed
        }
    }
}
