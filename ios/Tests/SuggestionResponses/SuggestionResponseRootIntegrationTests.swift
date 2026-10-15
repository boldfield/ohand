import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: suggestion responses never reach a provider.
private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

private final class SuggestionResultBox<Value>: @unchecked Sendable {
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

/// Drives `ServiceCompositionRoot.suggestionResponses` into the real Rust core over an on-disk store: the root's
/// assembly (client, read gate, router registration, operation range) and the core's eligibility, revision, replay and
/// capture-only rules.
final class SuggestionResponseRootIntegrationTests: XCTestCase {
    private static let routeID = "route-suggestion-responses"
    private static let captureText = "synthetic action for suggestions"
    private static let promptID = "prompt:2026-10-08"

    private var storageRoot: URL!
    private var session: ReadAuthenticationSession!
    private var activeStorage: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("suggestion-responses-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testActivationOffersTheEligibleActionWithItsTextAndNothingWhenNoneIsEligible() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let suggestions = root.suggestionResponses
        let uninterpretedID = try saveCapture(root)

        let before = try run { try await suggestions.activate(notificationID: Self.promptID) }
        XCTAssertEqual(before, .nothingToOffer, "an uninterpreted capture is never suggested")

        let revision = try makeAction(root, itemID: uninterpretedID)
        let offered = try run { try await suggestions.activate(notificationID: Self.promptID) }
        guard case .offer(let offer) = offered else { return XCTFail("expected an offer, got \(offered)") }
        XCTAssertEqual(offer.itemID, uninterpretedID)
        XCTAssertEqual(offer.revision, revision)
        XCTAssertEqual(offer.text, Self.captureText)
        XCTAssertEqual(offer.scope, .personal)
        XCTAssertEqual(offer.responses, [.notNow, .stopSuggesting, .done])

        XCTAssertThrowsError(try run { try await suggestions.activate(notificationID: "reminder:not-a-prompt") }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .invalidRequest)
        }
    }

    func testEachResponseIsStoredOnceAndRepeatedCommandsReplay() throws {
        for (response, eventType, lifecycle) in [
            (SuggestionResponseKind.notNow, "suggestion_control", "active"),
            (.stopSuggesting, "suggestion_control", "active"),
            (.done, "completion", "completed"),
        ] {
            let storage = storageRoot.appendingPathComponent(response.rawValue, isDirectory: true)
            try FileManager.default.createDirectory(at: storage, withIntermediateDirectories: true)
            let root = try makeRoot(authenticated: true, storage: storage)
            defer { root.shutDown() }
            let suggestions = root.suggestionResponses
            let itemID = try saveCapture(root)
            let revision = try makeAction(root, itemID: itemID)

            let first = try run { try await suggestions.respond(
                commandID: "cmd-1", itemID: itemID, expectedRevision: revision, response: response) }
            XCTAssertEqual(first, SuggestionResponseReceipt(replayed: false), "\(response)")
            let repeated = try run { try await suggestions.respond(
                commandID: "cmd-1", itemID: itemID, expectedRevision: revision, response: response) }
            XCTAssertEqual(repeated, SuggestionResponseReceipt(replayed: true), "\(response)")

            XCTAssertEqual(
                scalar("SELECT COUNT(*) FROM events WHERE event_id = 'prompt-response:cmd-1'"), "1")
            XCTAssertEqual(
                scalar("SELECT event_type FROM events WHERE event_id = 'prompt-response:cmd-1'"),
                eventType)
            XCTAssertEqual(
                scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), lifecycle)
        }
    }

    func testAStaleOrReplacedPromptIsReportedHonestlyAndAChangedCommandConflicts() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let suggestions = root.suggestionResponses
        let itemID = try saveCapture(root)
        let revision = try makeAction(root, itemID: itemID)
        _ = try run { try await suggestions.respond(
            commandID: "cmd-1", itemID: itemID, expectedRevision: revision, response: .stopSuggesting) }

        let current = try self.revision(of: itemID)
        XCTAssertThrowsError(try run { try await suggestions.respond(
            commandID: "cmd-2", itemID: itemID, expectedRevision: revision - 1, response: .notNow) }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .staleRevision(currentRevision: current))
        }
        XCTAssertThrowsError(try run { try await suggestions.respond(
            commandID: "cmd-1", itemID: itemID, expectedRevision: revision, response: .done) }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .commandConflict)
        }
        XCTAssertThrowsError(try run { try await suggestions.respond(
            commandID: "cmd-3", itemID: UUID().uuidString.lowercased(), expectedRevision: 0, response: .done) }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .noLongerCurrent)
        }
        let next = try run { try await suggestions.activate(notificationID: Self.promptID) }
        XCTAssertEqual(next, .nothingToOffer, "an item the user stopped suggesting is not offered again")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
    }

    func testACaptureOnlySessionDisclosesNothingAndChangesNothing() throws {
        let root = try makeRoot(authenticated: true)
        defer { root.shutDown() }
        let suggestions = root.suggestionResponses
        let itemID = try saveCapture(root)
        let revision = try makeAction(root, itemID: itemID)

        session.relock(.explicit)
        XCTAssertThrowsError(try run { try await suggestions.activate(notificationID: Self.promptID) }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await suggestions.respond(
            commandID: "cmd-1", itemID: itemID, expectedRevision: revision, response: .done) }) {
            XCTAssertEqual($0 as? SuggestionResponseError, .notAuthenticated)
        }
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM suggestion_eligibility"), "0", "nothing was selected")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_id LIKE 'prompt-response:%'"), "0")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
    }

    func testTheSuggestionRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.suggestionResponseClientName })
        XCTAssertEqual(mine.start, CoreSuggestionResponseClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: activeStorage).databaseURL.path
    }

    private func makeRoot(authenticated: Bool, storage: URL? = nil) throws -> ServiceCompositionRoot {
        let readSession = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            readSession.authenticate(reason: "test") { _ in }
            XCTAssertEqual(readSession.scope, .authenticated)
        }
        session = readSession
        activeStorage = storage ?? storageRoot
        let rootPath = activeStorage.path
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: rootPath, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(), readSession: readSession,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)))
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

    /// Makes the saved item an eligible action the way the user would: a type correction through the real corrections
    /// client. Returns the item's revision afterwards.
    private func makeAction(_ root: ServiceCompositionRoot, itemID: String) throws -> Int {
        let corrections = root.itemCorrections
        let start = try revision(of: itemID)
        _ = try run { try await corrections.correctType(
            commandID: "seed-type-\(itemID)", itemID: itemID, expectedRevision: start, itemType: .action) }
        return try revision(of: itemID)
    }

    private func revision(of itemID: String) throws -> Int {
        try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
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
        let box = SuggestionResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
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
}
