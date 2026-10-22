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

    // MARK: the production assembly

    func testTheAssembledScreenReadsRealHealthAndRetryKeepsTheSavedSourceUnchanged() throws {
        let built = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        services = built
        let status = try XCTUnwrap(built.managementContext.statusModel)
        // The lifecycle coordinator attaches its health observer when the root starts, as it does at launch.
        built.root.start(isActive: false)

        built.captureEntry.textModel.updateText("remind me to water the plants")
        built.captureEntry.saveText()
        wait { built.captureEntry.acknowledgment?.savedItemID != nil }
        XCTAssertNotNil(built.captureEntry.acknowledgment?.savedItemID)

        run { await status.refresh() }
        let processing = try XCTUnwrap(status.facts.first { $0.kind == .processing })
        XCTAssertNotEqual(processing.headline, StatusText.processingUnknownHeadline, "the real core answered the read")
        XCTAssertNotEqual(processing.headline, StatusText.processingCheckingHeadline)

        let before = scalar("SELECT text FROM captures")
        XCTAssertEqual(before, "remind me to water the plants")
        run { await status.retry() }
        XCTAssertEqual(scalar("SELECT text FROM captures"), before, "retry never rewrites the saved source")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM captures"), "1")
    }

    func testCaptureIsImmediateWhileTheStatusReadIsFailingAndAfterAnErrorState() throws {
        let built = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        services = built
        let failing = StatusModel(
            health: FailedHealth(), permissions: EmptyPermissions(), retrier: NoopRetrier(), openSystemSettings: {})
        run { await failing.refresh() }
        XCTAssertEqual(failing.facts.first { $0.kind == .processing }?.headline, StatusText.processingUnknownHeadline)

        built.captureEntry.textModel.updateText("call the dentist")
        built.captureEntry.saveText()
        wait(timeout: 10) { built.captureEntry.acknowledgment?.savedItemID != nil }

        XCTAssertNotNil(built.captureEntry.acknowledgment?.savedItemID, "the save is not held up by the unreadable status")
        XCTAssertEqual(scalar("SELECT text FROM captures"), "call the dentist")
        XCTAssertEqual(failing.facts.first { $0.kind == .capture }?.tone, .good)
    }
}
