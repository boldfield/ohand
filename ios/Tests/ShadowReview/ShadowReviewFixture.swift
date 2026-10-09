import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Synthetic stored state for one finished interpretation and a separately approved review profile: the profile,
/// capture, item, route, route grants and the primary proposal a review is compared against. Fixture data only.
struct ShadowReviewScenario {
    static let profileVersion = "7c2d9a10-5b3e-4f61-9d0a-1e8f4a6b2c33"
    static let vendorOrigin = "https://api.anthropic.com"
    static let routeID = "route-synthetic"
    static let captureText = "call the roofer tomorrow at 9"
    static let reviewModel = "synthetic-review-model"
    static let itemID = "5a1c0000-0000-4000-8000-0000000000e1"
    static let requestVersion = "5a1c0000-0000-4000-8000-0000000000a1"

    var credentialReference: String
    var reviewGranted = true
    var primaryItemType = "action"
}

enum ShadowReviewSeed {
    private static let captureID = "5a1c0000-0000-4000-8000-0000000000c1"
    private static let proposalID = "5a1c0000-0000-4000-8000-0000000000b1"
    private static let retryPolicy = #"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#
    private static let capabilities =
        #"{"text_interpretation":{"capability":"text_interpretation","support":"supported","# +
        #""evidence":"ffi-fixtures:shadow_review","reason":null,"input_size_limit":500,"# +
        #""structured_output":"json_schema"}}"#

    /// Creates the store with the real core so its migrations run, closes it, then inserts the rows that import,
    /// profile installation and a finished interpretation would create.
    static func makeStore(in directory: URL, scenario: ShadowReviewScenario) throws -> String {
        let path = directory.appendingPathComponent("core.sqlite").path
        try CoreHandle(path: path).close()
        try execute(path: path, statements: seedStatements(scenario))
        return path
    }

    static func revokeReviewGrant(path: String) throws {
        try execute(path: path, statements: ["DELETE FROM route_authorizations WHERE capability = 'review'"])
    }

    /// Every row of every table a shadow verdict must never change, as comparable text.
    static func authoritativeSnapshot(path: String) throws -> [String] {
        var database: OpaquePointer?
        guard sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { throw SeedFailure.open }
        defer { sqlite3_close(database) }
        var rows: [String] = []
        for table in ["items", "proposals", "corrections", "route_authorizations", "provider_profiles", "captures"] {
            var statement: OpaquePointer?
            guard sqlite3_prepare_v2(database, "SELECT * FROM \(table) ORDER BY 1", -1, &statement, nil) == SQLITE_OK else {
                throw SeedFailure.statementFailed
            }
            defer { sqlite3_finalize(statement) }
            while sqlite3_step(statement) == SQLITE_ROW {
                let columns = (0..<sqlite3_column_count(statement)).map { index -> String in
                    sqlite3_column_text(statement, index).map { String(cString: $0) } ?? "NULL"
                }
                rows.append("\(table):" + columns.joined(separator: "|"))
            }
        }
        return rows
    }

