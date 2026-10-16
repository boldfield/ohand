import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class SettingsTestProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("a prompt settings change sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}

/// Drives the settings model over the real prompt service, the real core and the production-shaped root, faking only
/// the notification centre and the provider transport.
final class PromptSettingsRealCoreTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("prompt-settings-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func promptIdentifiers(_ identifiers: [String]) -> [String] {
        identifiers.filter { $0.hasPrefix("prompt:") }
    }

    private func makeRoot(center: FakeNotificationCenter) throws -> ServiceCompositionRoot {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-prompt-settings", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: SettingsTestProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true), lifecycleNotifications: NotificationCenter()),
            modules: [coordinator])
        root.start(isActive: false)
        return root
    }

    private static let canary = "canary-private-capture-text"

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func execute(_ sql: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, sql, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "statement failed: \(detail)")
    }

    /// Saves a capture containing the canary and gives it a scheduled-but-not-yet-installed explicit reminder, exactly
    /// as the core stores one before a reminder pass. Returns the reminder identifier.
    private func seedExplicitReminder(_ root: ServiceCompositionRoot) async throws -> String {
        let outcome: CompositionSaveOutcome = await withCheckedContinuation { continuation in
            root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.canary, capturedAt: Date()) {
                continuation.resume(returning: $0)
            }
        }
        guard case .saved(_, let itemID, _) = outcome else {
            XCTFail("expected a confirmed save, got \(outcome)")
            throw CoreFailure.handleClosed
        }
        let reminderID = UUID().uuidString.lowercased()
        let notificationID = "\(reminderID)#1"
        let dueInstant = "2090-01-01T09:00:00Z"
        execute(
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                + "'not_acknowledged', '\(dueInstant)', 'UTC', 1, '2026-10-08T09:30:02Z', '2026-10-08T09:30:02Z')")
        execute(
            "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                + "effect_identity, external_id, scheduled_for, created_at) VALUES "
                + "('schedule:\(notificationID)', '\(reminderID)', 'schedule', 'pending', "
                + "'schedule:\(notificationID)', '\(notificationID)', '\(dueInstant)', '2026-10-08T09:30:02Z')")
        return reminderID
    }

    private func pendingReminders(_ center: FakeNotificationCenter, _ reminderID: String) -> [String] {
        center.pendingIdentifiers.filter { $0.contains(reminderID) }
    }

    func testDefaultOffThenOnInstallsBoundedPromptsAndOffCancelsThemWithoutTouchingReminders() async throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(center: center)
        defer { root.shutDown() }
        let reminderID = try await seedExplicitReminder(root)
        let model = PromptSettingsModel(controller: root.promptSchedule)

        await model.load()
        XCTAssertFalse(model.isEnabled, "default is off")
        XCTAssertEqual(promptIdentifiers(center.addedRequests.map(\.identifier)), [])

        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled)
        XCTAssertEqual(model.notice, .saved)
        XCTAssertNotNil(model.nextPrompt, "the core's whole-second timestamp parses")
        let installed = promptIdentifiers(center.pendingIdentifiers).count
        XCTAssertGreaterThanOrEqual(installed, 1)
        XCTAssertLessThanOrEqual(installed, PromptSettingsModel.horizonDays, "no catch-up burst beyond the horizon")
        XCTAssertEqual(pendingReminders(center, reminderID).count, 1, "the explicit reminder is pending")
        for request in center.addedRequests where request.identifier.hasPrefix("prompt:") {
            XCTAssertFalse(request.content.title.contains(Self.canary))
            XCTAssertFalse(request.content.body.contains(Self.canary), "a prompt never carries stored text")
        }

        await model.setEnabled(false)
        XCTAssertFalse(model.isEnabled)
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), [], "turning prompts off cancels what was installed")
        XCTAssertEqual(pendingReminders(center, reminderID).count, 1, "turning prompts off leaves the reminder pending")
        let relaunched = PromptSettingsModel(controller: root.promptSchedule)
        await relaunched.load()
        XCTAssertFalse(relaunched.isEnabled, "the cancellation persists")
    }

    func testADeniedPermissionSavesTheChoiceExplainsItAndInstallsNothing() async throws {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .denied
        let root = try makeRoot(center: center)
        defer { root.shutDown() }
        let model = PromptSettingsModel(controller: root.promptSchedule)
        await model.load()
        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled, "the choice persists")
        XCTAssertEqual(model.notice, .savedNotificationsOff, "the user is told no prompt will appear")
        XCTAssertEqual(promptIdentifiers(center.addedRequests.map(\.identifier)), [], "nothing is installed")
    }

    func testAFullBudgetIsExplainedKeepsRemindersAndDoesNotBurstWhenRoomReturns() async throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(center: center)
        defer { root.shutDown() }
        let reminderID = try await seedExplicitReminder(root)
        // The usable budget is 60; other requests hold 59 slots and the explicit reminder takes the last one.
        let foreignIdentifiers = (0..<59).map { "foreign-request-\($0)" }
        for identifier in foreignIdentifiers {
            center.installForeignRequest(identifier: identifier, dueInstant: Date().addingTimeInterval(7200))
        }
        let model = PromptSettingsModel(controller: root.promptSchedule)
        await model.load()

        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled, "the choice persists")
        XCTAssertEqual(model.notice, .savedWaitingForRoom, "the user is told prompts wait for room")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), [], "no prompt takes a reminder's slot")
        XCTAssertEqual(pendingReminders(center, reminderID).count, 1, "the explicit reminder stays pending")

        center.removeImmediately(identifiers: foreignIdentifiers)
        await model.setTime(Calendar.current.date(bySettingHour: 8, minute: 15, second: 0, of: Date())!)
        XCTAssertEqual(model.notice, .saved)
        let installed = promptIdentifiers(center.pendingIdentifiers).count
        XCTAssertGreaterThanOrEqual(installed, 1)
        XCTAssertLessThanOrEqual(installed, PromptSettingsModel.horizonDays, "room returning causes no catch-up burst")
        XCTAssertEqual(pendingReminders(center, reminderID).count, 1)
    }
}
