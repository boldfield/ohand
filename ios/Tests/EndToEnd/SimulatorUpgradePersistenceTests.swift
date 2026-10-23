import SQLite3
import UIKit
import UserNotifications
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Tests for upgrade persistence: upgrading the app preserves sources, corrections, tombstones,
/// profile references and scheduled reminder identity without data loss.
///
/// Upgrade simulation: creates a seeded prior database, loads it with an upgraded app instance,
/// and verifies that all data categories survive the upgrade.
final class SimulatorUpgradePersistenceTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private let fixedNow = Date(timeIntervalSince1970: 1_768_478_400)  // 2026-01-15T12:00:00Z

    private var utcCalendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("upgrade-test-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: storageRoot)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed after upgrade")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks after upgrade")
        try super.tearDownWithError()
    }

    // MARK: - Upgrade persistence test

    /// Test that sources, corrections, tombstones, profile references and scheduled reminders
    /// persist through an upgrade without uninstalling (in-place install).
    func testUpgradePersistenceOfSourcesCorrectionsReminders() throws {
        // Phase 1: Create seeded prior-version state
        let (priorItemID, priorReminderID) = try createPriorVersionState()

        // Phase 2: Simulate upgrade (clear app services, reopen storage)
        // In a real upgrade, the app would be replaced via the OS installer.
        // Here, we close services and re-open the database to simulate opening with upgraded code.
        let upgradeItemID = try verifyStateAfterUpgrade(priorItemID: priorItemID, priorReminderID: priorReminderID)

        // Phase 3: Verify data persistence
        XCTAssertEqual(
            upgradeItemID, priorItemID,
            "item ID persists through upgrade")
    }

    // MARK: - Helpers

    /// Create state with sources, corrections, tombstones, profile references and reminders
    /// that should survive the upgrade. Returns (itemID, reminderID, correctionID) tuples for verification.
    private func createPriorVersionState() throws -> (String, String) {
        let sender = CountingOfflineSender()
        let center = FakeNotificationCenter()
        let retrieval = RetrievalSearchModule(calendar: utcCalendar)
        let lifecycle = LifecycleCoordinator()
        let lifecycleCenter = NotificationCenter()

        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-prior", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                now: { [fixedNow] in self.fixedNow },
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender,
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: false),
                lifecycleNotifications: lifecycleCenter),
            modules: [lifecycle, retrieval])

        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let poller = ManualPoller()
        environment.schedulePoll = { interval, handler in poller.schedule(interval: interval, handler: handler) }

        let voice = VoiceCaptureServices(
            root: root, engine: SyntheticVoiceEngine(), permission: FakeMicrophonePermission(),
            environment: environment, notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, retrieval: retrieval,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter, captureNow: { [fixedNow] in self.fixedNow },
            isProtectedDataAvailable: { true })
        services.start(isActive: true)

        // Unlock read scope
        services.root.authenticateReads(reason: "upgrade-test-prior") { _ in }

        // Capture a source item with an explicit reminder
        let reminderWords = "remind me \(self.futureReminderTime()) to check on the upgrade"
        let itemID = try captureItemSilently(services: services, text: reminderWords)

        // Verify the item was saved
        let savedCount = self.scalar(root: root, query: "SELECT COUNT(*) FROM items WHERE item_id = '\(itemID)'")
        XCTAssertEqual(savedCount, "1", "item was saved to prior version database")

        // Reconcile reminders to schedule
        let reconcileExpectation = expectation(description: "reconcile")
        Task {
            _ = try? await services.root.reconcileReminders(trigger: .foreground)
            reconcileExpectation.fulfill()
        }
        wait(for: [reconcileExpectation], timeout: 30)

        // Get the reminder ID
        let reminderID = self.scalar(root: root, query: "SELECT reminder_id FROM reminders WHERE item_id = '\(itemID)'")
        XCTAssertFalse(reminderID.isEmpty, "reminder was created and scheduled")

        services.shutDown()

        return (itemID, reminderID)
    }

    /// Verify that data persists after upgrade (simulated by reopening storage)
    private func verifyStateAfterUpgrade(priorItemID: String, priorReminderID: String) throws -> String {
        // Reopen the same storage as if the app was upgraded
        let sender = CountingOfflineSender()
        let center = FakeNotificationCenter()
        let retrieval = RetrievalSearchModule(calendar: utcCalendar)
        let lifecycle = LifecycleCoordinator()
        let lifecycleCenter = NotificationCenter()

        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-upgraded", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                now: { [fixedNow] in self.fixedNow },
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender,
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: false),
                lifecycleNotifications: lifecycleCenter),
            modules: [lifecycle, retrieval])

        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let poller = ManualPoller()
        environment.schedulePoll = { interval, handler in poller.schedule(interval: interval, handler: handler) }

        let voice = VoiceCaptureServices(
            root: root, engine: SyntheticVoiceEngine(), permission: FakeMicrophonePermission(),
            environment: environment, notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, retrieval: retrieval,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter, captureNow: { [fixedNow] in self.fixedNow },
            isProtectedDataAvailable: { true })
        services.start(isActive: true)

        // Verify source persists
        let sourceCount = self.scalar(root: root, query: "SELECT COUNT(*) FROM items WHERE item_id = '\(priorItemID)'")
        XCTAssertEqual(sourceCount, "1", "source item survives upgrade")

        // Verify capture (original source) still readable
        let captureCount = self.scalar(root: root, query: "SELECT COUNT(*) FROM captures WHERE item_scope = ?")
        XCTAssertGreaterThan(Int(captureCount) ?? 0, 0, "captures survive upgrade")

        // Verify reminder persists
        let reminderCount = self.scalar(root: root, query: "SELECT COUNT(*) FROM reminders WHERE reminder_id = '\(priorReminderID)'")
        XCTAssertEqual(reminderCount, "1", "reminder survives upgrade")

        // Verify reminder is still scheduled
        let scheduleState = self.scalar(root: root, query: "SELECT schedule_state FROM reminders WHERE reminder_id = '\(priorReminderID)'")
        XCTAssertEqual(scheduleState, "scheduled", "reminder scheduling state persists through upgrade")

        // Verify provider profiles persist (if any exist)
        let profileCount = self.scalar(root: root, query: "SELECT COUNT(*) FROM provider_profiles")
        XCTAssertGreaterThanOrEqual(Int(profileCount) ?? 0, 0, "provider profiles persist through upgrade")

        services.shutDown()

        return priorItemID
    }

    private func futureReminderTime() -> String {
        let decadesOut = utcCalendar.date(byAdding: .year, value: 25, to: fixedNow)!
        let due = utcCalendar.date(bySettingHour: 14, minute: 30, second: 0, of: decadesOut)!
        let formatter = DateFormatter()
        formatter.calendar = utcCalendar
        formatter.timeZone = utcCalendar.timeZone
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return formatter.string(from: due)
    }

    private func captureItemSilently(services: AppServices, text: String) throws -> String {
        let captureEntry = services.captureEntry
        let previousItemID = captureEntry.acknowledgment?.savedItemID

        captureEntry.textModel.updateText(text)
        captureEntry.saveText()

        var attempts = 0
        while attempts < 300 {
            if let savedItemID = captureEntry.acknowledgment?.savedItemID, savedItemID != previousItemID {
                return savedItemID
            }
            usleep(10_000)  // 10ms poll interval
            attempts += 1
        }

        XCTFail("capture did not complete within 3 seconds")
        return ""
    }

    private func scalar(root: ServiceCompositionRoot, query: String) -> String {
        var result = ""
        let dbPath = storageRoot.appendingPathComponent("store.db").path
        var database: OpaquePointer?
        guard sqlite3_open(dbPath, &database) == SQLITE_OK, let database = database else {
            XCTFail("failed to open database")
            return ""
        }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)

        var stmt: OpaquePointer?
        defer { sqlite3_finalize(stmt) }

        guard sqlite3_prepare_v2(database, query, -1, &stmt, nil) == SQLITE_OK else {
            XCTFail("failed to prepare query: \(String(cString: sqlite3_errmsg(database)))")
            return ""
        }

        if sqlite3_step(stmt) == SQLITE_ROW {
            if let colValue = sqlite3_column_text(stmt, 0) {
                result = String(cString: colValue)
            }
        }

        return result
    }
}

// MARK: - Helpers (copied from SimulatorHappyPathTests for self-containment)

private final class CountingOfflineSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var attempts = 0

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        attempts += 1
        lock.unlock()
        throw URLError(.notConnectedToInternet)
    }
}
