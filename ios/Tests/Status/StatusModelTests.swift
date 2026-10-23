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

private final class ScriptedScheduling: ReminderSchedulingReading {
    var report: NotificationPermissionReport?
    init(_ report: NotificationPermissionReport?) { self.report = report }
    func lastReport() -> NotificationPermissionReport? { report }
}

private func schedulingReport(
    _ outlooks: [ReminderOutlook], failure: ReminderReconciliationFailure? = nil, converged: Bool? = nil,
    promptsDeferred: Int = 0, promptsWithheld: Int = 0
) -> NotificationPermissionReport {
    let reminders = outlooks.enumerated().map { index, outlook in
        ReminderStatus(facts: ReminderFixtures.resolved("item-\(index)"), outlook: outlook)
    }
    return NotificationPermissionReport(
        permission: .usable, transition: .unchanged, trigger: .explicit,
        readiness: NotificationReadinessReport(permission: .usable, reminders: reminders),
        promptsGranted: 0, promptsDeferred: promptsDeferred, promptsWithheld: promptsWithheld,
        reconciliationFailure: failure, converged: converged ?? (failure == nil))
}

/// Answers each permission from a script the test changes, as iOS Settings would.
private final class SwitchablePermissionReader: PermissionStatusReading, @unchecked Sendable {
    private let lock = NSLock()
    private var current: PermissionStatus

    init(_ status: PermissionStatus) { current = status }

    func set(_ status: PermissionStatus) {
        lock.lock(); defer { lock.unlock() }
        current = status
    }

    func status(of kind: PermissionKind) async -> PermissionStatus {
        lock.lock(); defer { lock.unlock() }
        return current
    }
}

private let notificationsGranted = [PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)]

final class StatusModelTests: XCTestCase {
    private func model(
        health: ProcessingHealthOutcome, permissions: [PermissionEntry] = notificationsGranted,
        retrier: CountingRetrier = CountingRetrier(), scheduling: NotificationPermissionReport? = nil
    ) -> (StatusModel, ScriptedHealth, CountingRetrier) {
        let scripted = ScriptedHealth(health)
        return (
            StatusModel(
                health: scripted, permissions: ScriptedPermissions(permissions), retrier: retrier,
                scheduling: ScriptedScheduling(scheduling),
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
            XCTAssertEqual(fact(status, .capture).actions, [])
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
        XCTAssertEqual(fact(status, .processing).actions, [.retry])
        await status.retry()
        XCTAssertEqual(retrier.count, 1)
    }

    func testProviderFaultNeedingUserRoutesToProviderSettingsNotRetry() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "needs_attention", errors: [error(kind: "configuration_needed", recovery: "user_action")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).detail, StatusText.processingAttentionUserDetail)
        XCTAssertEqual(fact(status, .processing).actions, [.openProviderSettings])
    }

