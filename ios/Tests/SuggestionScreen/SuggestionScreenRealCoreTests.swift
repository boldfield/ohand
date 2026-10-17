import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScreenTestProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("a suggestion response sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}

/// Drives the suggestion screen model over the real suggestion service, the real core and an on-disk store, and reads
/// the stored result back, including across a relaunch on the same storage.
final class SuggestionScreenRealCoreTests: XCTestCase {
    private static let promptID = "prompt:2026-10-08"
    private static let captureText = "synthetic action for the suggestion screen"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("suggestion-screen-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func makeRoot(authenticated: Bool = true) throws -> (ServiceCompositionRoot, ReadAuthenticationSession) {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-suggestion-screen", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: ScreenTestProviderSender(), readSession: session, notificationCenter: FakeNotificationCenter(),
                reachability: FakeReachability(reachable: true), lifecycleNotifications: NotificationCenter()),
            modules: [coordinator])
        root.start(isActive: false)
        return (root, session)
    }

    private func seedCapture(_ root: ServiceCompositionRoot, text: String = SuggestionScreenRealCoreTests.captureText) async throws -> String {
        let outcome: CompositionSaveOutcome = await withCheckedContinuation { continuation in
            root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) {
                continuation.resume(returning: $0)
            }
        }
        guard case .saved(_, let itemID, _) = outcome else {
            XCTFail("expected a confirmed save, got \(outcome)")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func seedAction(_ root: ServiceCompositionRoot) async throws -> String {
        let itemID = try await seedCapture(root)
        let start = try XCTUnwrap(Int(scalar("SELECT revision FROM items WHERE item_id = '\(itemID)'")))
        _ = try await root.itemCorrections.correctType(
            commandID: "seed-type-\(itemID)", itemID: itemID, expectedRevision: start, itemType: .action)
        return itemID
    }

    private func execute(_ sql: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        XCTAssertEqual(sqlite3_exec(database, sql, nil, nil, nil), SQLITE_OK, sql)
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

    /// Gives the item a scheduled explicit reminder, as the core stores one before a reminder pass.
    private func seedReminder(itemID: String) -> String {
        let reminderID = UUID().uuidString.lowercased()
        execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                + "'not_acknowledged', '2090-01-01T09:00:00Z', 'UTC', 1, '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
        return reminderID
    }

    func testStopSuggestingIsStoredPersistsAcrossRelaunchAndLeavesTheReminderAlone() async throws {
        var (root, _) = try makeRoot()
        let itemID = try await seedAction(root)
        // The reminder sits on a different item: an item with its own reminder is not suggested.
        let reminderItemID = try await seedCapture(root, text: "synthetic reminder item")
        let reminderID = seedReminder(itemID: reminderItemID)
        let reminderBefore = scalar("SELECT schedule_state || delivery_state FROM reminders WHERE reminder_id = '\(reminderID)'")

        let model = SuggestionScreenModel(responder: root.suggestionResponses)
        await model.open(notificationID: Self.promptID)
        guard case .offer(let offer) = model.state else { return XCTFail("expected an offer, got \(model.state)") }
        XCTAssertEqual(offer.itemID, itemID)
        XCTAssertEqual(offer.text, Self.captureText)

        await model.respond(.stopSuggesting)
        XCTAssertEqual(model.state, .responded(.stopSuggesting))
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_type = 'suggestion_control' AND event_id LIKE 'prompt-response:%'"), "1")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(
            scalar("SELECT schedule_state || delivery_state FROM reminders WHERE reminder_id = '\(reminderID)'"),
            reminderBefore, "the explicit reminder is unchanged")

        root.shutDown()
        (root, _) = try makeRoot()
        defer { root.shutDown() }
        let relaunched = SuggestionScreenModel(responder: root.suggestionResponses)
        await relaunched.open(notificationID: Self.promptID)
        XCTAssertEqual(relaunched.state, .nothingToOffer, "the stop persists and nothing else is offered")
    }

    func testNotNowStoresTheCooldownPersistsAcrossRelaunchAndLeavesAReminderAddedAfterActivationAlone() async throws {
        var (root, _) = try makeRoot()
        let itemID = try await seedAction(root)
        let model = SuggestionScreenModel(responder: root.suggestionResponses)
        await model.open(notificationID: Self.promptID)
        guard case .offer(let offer) = model.state else { return XCTFail("expected an offer, got \(model.state)") }
        XCTAssertEqual(offer.itemID, itemID)

        // The offered item gains an explicit reminder after the screen opened and before the answer.
        let reminderID = seedReminder(itemID: itemID)
        let reminderBefore = scalar("SELECT schedule_state || delivery_state FROM reminders WHERE reminder_id = '\(reminderID)'")

        await model.respond(.notNow)
        XCTAssertEqual(
            scalar("SELECT schedule_state || delivery_state FROM reminders WHERE reminder_id = '\(reminderID)'"),
            reminderBefore, "the explicit reminder is unchanged whatever the screen resolved to")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_type = 'completion'"), "0")
        XCTAssertEqual(model.state, .responded(.notNow))
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_type = 'suggestion_control' AND event_id LIKE 'prompt-response:%'"), "1")

        // A second item shows the stored cooldown across a relaunch.
        root.shutDown()
        (root, _) = try makeRoot()
        let secondID = try await seedAction(root)
        let second = SuggestionScreenModel(responder: root.suggestionResponses)
        await second.open(notificationID: Self.promptID)
        guard case .offer(let secondOffer) = second.state else { return XCTFail("expected an offer, got \(second.state)") }
        XCTAssertEqual(secondOffer.itemID, secondID, "the reminded item is not offered")
        await second.respond(.notNow)
        XCTAssertEqual(second.state, .responded(.notNow))
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_type = 'suggestion_control' AND event_id LIKE 'prompt-response:%'"), "2")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(secondID)'"), "active")

        root.shutDown()
        (root, _) = try makeRoot()
        defer { root.shutDown() }
        let relaunched = SuggestionScreenModel(responder: root.suggestionResponses)
        await relaunched.open(notificationID: Self.promptID)
        XCTAssertEqual(relaunched.state, .nothingToOffer, "the cooldown persists across the relaunch")
    }

    func testDoneCompletesTheItemAndAStaleScreenResolvesToNothing() async throws {
        let (root, _) = try makeRoot()
        defer { root.shutDown() }
        let itemID = try await seedAction(root)
        let model = SuggestionScreenModel(responder: root.suggestionResponses)
        await model.open(notificationID: Self.promptID)
        guard case .offer = model.state else { return XCTFail("expected an offer, got \(model.state)") }

        // Another surface completes the item while this screen still shows it.
        let stale = SuggestionScreenModel(responder: root.suggestionResponses)
        await stale.open(notificationID: Self.promptID)
        await model.respond(.done)
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")

        await stale.respond(.notNow)
        XCTAssertEqual(stale.state, .nothingToOffer, "the completed item is resolved again, not retried")
        XCTAssertEqual(stale.notice, .changed)
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "completed")
    }

    func testACaptureOnlySessionShowsNothingAndStoresNothing() async throws {
        let (root, session) = try makeRoot()
        defer { root.shutDown() }
        let itemID = try await seedAction(root)
        session.relock(.explicit)
        let model = SuggestionScreenModel(responder: root.suggestionResponses)
        await model.open(notificationID: Self.promptID)
        XCTAssertEqual(model.state, .locked)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM events WHERE event_type IN ('suggestion_control', 'completion')"), "0")
        XCTAssertEqual(scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'"), "active")
    }
}
