import SQLite3
import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class FailedHealth: ProcessingHealthReading {
    func read() async -> ProcessingHealthOutcome { .failed(CoreFailure.handleClosed) }
}

private final class EmptyPermissions: PermissionsReading, @unchecked Sendable {
    func refresh() async -> [PermissionEntry] {
        [PermissionEntry(kind: .notifications, status: .denied, change: .revoked)]
    }
}

private final class NoopRetrier: ProcessingRetrying {
    func retryNow() {}
}

private final class CountingRetrier: ProcessingRetrying {
    private(set) var count = 0
    func retryNow() { count += 1 }
}

private final class FixedHealth: ProcessingHealthReading, @unchecked Sendable {
    let outcome: ProcessingHealthOutcome
    init(_ outcome: ProcessingHealthOutcome) { self.outcome = outcome }
    func read() async -> ProcessingHealthOutcome { outcome }
}

private func healthSnapshot(summary: String, errors: [String] = []) -> ProcessingHealthOutcome {
    let json = """
        {"operation_id":1,"generated_at":"2026-10-22T00:00:00Z","summary":"\(summary)","stalled":\(summary == "stalled"),
        "stall_reasons":[],"pending_jobs":1,"running_jobs":0,"overdue_jobs":0,
        "oldest_pending_age_seconds":null,"oldest_overdue_seconds":null,"last_interpretation_success_at":null,
        "errors":[\(errors.joined(separator: ","))]}
        """
    return .snapshot(try! JSONDecoder().decode(ProcessingHealth.self, from: Data(json.utf8)))
}

/// SQL that leaves one due interpretation job nothing has drained, with its saved source text.
private let dueJobStatements = [
    "INSERT INTO captures (capture_id, text, capture_instant, timezone_id, utc_offset_minutes, locale, "
        + "calendar, item_scope, route_id, entry_locked, created_at) VALUES ('capture-stalled', "
        + "'call the roofer about the leak', '2020-01-01T00:00:00Z', 'UTC', 0, 'en', 'gregorian', "
        + "'personal', 'route-1', 0, '2020-01-01T00:00:00Z')",
    "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
        + "processing_state, transcription_state, created_at, updated_at) VALUES ('item-stalled', 'capture-stalled', 0, "
        + "'active', 'saved_local', 'not_configured', 'unprocessed', 'not_applicable', "
        + "'2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
    "INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, status, "
        + "attempt_count, created_at) VALUES ('job-stalled', 1, 'item-stalled', 'interpret', 0, 'queued', 0, "
        + "'2020-01-01T00:00:00Z')",
]