    func testTerminalFaultOffersNoRetryAndKeepsSourceUntouched() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "needs_attention", errors: [error(kind: "permanent_failure", recovery: "terminal")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).actions, [])
        XCTAssertTrue(fact(status, .processing).detail.contains("untouched"))
    }

    func testAStallDoesNotHideAProviderFaultNeedingThePerson() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "stalled",
                errors: [
                    error(kind: "configuration_needed", recovery: "user_action"),
                    error(kind: "permanent_failure", recovery: "terminal"),
                ]))
        await status.refresh()
        let processing = fact(status, .processing)
        XCTAssertEqual(processing.headline, StatusText.processingStalledHeadline)
        XCTAssertTrue(processing.detail.contains(StatusText.processingStalledDetail))
        XCTAssertTrue(processing.detail.contains(StatusText.processingAttentionUserDetail), "the provider fault is visible")
        XCTAssertTrue(processing.detail.contains(StatusText.processingAttentionTerminalDetail), "ended work is visible")
        XCTAssertEqual(processing.actions, [.retry, .openProviderSettings], "retry alone cannot repair configuration")
    }

    func testAStallWithOnlyEndedWorkOffersRetryButNoProviderRoute() async {
        let (status, _, _) = model(
            health: snapshot(summary: "stalled", errors: [error(kind: "permanent_failure", recovery: "terminal")]))
        await status.refresh()
        XCTAssertTrue(fact(status, .processing).detail.contains(StatusText.processingAttentionTerminalDetail))
        XCTAssertEqual(fact(status, .processing).actions, [.retry])
    }

    func testNeedingThePersonAndEndedWorkTogetherAreBothShown() async {
        let (status, _, _) = model(
            health: snapshot(
                summary: "needs_attention",
                errors: [
                    error(kind: "destination_unavailable", recovery: "user_action"),
                    error(kind: "retries_exhausted", recovery: "terminal"),
                ]))
        await status.refresh()
        XCTAssertTrue(fact(status, .processing).detail.contains(StatusText.processingAttentionUserDetail))
        XCTAssertTrue(fact(status, .processing).detail.contains(StatusText.processingAttentionTerminalDetail))
        XCTAssertEqual(fact(status, .processing).actions, [.openProviderSettings])
    }

    func testWaitingForNetworkSaysSoAndNeedsNoTriage() async {
        let (status, _, _) = model(
            health: snapshot(summary: "waiting", errors: [error(kind: "waiting_for_network", recovery: "automatic")]))
        await status.refresh()
        XCTAssertEqual(fact(status, .processing).tone, .waiting)
        XCTAssertTrue(fact(status, .processing).detail.contains("network"))
        XCTAssertTrue(fact(status, .processing).detail.contains("by itself"))
        XCTAssertEqual(fact(status, .processing).actions, [], "waiting work resumes itself; no triage")
    }

    func testRetryReadsStatusAgainAfterRecovery() async {
        let (status, health, _) = model(health: snapshot(summary: "stalled"))
        await status.refresh()
        health.set(snapshot(summary: "idle"))
        await status.retry()
        XCTAssertEqual(fact(status, .processing).headline, StatusText.processingIdleHeadline)
        XCTAssertEqual(fact(status, .processing).actions, [])
    }

    func testNotificationFaultsAreShown() async {
        let cases: [(PermissionStatus, PermissionChange, String, [StatusAction])] = [
            (.denied, .unchanged, StatusText.notificationsOffHeadline, [.openSettings]),
            (.denied, .revoked, StatusText.notificationsRevokedHeadline, [.openSettings]),
            (.restricted, .unchanged, StatusText.notificationsOffHeadline, []),
            (.notRequested, .unchanged, StatusText.notificationsNotAskedHeadline, []),
            (.granted, .unchanged, StatusText.notificationsUncheckedHeadline, []),
        ]
        for (permission, change, headline, actions) in cases {
            let (status, _, _) = model(
                health: snapshot(summary: "idle"),
                permissions: [PermissionEntry(kind: .notifications, status: permission, change: change)])
            await status.refresh()
            XCTAssertEqual(fact(status, .notifications).headline, headline)
            XCTAssertEqual(fact(status, .notifications).actions, actions)
        }
    }

    func testGrantedPermissionDoesNotImplyScheduling() async {
        let cases: [(NotificationPermissionReport, String, StatusTone)] = [
            (schedulingReport([.unscheduled(.capacityExceeded)]), StatusText.notificationsUnscheduledHeadline, .needsAttention),
            (schedulingReport([.unscheduled(.installFailed), .scheduled]), StatusText.notificationsUnscheduledHeadline, .needsAttention),
            (schedulingReport([.scheduled], failure: .reconciliationFailed), StatusText.notificationsPassFailedHeadline, .needsAttention),
            (schedulingReport([.awaitingInstall]), StatusText.notificationsInstallingHeadline, .waiting),
            (schedulingReport([.scheduled]), StatusText.notificationsScheduledHeadline, .good),
            (schedulingReport([]), StatusText.notificationsNoneHeadline, .good),
        ]
        for (report, headline, tone) in cases {
            let (status, _, _) = model(health: snapshot(summary: "idle"), scheduling: report)
            await status.refresh()
            XCTAssertEqual(fact(status, .notifications).headline, headline)
            XCTAssertEqual(fact(status, .notifications).tone, tone)
        }
        let (unscheduled, _, _) = model(
            health: snapshot(summary: "idle"), scheduling: schedulingReport([.unscheduled(.capacityExceeded)]))
        await unscheduled.refresh()
        XCTAssertTrue(fact(unscheduled, .notifications).detail.contains("not scheduled"))
        XCTAssertFalse(fact(unscheduled, .notifications).detail.contains("will appear when they are due"))
    }

    func testAPassWhoseNativeEffectFailedIsNotShownAsHealthy() async {
        // The core finished the pass (no reconciliation failure) but one schedule or cancel was not accepted.
        let cases: [(NotificationPermissionReport, String)] = [
            (schedulingReport([], converged: false), StatusText.notificationsUnfinishedHeadline),
            (schedulingReport([.scheduled], converged: false), StatusText.notificationsUnfinishedHeadline),
            (schedulingReport([.awaitingInstall], converged: false), StatusText.notificationsUnfinishedHeadline),
            (schedulingReport([.unscheduled(.installFailed)], converged: false), StatusText.notificationsUnscheduledHeadline),
        ]
        for (report, headline) in cases {
            let (status, _, _) = model(health: snapshot(summary: "idle"), scheduling: report)
            await status.refresh()
            let notifications = fact(status, .notifications)
            XCTAssertEqual(notifications.headline, headline)
            XCTAssertEqual(notifications.tone, .needsAttention)
            XCTAssertTrue(notifications.detail.contains(StatusText.notificationsUnfinishedDetail))
            XCTAssertFalse(notifications.detail.contains("Nothing is held back"))
        }
    }

    func testHeldBackDailyPromptsAreNotDescribedAsNothingHeldBack() async {
        let cases: [(NotificationPermissionReport, String, String)] = [
            (schedulingReport([], promptsDeferred: 2), StatusText.notificationsNoneHeadline, "2 daily prompts wait for room"),
            (schedulingReport([], promptsWithheld: 1), StatusText.notificationsNoneHeadline, "1 daily prompt is held back"),
            (schedulingReport([.scheduled], promptsDeferred: 1), StatusText.notificationsScheduledHeadline, "1 daily prompt waits"),
        ]
        for (report, headline, prompts) in cases {
            let (status, _, _) = model(health: snapshot(summary: "idle"), scheduling: report)
            await status.refresh()
            let notifications = fact(status, .notifications)
            XCTAssertEqual(notifications.headline, headline)
            XCTAssertEqual(notifications.tone, .waiting)
            XCTAssertTrue(notifications.detail.contains(prompts), notifications.detail)
            XCTAssertFalse(notifications.detail.contains("Nothing is held back"))
        }
        let (settled, _, _) = model(health: snapshot(summary: "idle"), scheduling: schedulingReport([]))
        await settled.refresh()
        XCTAssertEqual(fact(settled, .notifications).detail, StatusText.notificationsNoneDetail)
        XCTAssertEqual(fact(settled, .notifications).tone, .good)
    }

    /// Status and Permissions each call a revocation out; neither reading uses up the other's callout.
    func testOpeningStatusDoesNotConsumeThePermissionsScreensRevocationCallout() async throws {
        for statusFirst in [true, false] {
            let suite = "status-permission-memory-\(UUID().uuidString)"
            let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
            defer { defaults.removePersistentDomain(forName: suite) }
            let system = SwitchablePermissionReader(.granted)
            let permissionsScreen = PermissionsSettingsModel(
                overview: PermissionsOverview(reader: system, memory: UserDefaultsPermissionMemory(defaults: defaults)),
                openSystemSettings: {})
            let statusScreen = StatusModel(
                health: ScriptedHealth(snapshot(summary: "idle")),
                permissions: PermissionsOverview(reader: system, memory: StatusPermissionMemory(defaults: defaults)),
                retrier: CountingRetrier(), openSystemSettings: {})
            await permissionsScreen.refresh()
            await statusScreen.refresh()

            system.set(.denied)
            if statusFirst {
                await statusScreen.refresh()
                await permissionsScreen.refresh()
            } else {
                await permissionsScreen.refresh()
                await statusScreen.refresh()
            }

            let order = statusFirst ? "Status opened first" : "Permissions opened first"
            XCTAssertEqual(
                permissionsScreen.rows.first { $0.kind == .notifications }?.change, .revoked, order)
            XCTAssertEqual(fact(statusScreen, .notifications).headline, StatusText.notificationsRevokedHeadline, order)
            await permissionsScreen.refresh()
            XCTAssertEqual(
                permissionsScreen.rows.first { $0.kind == .notifications }?.change, .unchanged,
                "Permissions still calls the revocation out once (\(order))")
        }
    }

    func testRetryRequestIsAcknowledgedUntilNoRetryIsOffered() async {
        let (status, health, _) = model(health: snapshot(summary: "stalled"))
        await status.refresh()
        XCTAssertFalse(status.retryRequested)
        await status.retry()
        XCTAssertTrue(status.retryRequested)
        health.set(snapshot(summary: "idle"))
        await status.refresh()
        XCTAssertFalse(status.retryRequested)
    }

    func testFactsBeforeFirstReadDoNotClaimHealth() {
        let (status, _, _) = model(health: snapshot(summary: "idle"))
        XCTAssertFalse(status.hasLoaded)
        XCTAssertEqual(fact(status, .processing).headline, StatusText.processingCheckingHeadline)
        XCTAssertEqual(fact(status, .capture).tone, .good)
    }
}