    private static func seedStatements(_ scenario: ShadowReviewScenario) -> [String] {
        let profileVersion = quote(ShadowReviewScenario.profileVersion)
        let origins = quote(json([ShadowReviewScenario.vendorOrigin]))
        var statements = [
            """
            INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, \
            credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at, \
            revoked_at) VALUES (\(profileVersion), 'synthetic-review-profile', 'anthropic', NULL, \
            \(quote(ShadowReviewScenario.reviewModel)), \(quote(scenario.credentialReference)), 30, \
            \(quote(retryPolicy)), \(origins), \(quote(capabilities)), '2026-10-08T09:00:00Z', NULL)
            """,
            """
            INSERT INTO captures (capture_id, text, audio_reference, capture_instant, timezone_id, \
            utc_offset_minutes, locale, calendar, item_scope, route_id, entry_locked, created_at) VALUES \
            (\(quote(captureID)), \(quote(ShadowReviewScenario.captureText)), NULL, '2026-10-08T09:30:00Z', \
            'America/Chicago', -300, 'en_US', 'gregorian', 'personal', \(quote(ShadowReviewScenario.routeID)), 0, \
            '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, \
            processing_state, transcription_state, created_at, updated_at) VALUES \
            (\(quote(ShadowReviewScenario.itemID)), \(quote(captureID)), 0, 'active', 'saved_local', \
            'not_configured', 'unprocessed', 'not_applicable', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')
            """,
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            (\(quote(ShadowReviewScenario.routeID)), 'synthetic', 'personal', \(origins), '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, created_at) \
            VALUES ('auth-interpret', \(quote(ShadowReviewScenario.routeID)), 'text_interpretation', \(origins), \
            '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO proposals (proposal_id, item_id, capture_id, source_revision, schema_version, \
            text_basis_kind, text_basis_id, applied_state, proposal_type, reminder_proposal, \
            session_topic_proposal, source_spans, abstained, request_version, created_at) VALUES \
            (\(quote(proposalID)), \(quote(ShadowReviewScenario.itemID)), \(quote(captureID)), 0, 1, 'original', \
            NULL, 'applied', \(quote(scenario.primaryItemType)), NULL, NULL, '[]', 0, \
            \(quote(ShadowReviewScenario.requestVersion)), '2026-10-08T09:31:00Z')
            """,
        ]
        if scenario.reviewGranted {
            statements.append(
                """
                INSERT INTO route_authorizations (auth_id, route_id, capability, authorized_destinations, \
                created_at) VALUES ('auth-review', \(quote(ShadowReviewScenario.routeID)), 'review', \(origins), \
                '2026-10-08T09:00:00Z')
                """)
        }
        return statements
    }

    private static func execute(path: String, statements: [String]) throws {
        var database: OpaquePointer?
        guard sqlite3_open(path, &database) == SQLITE_OK else { throw SeedFailure.open }
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

    private static func json(_ strings: [String]) -> String {
        let data = (try? JSONEncoder().encode(strings)) ?? Data("[]".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// Answers every send with a scripted native failure, standing in for a transport that times out or is offline.
final class FailingSender: ProviderRequestSender {
    private let error: ProviderTransportError
    private let lock = NSLock()
    private var count = 0

    init(_ error: ProviderTransportError) { self.error = error }

    var sendCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        count += 1
        lock.unlock()
        throw error
    }
}

final class RecordingInstrumentation: ShadowReviewInstrumentation {
    private(set) var events: [ShadowReviewInstrumentationEvent] = []
    func shadowReview(_ event: ShadowReviewInstrumentationEvent) { events.append(event) }
}

final class ResultBox<Value> {
    var value: Value?
}

/// A real core over a seeded store with the native transport attached and the shadow review service routed from
/// the core's events. Events arrive on the main queue, so waiting pumps the run loop.
final class ShadowReviewSession {
    let core: CoreHandle
    let service: ShadowReviewService
    let storePath: String
    private let coordinator: ProviderExchangeCoordinator
    private(set) var events: [CoreEvent] = []
    private var isClosed = false

    init(storePath: String, sender: ProviderRequestSender, configuration: ShadowReviewConfiguration) throws {
        self.storePath = storePath
        core = try CoreHandle(path: storePath)
        coordinator = try ProviderExchangeCoordinator.attach(to: core, sender: sender)
        service = ShadowReviewService(core: core, configuration: configuration)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            self.events.append(event)
            XCTAssertTrue(self.service.handle(event), "every event belongs to the shadow review service")
        }
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 10) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    func startOffer() -> ResultBox<ShadowReviewSelectionResult> {
        let box = ResultBox<ShadowReviewSelectionResult>()
        service.offer(
            itemID: ShadowReviewScenario.itemID, sourceRevision: 0, requestVersion: ShadowReviewScenario.requestVersion
        ) { box.value = $0 }
        return box
    }

    func offer() -> ShadowReviewSelectionResult? {
        let box = startOffer()
        _ = pump { box.value != nil }
        return box.value
    }

    func startReview(jobID: String) -> ResultBox<ShadowReviewRunResult> {
        let box = ResultBox<ShadowReviewRunResult>()
        service.review(jobID: jobID) { box.value = $0 }
        return box
    }

    func review(jobID: String) -> ShadowReviewRunResult? {
        let box = startReview(jobID: jobID)
        _ = pump { box.value != nil }
        return box.value
    }

    func record(jobID: String) -> ShadowReviewReadResult? {
        let box = ResultBox<ShadowReviewReadResult>()
        service.record(jobID: jobID) { box.value = $0 }
        _ = pump { box.value != nil }
        return box.value
    }

    /// Every core event payload as text, to prove nothing sensitive crossed back to native code.
    var eventText: String {
        events.map { event -> String in
            switch event.outcome {
            case .success(let bytes): return String(decoding: bytes, as: UTF8.self)
            case .failure(let failure): return "\(failure.errorClass.rawValue) \(failure.code) \(failure.message)"
            }
        }.joined(separator: "\n")
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        coordinator.shutDown()
        core.close()
    }
}
