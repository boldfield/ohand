import Foundation
import SQLite3
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private let tokyo = TimeZone(identifier: "Asia/Tokyo")!
private let losAngeles = TimeZone(identifier: "America/Los_Angeles")!

/// A provider transport for tests that never expect a provider request.
private final class UnusedProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("a time-change pass sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}

/// Runs the time-change path over a production-shaped root: the real Rust core and the real reminder services, with a
/// fake only at the operating system's edges (the notification center, the system clock callbacks and the clocks).
final class TimeChangeRealCoreTests: XCTestCase {
    private static let routeID = "route-time-change"
    private static let futureItemID = "7d1c5a1e-0000-4000-8000-0000000000b1"
    private static let futureReminderID = "3f2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4b01"
    private static let overdueItemID = "7d1c5a1e-0000-4000-8000-0000000000b2"
    private static let overdueReminderID = "3f2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4b02"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("time-change-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    func testZoneAndClockChangesKeepTheResolvedInstantInstallItOnceAndNeverCatchUpAnOverdueReminder() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        let futureInstant = Self.wholeSecond(Date().addingTimeInterval(2 * 86_400))
        let overdueInstant = Self.wholeSecond(Date().addingTimeInterval(-3_600))
        try prepareStore(layout, futureInstant: futureInstant, overdueInstant: overdueInstant)

        let time = ManualTime(wall: Date(), zone: TimeZone(identifier: "America/New_York")!)
        let center = FakeNotificationCenter()
        let lifecycle = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [lifecycle], notificationCenter: center, time: time)
        defer { root.shutDown() }
        try lifecycle.connect(to: root)
        let systemCallbacks = NotificationCenter()
        let coordinator = TimeChangeCoordinator(
            lifecycle: lifecycle, notificationCenter: systemCallbacks, sources: time.sources)
        coordinator.start()
        defer { coordinator.stop() }

        // The person flies east and the OS repeats its callbacks for the one change.
        time.travel(to: tokyo)
        for _ in 0..<3 { systemCallbacks.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        systemCallbacks.post(name: UIApplication.significantTimeChangeNotification, object: nil)
        await settleTimers(time, count: 4)
        let firstPass = await timeChangeWaitUntil { lifecycle.finishedPassCount == 1 }
        XCTAssertTrue(firstPass)
        XCTAssertEqual(lifecycle.lastReport?.triggers, [.systemTimeChange])
        XCTAssertEqual(lifecycle.lastReport?.results.map(\.step), [.reminders, .promptEligibility])

        let futureNotificationID = "\(Self.futureReminderID)#2"
        XCTAssertEqual(center.addedRequests.map(\.identifier), [futureNotificationID], "one install: the overdue reminder is not caught up")
        XCTAssertEqual(center.pendingIdentifiers, [futureNotificationID])
        let installed = try XCTUnwrap(center.addedRequests.first)
        XCTAssertEqual(installed.dueInstant, futureInstant, "the installed instant is the resolved absolute instant")
        let report = try XCTUnwrap(root.lastReminderReport)
        let future = try XCTUnwrap(report.readiness.reminders.first { $0.facts.itemID == Self.futureItemID })
        XCTAssertEqual(future.outlook, .scheduled)
        XCTAssertEqual(future.facts.deliveryState, "unknown", "installing is not delivery evidence")
        XCTAssertEqual(future.facts.acknowledgmentState, "not_acknowledged")
        let overdue = try XCTUnwrap(report.readiness.reminders.first { $0.facts.itemID == Self.overdueItemID })
        XCTAssertEqual(overdue.outlook, .unscheduled(.timeInPast))
        XCTAssertEqual(overdue.facts.deliveryState, "unknown", "the passage of time is not delivery evidence")

        // The clock is then set forward and the person flies on, with more repeated callbacks.
        time.jumpWallClock(by: 86_400)
        time.travel(to: losAngeles)
        for _ in 0..<2 { systemCallbacks.post(name: .NSSystemClockDidChange, object: nil) }
        systemCallbacks.post(name: .NSSystemTimeZoneDidChange, object: nil)
        await settleTimers(time, count: 3)
        let secondPass = await timeChangeWaitUntil { lifecycle.finishedPassCount == 2 }
        XCTAssertTrue(secondPass)

        // A callback with nothing changed, repeated, runs no pass at all.
        for _ in 0..<5 { systemCallbacks.post(name: .NSSystemClockDidChange, object: nil) }
        await settleTimers(time, count: 5)
        try? await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(lifecycle.finishedPassCount, 2, "no change, no pass")

        XCTAssertEqual(center.addedRequests.count, 1, "repeated passes install nothing twice")
        XCTAssertEqual(center.pendingIdentifiers, [futureNotificationID])
        XCTAssertEqual(center.removedIdentifiers, [], "nothing was cancelled and reinstalled")
        XCTAssertEqual(center.pendingIdentifiers.count, 1)

        root.shutDown()
        let stored = try storedReminder(Self.futureReminderID)
        XCTAssertEqual(stored.resolvedInstant, Self.isoText(futureInstant), "the core still holds the resolved instant")
        XCTAssertEqual(stored.timezoneID, "UTC", "the zone the reminder was resolved in is kept for display")
        XCTAssertEqual(stored.scheduleGeneration, 2, "no new generation: nothing was rescheduled")
        let overdueStored = try storedReminder(Self.overdueReminderID)
        XCTAssertEqual(overdueStored.resolvedInstant, Self.isoText(overdueInstant), "the overdue instant is preserved")
    }

    // MARK: helpers

    private func settleTimers(_ time: ManualTime, count: Int) async {
        let held = await timeChangeWaitUntil { time.sleeperCount == count }
        XCTAssertTrue(held, "every callback started a timer")
        time.releaseTimers()
    }

    private func makeRoot(
        modules: [CompositionModule],
        notificationCenter: FakeNotificationCenter,
        time: ManualTime
    ) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { time.sources.timeZone() }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnusedProviderSender(), readSession: session, notificationCenter: notificationCenter,
                reachability: FakeReachability(reachable: true)),
            modules: modules)
    }

    private static func wholeSecond(_ date: Date) -> Date {
        Date(timeIntervalSince1970: date.timeIntervalSince1970.rounded(.down))
    }

    private static func isoText(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }

    /// Creates the stores and the schema with the real core, then seeds two items, each with a resolved reminder that
    /// has not been scheduled: one due in two days and one already overdue.
    private func prepareStore(_ layout: ProtectedStorageLayout, futureInstant: Date, overdueInstant: Date) throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        try CoreHandle(path: layout.databaseURL.path).close()
        let created = "2026-10-08T09:30:00Z"
        let destination = #"["https://api.anthropic.com"]"#
        var statements: [String] = [
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES "
                + "('\(Self.routeID)', 'synthetic', 'personal', '\(destination)', '2026-10-08T09:00:00Z')",
            "INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) VALUES "
                + "('\(Self.routeID)', 'text_interpretation', '\(destination)', '2026-10-08T09:00:00Z')",
        ]
        for (suffix, itemID, reminderID, due) in [
            ("future", Self.futureItemID, Self.futureReminderID, futureInstant),
            ("overdue", Self.overdueItemID, Self.overdueReminderID, overdueInstant),
        ] {
            statements.append(
                "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, "
                    + "calendar, item_scope, route_id, entry_locked, created_at) VALUES "
                    + "('capture-\(suffix)', 'synthetic reminder text', '\(created)', 'UTC', 0, 'en_US', 'gregorian', "
                    + "'personal', '\(Self.routeID)', 0, '\(created)')")
            statements.append(
                "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
                    + "processing_state, transcription_state, created_at, updated_at) VALUES "
                    + "('\(itemID)', 'capture-\(suffix)', 0, 'active', 'saved_local', 'not_configured', 'unprocessed', "
                    + "'not_applicable', '\(created)', '\(created)')")
            statements.append(
                "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                    + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                    + "VALUES ('\(reminderID)', '\(itemID)', 'resolved', 'pending_schedule', 'unknown', "
                    + "'not_acknowledged', '\(Self.isoText(due))', 'UTC', 2, '\(created)', '\(created)')")
            if suffix == "future" {
                // Resolving a reminder records its pending schedule operation; the core marks the reminder
                // scheduled only when the install completes against that operation.
                let notificationID = "\(reminderID)#2"
                statements.append(
                    "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                        + "effect_identity, external_id, scheduled_for, created_at) VALUES "
                        + "('schedule:\(notificationID)', '\(reminderID)', 'schedule', 'pending', "
                        + "'schedule:\(notificationID)', '\(notificationID)', '\(Self.isoText(due))', '\(created)')")
            }
        }
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw CompositionError.storageUnavailable }
        }
    }

    private struct StoredReminder {
        let resolvedInstant: String?
        let timezoneID: String?
        let scheduleGeneration: Int
    }

    private func storedReminder(_ reminderID: String) throws -> StoredReminder {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open_v2(layout.databaseURL.path, &database, SQLITE_OPEN_READONLY, nil), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        let sql = "SELECT resolved_instant, timezone_id, schedule_generation FROM reminders WHERE reminder_id = ?"
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        sqlite3_bind_text(statement, 1, reminderID, -1, transient)
        guard sqlite3_step(statement) == SQLITE_ROW else { throw CompositionError.storageUnavailable }
        func text(_ column: Int32) -> String? {
            sqlite3_column_text(statement, column).map { String(cString: $0) }
        }
        return StoredReminder(
            resolvedInstant: text(0), timezoneID: text(1), scheduleGeneration: Int(sqlite3_column_int(statement, 2)))
    }
}
