import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class OfflineProviderSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var attempts = 0

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    private func recordAttempt() {
        lock.lock()
        attempts += 1
        lock.unlock()
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        recordAttempt()
        throw URLError(.notConnectedToInternet)
    }
}

private final class SearchOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<RetrievalPage, RetrievalSearchFailure>?

    var value: Result<RetrievalPage, RetrievalSearchFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

/// Drives `RetrievalSearchModule` through the real composition root and Rust core over an on-disk store, with every
/// provider unreachable: search must work from the device's own store alone.
final class RetrievalSearchModuleIntegrationTests: XCTestCase {
    private static let routeID = "route-retrieval-search"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var session: ReadAuthenticationSession!
    private var sender: OfflineProviderSender!
    private var module: RetrievalSearchModule!
    private var root: ServiceCompositionRoot!

    private var utcCalendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("retrieval-search-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        sender = OfflineProviderSender()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        module = RetrievalSearchModule(calendar: utcCalendar)
        root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false)),
            modules: [module])
    }

    override func tearDownWithError() throws {
        root.shutDown()
        root = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: scope

    func testACaptureOnlySessionIsRefusedBeforeAnythingReachesTheCore() throws {
        _ = try saveCapture("synthetic roofer words")

        let outcome = search(RetrievalSearchRequest(query: "roofer"))

        XCTAssertEqual(outcome.failure, .notAuthenticated)
        XCTAssertNil(outcome.page, "no count and no snippet without authentication")
    }

    func testAnAnswerInFlightWhenTheSessionRelocksIsDiscarded() throws {
        _ = try saveCapture("synthetic roofer words")
        authenticate()

        let box = SearchOutcomeBox()
        module.search(RetrievalSearchRequest(query: "roofer")) { box.set($0) }
        session.relock(.explicit)
        XCTAssertTrue(pump(until: { box.value != nil }))

        guard case .failure(let failure)? = box.value else { return XCTFail("a relocked answer was delivered") }
        XCTAssertEqual(failure, .relocked)
    }

    func testRelockingThenAuthenticatingAgainSearchesAgain() throws {
        _ = try saveCapture("synthetic roofer words")
        authenticate()
        session.relock(.explicit)
        XCTAssertEqual(search(RetrievalSearchRequest(query: "roofer")).failure, .notAuthenticated)

        authenticate()
        XCTAssertEqual(search(RetrievalSearchRequest(query: "roofer")).page?.rows.count, 1)
    }

    // MARK: offline words

    func testLiteralSearchFindsOriginalWordsOfflineWithDateScopeAndBasis() throws {
        let capturedAt = Date().addingTimeInterval(-3600)
        let itemID = try saveCapture("synthetic roofer words", capturedAt: capturedAt)
        _ = try saveCapture("unrelated gardening words")
        authenticate()

        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).page)

        XCTAssertEqual(page.totalAccessible, 1)
        let row = try XCTUnwrap(page.rows.first)
        XCTAssertEqual(row.reference.value, itemID)
        XCTAssertEqual(row.text, "synthetic roofer words")
        XCTAssertEqual(row.basis, .original)
        XCTAssertNil(row.originalText)
        XCTAssertEqual(row.matched, .current)
        XCTAssertEqual(row.snippet.match.lowercased(), "roofer")
        XCTAssertEqual(row.scopeName, "Personal")
        XCTAssertEqual(row.capturedAt.timeIntervalSince(capturedAt), 0, accuracy: 1)
        XCTAssertEqual(sender.attemptCount, 0, "search asked no provider anything")
    }

    func testNoMatchIsAnEmptyPageWithAZeroCount() throws {
        _ = try saveCapture("synthetic roofer words")
        authenticate()

        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "zebra")).page)

        XCTAssertEqual(page.totalAccessible, 0)
        XCTAssertTrue(page.rows.isEmpty)
    }

    func testACorrectedItemShowsTheCorrectionAndFindsTheReplacedOriginal() throws {
        authenticate()
        let itemID = try saveCapture("synthetic rooter words")
        let start = try revision(of: itemID)
        let corrections = root.itemCorrections
        _ = try runCorrection { try await corrections.correctText(
            commandID: "cmd-text", itemID: itemID, expectedRevision: start, text: "synthetic roofer words") }

        let current = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).page?.rows.first)
        XCTAssertEqual(current.basis, .corrected)
        XCTAssertEqual(current.text, "synthetic roofer words")
        XCTAssertEqual(current.originalText, "synthetic rooter words")
        XCTAssertEqual(current.matched, .current)

        let replaced = try XCTUnwrap(search(RetrievalSearchRequest(query: "rooter")).page?.rows.first)
        XCTAssertEqual(replaced.reference.value, itemID)
        XCTAssertEqual(replaced.text, "synthetic roofer words", "the current text is shown, never the replaced original")
        XCTAssertEqual(replaced.matched, .supersededOriginal)
        XCTAssertEqual(replaced.snippet.match, "")
    }

    // MARK: filters

    func testDateFiltersSelectByCaptureDayInclusively() throws {
        let tenDaysAgo = Date().addingTimeInterval(-10 * 86_400)
        let threeDaysAgo = Date().addingTimeInterval(-3 * 86_400)
        let oldID = try saveCapture("synthetic old words", capturedAt: tenDaysAgo)
        let recentID = try saveCapture("synthetic recent words", capturedAt: threeDaysAgo)
        authenticate()

        let sinceFiveDays = RetrievalSearchRequest(since: Date().addingTimeInterval(-5 * 86_400))
        XCTAssertEqual(try ids(sinceFiveDays), [recentID])

        let untilFiveDays = RetrievalSearchRequest(until: Date().addingTimeInterval(-5 * 86_400))
        XCTAssertEqual(try ids(untilFiveDays), [oldID])

        let theWholeDayOfTheRecentCapture = RetrievalSearchRequest(since: threeDaysAgo, until: threeDaysAgo)
        XCTAssertEqual(try ids(theWholeDayOfTheRecentCapture), [recentID])

        let tomorrow = RetrievalSearchRequest(query: "synthetic", since: Date().addingTimeInterval(86_400))
        let empty = try XCTUnwrap(search(tomorrow).page)
        XCTAssertEqual(empty.totalAccessible, 0)
    }

    func testSessionTopicsSinceADateListWithoutWordsOrAnyProvider() throws {
        authenticate()
        let garageID = try saveCapture("synthetic garage shelf", capturedAt: Date().addingTimeInterval(-2 * 86_400))
        let kitchenID = try saveCapture("synthetic kitchen tap", capturedAt: Date().addingTimeInterval(-2 * 86_400))
        let oldGarageID = try saveCapture("synthetic old garage", capturedAt: Date().addingTimeInterval(-20 * 86_400))
        let untopicID = try saveCapture("synthetic loose thought", capturedAt: Date().addingTimeInterval(-2 * 86_400))
        let corrections = root.itemCorrections
        for (index, pair) in [(garageID, "Garage"), (kitchenID, "kitchen"), (oldGarageID, "garage")].enumerated() {
            let start = try revision(of: pair.0)
            _ = try runCorrection { try await corrections.correctSessionTopic(
                commandID: "cmd-topic-\(index)", itemID: pair.0, expectedRevision: start, sessionTopic: pair.1) }
        }
        let lastWeek = Date().addingTimeInterval(-7 * 86_400)

        XCTAssertEqual(
            try ids(RetrievalSearchRequest(sessionTopic: "garage", since: lastWeek)), [garageID],
            "a topic matches whatever its case, and the date drops the older item")
        XCTAssertEqual(
            Set(try ids(RetrievalSearchRequest(onlyWithSessionTopic: true, since: lastWeek))),
            [garageID, kitchenID], "only items that have a topic")
        XCTAssertFalse(try ids(RetrievalSearchRequest(since: lastWeek)).isEmpty)
        XCTAssertTrue(try ids(RetrievalSearchRequest(since: lastWeek)).contains(untopicID))
        XCTAssertEqual(sender.attemptCount, 0)
    }

    func testTheTypeFilterNarrowsAndTheRowsNameTheType() throws {
        authenticate()
        let ideaID = try saveCapture("synthetic idea words")
        _ = try saveCapture("synthetic other words")
        let start = try revision(of: ideaID)
        let corrections = root.itemCorrections
        _ = try runCorrection { try await corrections.correctType(
            commandID: "cmd-type", itemID: ideaID, expectedRevision: start, itemType: .idea) }

        let ideas = try XCTUnwrap(search(RetrievalSearchRequest(query: "synthetic", itemType: .idea)).page)
        XCTAssertEqual(ideas.rows.map(\.reference.value), [ideaID])
        XCTAssertEqual(ideas.rows.first?.itemType, .idea)
        XCTAssertEqual(ideas.totalAccessible, 1)
    }

    func testPagingContinuesFromTheOffset() throws {
        for index in 0..<(RetrievalSearchRequest.pageSize + 2) {
            _ = try saveCapture("synthetic paged words \(index)")
        }
        authenticate()

        let first = try XCTUnwrap(search(RetrievalSearchRequest(query: "paged")).page)
        XCTAssertEqual(first.rows.count, RetrievalSearchRequest.pageSize)
        XCTAssertEqual(first.totalAccessible, RetrievalSearchRequest.pageSize + 2)
        XCTAssertTrue(first.hasMore)
        let second = try XCTUnwrap(
            search(RetrievalSearchRequest(query: "paged", offset: RetrievalSearchRequest.pageSize)).page)
        XCTAssertEqual(second.rows.count, 2)
        XCTAssertFalse(second.hasMore)
        XCTAssertTrue(Set(first.rows.map(\.id)).isDisjoint(with: second.rows.map(\.id)))
    }

    // MARK: module

    func testARequestTheCoreWouldRefuseNeverLeavesTheModule() throws {
        authenticate()
        let backwards = RetrievalSearchRequest(since: Date(), until: Date().addingTimeInterval(-3 * 86_400))
        XCTAssertEqual(search(backwards).failure, .invalidRequest)
        XCTAssertEqual(search(RetrievalSearchRequest(query: "  ")).failure, .invalidRequest)
    }

    func testTheModuleRangeIsAcceptedByTheRootAndDisjointFromTheReservedClients() {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let end = module.firstOperationID + ServiceCompositionRoot.operationRangeWidth
        let overlapping = ranges.filter { $0.start < end && module.firstOperationID < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    func testAnUnattachedModuleSaysSearchIsUnavailable() {
        let unattached = RetrievalSearchModule()
        let box = SearchOutcomeBox()
        unattached.search(RetrievalSearchRequest(query: "roofer")) { box.set($0) }
        guard case .failure(let failure)? = box.value else { return XCTFail("expected a failure") }
        XCTAssertEqual(failure, .unavailable)
    }

    func testShuttingDownFailsASearchStillWaiting() throws {
        _ = try saveCapture("synthetic roofer words")
        authenticate()
        let box = SearchOutcomeBox()
        module.search(RetrievalSearchRequest(query: "roofer")) { box.set($0) }

        module.shutDown()

        guard case .failure(let failure)? = box.value else { return XCTFail("a waiting search must be failed") }
        XCTAssertEqual(failure, .unavailable)
        let after = SearchOutcomeBox()
        module.search(RetrievalSearchRequest(query: "roofer")) { after.set($0) }
        guard case .failure(let laterFailure)? = after.value else { return XCTFail("later searches are refused") }
        XCTAssertEqual(laterFailure, .unavailable)
    }

    // MARK: helpers

    private func authenticate() {
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    private struct SearchOutcome {
        let result: Result<RetrievalPage, RetrievalSearchFailure>

        var page: RetrievalPage? { try? result.get() }
        var failure: RetrievalSearchFailure? {
            if case .failure(let failure) = result { return failure }
            return nil
        }
    }

    private func search(_ request: RetrievalSearchRequest) -> SearchOutcome {
        let box = SearchOutcomeBox()
        module.search(request) { box.set($0) }
        XCTAssertTrue(pump(until: { box.value != nil }), "the search never completed")
        return SearchOutcome(result: box.value ?? .failure(.unavailable))
    }

    private func ids(_ request: RetrievalSearchRequest) throws -> [String] {
        try XCTUnwrap(search(request).page).rows.map(\.reference.value)
    }

    private func saveCapture(_ text: String, capturedAt: Date = Date()) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: capturedAt) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func runCorrection(
        _ operation: @escaping @Sendable () async throws -> ItemCorrectionReceipt
    ) throws -> ItemCorrectionReceipt {
        let finished = expectation(description: "correction")
        let box = CorrectionBox()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 30)
        return try XCTUnwrap(box.value).get()
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

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }
}

private final class CorrectionBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<ItemCorrectionReceipt, Error>?

    var value: Result<ItemCorrectionReceipt, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<ItemCorrectionReceipt, Error>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}