/// The status screen over the real core and the production assembly, and hosted rendering of the screen itself.
@MainActor
final class StatusIntegrationTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var services: AppServices?
    private var window: UIWindow?
    private var rendered: [String: String] = [:]

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("status-integration-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        rendered = [:]
    }

    override func tearDownWithError() throws {
        window?.isHidden = true
        window = nil
        if let services {
            // A save starts a drain; let it finish so no core result is still in flight when the root closes.
            wait { !services.root.jobRunner.isDraining }
            let settled = Date().addingTimeInterval(1.5)
            wait(timeout: 2) { Date() >= settled }
        }
        services?.shutDown()
        services = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func wait(timeout: TimeInterval = 30, until condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    /// Runs an async status call from a synchronous test. The production root provisions its route by turning the main
    /// run loop, which an async test method on the main actor would never do.
    private func run(_ operation: @escaping @MainActor () async -> Void) {
        let finished = expectation(description: "status call")
        Task { @MainActor in
            await operation()
            finished.fulfill()
        }
        self.wait(for: [finished], timeout: 30)
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        guard sqlite3_open(path, &database) == SQLITE_OK else { return "" }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return "" }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    /// Runs SQL against the store file, which the core also has open; writes wait on the busy timeout.
    private func execute(_ statements: [String]) {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        }
    }

    /// Finds the accessibility element with an identifier in the hosted screen and activates it, as a tap would.
    private func activate(_ identifier: String) -> Bool {
        func search(_ object: NSObject, depth: Int) -> NSObject? {
            if object.responds(to: NSSelectorFromString("accessibilityIdentifier")),
                object.value(forKey: "accessibilityIdentifier") as? String == identifier
            {
                return object
            }
            guard depth < 30 else { return nil }
            var children: [NSObject] = []
            if let view = object as? UIView { children += view.subviews }
            children += (object.accessibilityElements ?? []).compactMap { $0 as? NSObject }
            for child in children { if let found = search(child, depth: depth + 1) { return found } }
            return nil
        }
        guard let root = window?.rootViewController?.view, let element = search(root, depth: 0) else { return false }
        return element.accessibilityActivate()
    }

    /// Builds the production assembly, starts it in the foreground and waits for its launch drain to finish.
    private func foregroundServices() throws -> AppServices {
        let built = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        services = built
        built.root.start(isActive: true)
        wait { built.root.jobRunner.startedDrainCount > 0 && !built.root.jobRunner.isDraining }
        return built
    }

    private func host(_ view: StatusView) {
        let shown = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        shown.rootViewController = UIHostingController(rootView: NavigationStack { view })
        shown.makeKeyAndVisible()
        window = shown
    }

    // MARK: hosted rendering

    func testTheScreenRendersEveryRowAndTheRecoveryButtonsItOffers() {
        let model = StatusModel(
            health: FailedHealth(), permissions: EmptyPermissions(), retrier: NoopRetrier(), openSystemSettings: {})
        host(StatusView(model: model, renderReporter: { [unowned self] in rendered = $0 }))
        wait(timeout: 10) { self.rendered[StatusView.rowIdentifier(.processing)]?.contains(StatusText.processingUnknownHeadline) == true }

        for kind in StatusFactKind.allCases {
            XCTAssertNotNil(rendered[StatusView.rowIdentifier(kind)], "\(kind) row is shown")
        }
        XCTAssertTrue(rendered[StatusView.rowIdentifier(.capture)]?.contains(StatusText.captureHeadline) == true)
        XCTAssertTrue(rendered[StatusView.rowIdentifier(.sync)]?.contains(StatusText.syncHeadline) == true)
        XCTAssertTrue(rendered[StatusView.rowIdentifier(.notifications)]?.contains(StatusText.notificationsRevokedHeadline) == true)
        XCTAssertEqual(rendered[StatusView.openSettingsIdentifier], StatusText.openSettingsLabel)
        XCTAssertEqual(rendered[StatusView.refreshIdentifier], StatusText.refreshLabel)
        XCTAssertNil(rendered[StatusView.retryIdentifier], "an unreadable status offers no retry")
    }

    func testAStalledStateRendersTryAgainAndTappingItAsksForTheRetry() {
        let retrier = CountingRetrier()
        let model = StatusModel(
            health: FixedHealth(healthSnapshot(summary: "stalled")), permissions: EmptyPermissions(), retrier: retrier,
            openSystemSettings: {})
        host(StatusView(model: model, renderReporter: { [unowned self] in rendered = $0 }))
        wait(timeout: 10) { self.rendered[StatusView.retryIdentifier] != nil }

        XCTAssertTrue(rendered[StatusView.rowIdentifier(.processing)]?.contains(StatusText.processingStalledHeadline) == true)
        XCTAssertEqual(rendered[StatusView.retryIdentifier], StatusText.retryLabel)
        XCTAssertNil(rendered[StatusView.providerSettingsIdentifier], "a stall is not a provider problem")
        XCTAssertTrue(activate(StatusView.retryIdentifier), "the Try again control is reachable")
        wait(timeout: 10) { retrier.count > 0 }
        XCTAssertEqual(retrier.count, 1)
        wait(timeout: 10) { self.rendered[StatusView.retryNoteIdentifier] != nil }
        XCTAssertEqual(rendered[StatusView.retryNoteIdentifier], StatusText.retryRequestedNote)
    }

    func testAProviderFaultNeedingThePersonRendersAnOpenProviderSettingsRouteInsteadOfRetry() {
        let retrier = CountingRetrier()
        let fault = "{\"kind\":\"configuration_needed\",\"reason\":\"synthetic\",\"recovery\":\"user_action\","
            + "\"job_count\":1,\"oldest_age_seconds\":5,\"next_attempt_at\":null}"
        let model = StatusModel(
            health: FixedHealth(healthSnapshot(summary: "needs_attention", errors: [fault])),
            permissions: EmptyPermissions(), retrier: retrier, openSystemSettings: {})
        var providerSettingsShown = false
        host(
            StatusView(
                model: model,
                makeProviderSettings: { AnyView(Color.clear.onAppear { providerSettingsShown = true }) },
                renderReporter: { [unowned self] in rendered = $0 }))
        wait(timeout: 10) { self.rendered[StatusView.providerSettingsIdentifier] != nil }

        XCTAssertEqual(rendered[StatusView.providerSettingsIdentifier], StatusText.providerSettingsLabel)
        XCTAssertNil(rendered[StatusView.retryIdentifier], "re-draining unchanged configuration is not offered")
        XCTAssertTrue(activate(StatusView.providerSettingsIdentifier), "the Provider settings route is reachable")
        wait(timeout: 10) { providerSettingsShown }
        XCTAssertTrue(providerSettingsShown, "activating the route opens Provider settings")
        XCTAssertEqual(retrier.count, 0)
    }

    // MARK: the production assembly

    func testTheAssembledScreenReadsRealHealthAndRetryKeepsTheSavedSourceUnchanged() throws {
        let built = try foregroundServices()
        let status = try XCTUnwrap(built.managementContext.statusModel)
        built.captureEntry.textModel.updateText("remind me to water the plants")
        built.captureEntry.saveText()
        wait { built.captureEntry.acknowledgment?.savedItemID != nil }
        XCTAssertNotNil(built.captureEntry.acknowledgment?.savedItemID)
        wait { !built.root.jobRunner.isDraining }

        // Work becomes due after the launch drain and nothing is draining it: the real core reports a stall.
        execute(dueJobStatements)
        run { await status.refresh() }
        let stalled = try XCTUnwrap(status.facts.first { $0.kind == .processing })
        XCTAssertEqual(stalled.headline, StatusText.processingStalledHeadline, "the real core reported the stall")
        XCTAssertEqual(stalled.action, .retry)

        let sourceBefore = scalar("SELECT text FROM captures WHERE capture_id = 'capture-stalled'")
        XCTAssertEqual(sourceBefore, "call the roofer about the leak")
        let capturesBefore = scalar("SELECT COUNT(*) FROM captures")
        let drainsBefore = built.root.jobRunner.startedDrainCount

        run { await status.retry() }
        XCTAssertGreaterThan(built.root.jobRunner.startedDrainCount, drainsBefore, "retry made the real runner start a drain")
        XCTAssertTrue(status.retryRequested)
        wait { built.root.jobRunner.finishedDrainCount >= built.root.jobRunner.startedDrainCount && !built.root.jobRunner.isDraining }

        run { await status.refresh() }
        let after = try XCTUnwrap(status.facts.first { $0.kind == .processing })
        XCTAssertNotEqual(after.headline, StatusText.processingStalledHeadline, "the drain took the waiting work up")
        XCTAssertNotEqual(after.action, .retry)
        XCTAssertEqual(
            scalar("SELECT text FROM captures WHERE capture_id = 'capture-stalled'"), sourceBefore,
            "retry never rewrites the saved source")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM captures"), capturesBefore)
    }

    func testCaptureIsImmediateWhileTheProductionAssemblyIsStalled() throws {
        let built = try foregroundServices()
        let status = try XCTUnwrap(built.managementContext.statusModel)
        execute(dueJobStatements)
        run { await status.refresh() }
        XCTAssertEqual(status.facts.first { $0.kind == .processing }?.headline, StatusText.processingStalledHeadline)

        built.captureEntry.textModel.updateText("call the dentist")
        let started = Date()
        built.captureEntry.saveText()
        wait(timeout: 5) { built.captureEntry.acknowledgment?.savedItemID != nil }
        let elapsed = Date().timeIntervalSince(started)

        XCTAssertNotNil(built.captureEntry.acknowledgment?.savedItemID, "the save is acknowledged under a real stall")
        XCTAssertLessThan(elapsed, 3, "the acknowledgment does not wait for the stalled work or the status read")
        XCTAssertEqual(scalar("SELECT text FROM captures WHERE text = 'call the dentist'"), "call the dentist")
        XCTAssertEqual(status.facts.first { $0.kind == .capture }?.tone, .good)
    }

    func testCaptureIsImmediateOnRelaunchIntoAStalledStoreWhileStatusReadingFails() throws {
        // A store left with due work by an earlier run (nonuse), then launched inactive so nothing drains it.
        try CoreHandle(path: ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path).close()
        execute(dueJobStatements)
        let built = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        services = built
        built.root.start(isActive: false)
        let status = try XCTUnwrap(built.managementContext.statusModel)
        run { await status.refresh() }
        XCTAssertEqual(status.facts.first { $0.kind == .processing }?.headline, StatusText.processingStalledHeadline)

        // A status read that fails does not touch the capture path either.
        let failing = StatusModel(
            health: FailedHealth(), permissions: EmptyPermissions(), retrier: NoopRetrier(), openSystemSettings: {})
        run { await failing.refresh() }
        XCTAssertEqual(failing.facts.first { $0.kind == .processing }?.headline, StatusText.processingUnknownHeadline)

        built.captureEntry.textModel.updateText("book the vet")
        let started = Date()
        built.captureEntry.saveText()
        wait(timeout: 5) { built.captureEntry.acknowledgment?.savedItemID != nil }

        XCTAssertNotNil(built.captureEntry.acknowledgment?.savedItemID)
        XCTAssertLessThan(Date().timeIntervalSince(started), 3, "capture is immediate after nonuse")
        XCTAssertEqual(scalar("SELECT text FROM captures WHERE text = 'book the vet'"), "book the vet")
    }
}
