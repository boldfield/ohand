import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedHealth: ProcessingHealthReading, @unchecked Sendable {
    private let lock = NSLock()
    private var next: ProcessingHealthOutcome

    init(_ outcome: ProcessingHealthOutcome) { next = outcome }

    func set(_ outcome: ProcessingHealthOutcome) {
        lock.lock(); defer { lock.unlock() }
        next = outcome
    }

    func read() async -> ProcessingHealthOutcome {
        lock.lock(); defer { lock.unlock() }
        return next
    }
}

private final class ScriptedPermissions: PermissionsReading, @unchecked Sendable {
    let entries: [PermissionEntry]
    init(_ entries: [PermissionEntry]) { self.entries = entries }
    func refresh() async -> [PermissionEntry] { entries }
}

private final class CountingRetrier: ProcessingRetrying {
    private(set) var count = 0
    func retryNow() { count += 1 }
}

private func snapshot(summary: String, errors: [String] = [], pending: Int = 0, running: Int = 0) -> ProcessingHealthOutcome {
    let json = """
        {"operation_id":1,"generated_at":"2026-10-22T00:00:00Z","summary":"\(summary)","stalled":\(summary == "stalled"),
        "stall_reasons":[],"pending_jobs":\(pending),"running_jobs":\(running),"overdue_jobs":0,
        "oldest_pending_age_seconds":null,"oldest_overdue_seconds":null,"last_interpretation_success_at":null,
        "errors":[\(errors.joined(separator: ","))]}
        """
    return .snapshot(try! JSONDecoder().decode(ProcessingHealth.self, from: Data(json.utf8)))
}

private func error(kind: String, recovery: String) -> String {
    "{\"kind\":\"\(kind)\",\"reason\":\"synthetic\",\"recovery\":\"\(recovery)\",\"job_count\":1,\"oldest_age_seconds\":5,\"next_attempt_at\":null}"
}

private let notificationsGranted = [PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)]

final class StatusModelTests: XCTestCase {
    private func model(
        health: ProcessingHealthOutcome, permissions: [PermissionEntry] = notificationsGranted,
        retrier: CountingRetrier = CountingRetrier()
    ) -> (StatusModel, ScriptedHealth, CountingRetrier) {
        let scripted = ScriptedHealth(health)
        return (
            StatusModel(
                health: scripted, permissions: ScriptedPermissions(permissions), retrier: retrier,
                openSystemSettings: {}),
            scripted, retrier
        )
    }

    private func fact(_ model: StatusModel, _ kind: StatusFactKind) -> StatusFact {
        model.facts.first { $0.kind == kind }!
    }

    func testSyncIsNotConfiguredAndRemoteInterpretationIsNotOnDevice() async {
        let (status, _, _) = model(health: snapshot(summary: "idle"))
        await status.refresh()
        XCTAssertEqual(fact(status, .sync).headline, StatusText.syncHeadline)
        XCTAssertTrue(fact(status, .sync).detail.contains("only"))
        let remote = fact(status, .remoteInterpretation).detail
        XCTAssertTrue(remote.contains("not on-device"))
        XCTAssertFalse(remote.lowercased().contains("private by default"))
    }

    func testCaptureIsReadyInEveryProcessingState() async {
        let states = [
            snapshot(summary: "idle"), snapshot(summary: "stalled"),
            snapshot(summary: "needs_attention", errors: [error(kind: "permanent_failure", recovery: "terminal")]),
            .failed(CoreFailure.handleClosed),
        ]
        for state in states {
            let (status, _, _) = model(health: state)
            XCTAssertEqual(fact(status, .capture).headline, StatusText.captureHeadline)
            await status.refresh()
            XCTAssertEqual(fact(status, .capture).tone, .good)
            XCTAssertEqual(fact(status, .capture).action, .none)
        }
    }

    func testFailedReadIsNeverShownAsHealthy() async {
        let (status, _, _) = model(health: .failed(CoreFailure.handleClosed))
        await status.refresh()
        let processing = fact(status, .processing)
        XCTAssertEqual(processing.headline, StatusText.processingUnknownHeadline)
        XCTAssertNotEqual(processing.tone, .good)
    }

    func testStalledWorkIsVisibleAndOffersSourcePreservingRetry() async {
        let (status, _, retrier) = model(health: snapshot(summary: "stalled"))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).tone, .needsAttention)
        XCTAssertEqual(fact(status, .processing).action, .retry)
        await status.retry()
        XCTAssertEqual(retrier.count, 1)
    }

    func testProviderFaultNeedingUserPointsToSettingsAndOffersRetry() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "needs_attention", errors: [error(kind: "configuration_needed", recovery: "user_action")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).detail, StatusText.processingAttentionUserDetail)
        XCTAssertEqual(fact(status, .processing).action, .retry)
    }

    func testTerminalFaultOffersNoRetryAndKeepsSourceUntouched() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "needs_attention", errors: [error(kind: "permanent_failure", recovery: "terminal")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).action, StatusAction.none)
        XCTAssertTrue(fact(status, .processing).detail.contains("untouched"))
    }

    func testWaitingForNetworkSaysSoAndNeedsNoTriage() async {
        let (status, _, _) = model(
            health: snapshot(summary: "waiting", errors: [error(kind: "waiting_for_network", recovery: "automatic")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).tone, .waiting)
        XCTAssertTrue(fact(status, .processing).detail.contains("network"))
        XCTAssertTrue(fact(status, .processing).detail.contains("by itself"))
    }

    func testRetryReadsStatusAgainAfterRecovery() async {
        let (status, health, _) = model(health: snapshot(summary: "stalled"))
        await status.refresh()
        health.set(snapshot(summary: "idle"))
        await status.retry()
        XCTAssertEqual(fact(status, .processing).headline, StatusText.processingIdleHeadline)
        XCTAssertEqual(fact(status, .processing).action, StatusAction.none)
    }

    func testNotificationFaultsAreShown() async {
        let cases: [(PermissionStatus, PermissionChange, String, StatusAction)] = [
            (.denied, .unchanged, StatusText.notificationsOffHeadline, .openSettings),
            (.denied, .revoked, StatusText.notificationsRevokedHeadline, .openSettings),
            (.restricted, .unchanged, StatusText.notificationsOffHeadline, .none),
            (.notRequested, .unchanged, StatusText.notificationsNotAskedHeadline, .none),
            (.granted, .unchanged, StatusText.notificationsOnHeadline, .none),
        ]
        for (permission, change, headline, action) in cases {
            let (status, _, _) = model(
                health: snapshot(summary: "idle"),
                permissions: [PermissionEntry(kind: .notifications, status: permission, change: change)])
            await status.refresh()
            XCTAssertEqual(fact(status, .notifications).headline, headline)
            XCTAssertEqual(fact(status, .notifications).action, action)
        }
    }

    func testFactsBeforeFirstReadDoNotClaimHealth() {
        let (status, _, _) = model(health: snapshot(summary: "idle"))
        XCTAssertFalse(status.hasLoaded)
        XCTAssertEqual(fact(status, .processing).headline, StatusText.processingCheckingHeadline)
        XCTAssertEqual(fact(status, .capture).tone, .good)
    }
}
