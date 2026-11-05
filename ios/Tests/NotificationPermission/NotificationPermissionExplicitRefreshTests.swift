import XCTest
@testable import OhAndServices

/// An explicit refresh is made because durable reminder state just changed. A pass already running began before that
/// change and may have read the core without it, so the explicit refresh must not return that pass's report.
final class NotificationPermissionExplicitRefreshTests: XCTestCase {
    private var center = FakeNotificationCenter()
    private var core = RecordingReconciliation()
    private var observations = InMemoryNotificationPermissionObservationStore()

    override func setUp() {
        super.setUp()
        center = FakeNotificationCenter()
        core = RecordingReconciliation()
        observations = InMemoryNotificationPermissionObservationStore()
    }

    func testExplicitRefreshesArrivingDuringAPassShareOneLaterPass() async throws {
        let gate = SuspensionGate()
        core.gate = gate
        let reconciler = NotificationPermissionReconciler(center: center, reconciliation: core, observations: observations)
        let launch = Task { try await reconciler.refresh(trigger: .launch) }
        let reachedCore = await eventually { gate.arrivals == 1 }
        XCTAssertTrue(reachedCore)

        let firstExplicit = Task { try await reconciler.refresh(trigger: .explicit) }
        let secondExplicit = Task { try await reconciler.refresh(trigger: .explicit) }
        try await Task.sleep(nanoseconds: 50_000_000)
        gate.release()

        let launchReport = try await launch.value
        let explicitReports = [try await firstExplicit.value, try await secondExplicit.value]
        XCTAssertEqual(launchReport.trigger, .launch)
        XCTAssertEqual(
            explicitReports.map(\.trigger), [.explicit, .explicit],
            "an explicit refresh returns a pass that began after it was asked for, not the one already running")
        XCTAssertEqual(core.requests.map(\.trigger), [.launch, .explicit], "the waiting explicit refreshes share one pass")
    }

    func testExplicitRefreshAfterAFailedRunningPassStillRunsItsOwnPass() async throws {
        let gate = SuspensionGate()
        core.gate = gate
        core.script(.failure(NotificationBridgeError.centerFailed))
        let reconciler = NotificationPermissionReconciler(center: center, reconciliation: core, observations: observations)
        let launch = Task { try await reconciler.refresh(trigger: .launch) }
        let reachedCore = await eventually { gate.arrivals == 1 }
        XCTAssertTrue(reachedCore)

        let explicit = Task { try await reconciler.refresh(trigger: .explicit) }
        try await Task.sleep(nanoseconds: 50_000_000)
        gate.release()

        let launchReport = try await launch.value
        let explicitReport = try await explicit.value
        XCTAssertNotNil(launchReport.reconciliationFailure)
        XCTAssertNil(explicitReport.reconciliationFailure, "the earlier pass's failure is not the explicit refresh's")
        XCTAssertEqual(core.requests.map(\.trigger), [.launch, .explicit])
    }
}
