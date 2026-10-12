import UIKit
import XCTest
@testable import OhAndServices

final class NotificationPermissionReconcilerTests: XCTestCase {
    private var center = FakeNotificationCenter()
    private var core = RecordingReconciliation()
    private var observations = InMemoryNotificationPermissionObservationStore()

    override func setUp() {
        super.setUp()
        center = FakeNotificationCenter()
        core = RecordingReconciliation()
        observations = InMemoryNotificationPermissionObservationStore()
    }

    private func makeReconciler() -> NotificationPermissionReconciler {
        NotificationPermissionReconciler(center: center, reconciliation: core, observations: observations)
    }

    func testFirstLaunchWithUsablePermissionRunsOnePassThatMayRearm() async throws {
        let reconciler = makeReconciler()
        let report = try await reconciler.refresh(trigger: .launch)

        XCTAssertEqual(report.transition, .firstObservation)
        XCTAssertEqual(report.permission, .usable)
        XCTAssertEqual(core.requests.count, 1)
        XCTAssertEqual(core.requests[0].trigger, .launch)
        XCTAssertTrue(core.requests[0].allowOptionalPrompts)
        XCTAssertEqual(observations.lastObserved(), .usable)
    }

    func testRevocationReportsUndeliverableAndWithholdsOptionalPromptsWithoutTouchingDelivery() async throws {
        core.setOutcome(ReminderReconciliationOutcome(
            reminders: [ReminderFixtures.resolved("a"), ReminderFixtures.unschedulable("b", reason: "capacity_exceeded")],
            promptsGranted: 2, promptsDeferred: 1, converged: true))
        let reconciler = makeReconciler()
        try await reconciler.refresh(trigger: .launch)

        center.authorizationStatus = .denied
        let report = try await reconciler.refresh(trigger: .foreground)

        XCTAssertEqual(report.transition, .lost)
        XCTAssertEqual(report.readiness.undeliverableCount, 1)
        XCTAssertEqual(report.readiness.unscheduledCount, 1)
        XCTAssertEqual(report.readiness.scheduledCount, 0)
        XCTAssertFalse(report.readiness.optionalPromptsAllowed)
        let request = core.requests[1]
        XCTAssertEqual(request.permission, .denied)
        XCTAssertFalse(request.allowOptionalPrompts)
        XCTAssertFalse(request.rearmRejectedSchedules, "a loss never re-arms anything")
        XCTAssertEqual(report.readiness.reminders.map(\.facts.deliveryState), ["unknown", "unknown"])
        XCTAssertEqual(report.readiness.reminders.map(\.facts.acknowledgmentState), ["not_acknowledged", "not_acknowledged"])
        XCTAssertTrue(center.addedRequests.isEmpty, "the permission module installs nothing itself")
        XCTAssertTrue(center.removedIdentifiers.isEmpty, "the permission module cancels nothing itself")
    }

    func testRegainingPermissionRearmsRejectedSchedulesExactlyOnce() async throws {
        center.authorizationStatus = .denied
        let reconciler = makeReconciler()
        try await reconciler.refresh(trigger: .launch)

        center.authorizationStatus = .authorized
        let regained = try await reconciler.refresh(trigger: .settingsReturn)
        let repeated = try await reconciler.refresh(trigger: .foreground)
        let again = try await reconciler.refresh(trigger: .foreground)

        XCTAssertEqual(regained.transition, .regained)
        XCTAssertEqual(repeated.transition, .unchanged)
        XCTAssertEqual(again.transition, .unchanged)
        XCTAssertEqual(core.requests.map(\.rearmRejectedSchedules), [false, true, false, false])
        XCTAssertEqual(core.requests.map(\.allowOptionalPrompts), [false, true, true, true])
    }

    func testEveryForegroundReconcilesAgain() async throws {
        let reconciler = makeReconciler()
        for _ in 0..<3 { try await reconciler.refresh(trigger: .foreground) }
        XCTAssertEqual(core.requests.count, 3, "capacity refill rides on each supported foreground event")
    }

    func testFailedPassIsNotRecordedSoTheRegainIsRepeatedAndOutageKeepsTheLastKnownStatus() async throws {
        let reconciler = makeReconciler()
        core.setOutcome(ReminderReconciliationOutcome(
            reminders: [ReminderFixtures.resolved("a")], promptsGranted: 0, promptsDeferred: 0, converged: true))
        center.authorizationStatus = .denied
        try await reconciler.refresh(trigger: .launch)

        center.authorizationStatus = .authorized
        core.script(.failure(NotificationBridgeError.centerFailed))
        let failed = try await reconciler.refresh(trigger: .foreground)
        XCTAssertEqual(
            failed.reconciliationFailure,
            ReminderReconciliationFailure(errorClass: .transient, code: NotificationBridgeError.centerFailed.code))
        XCTAssertFalse(failed.converged)
        XCTAssertEqual(failed.readiness.reminders.count, 1, "last known facts survive a core failure")
        XCTAssertEqual(observations.lastObserved(), .denied, "the transition is not recorded until a pass succeeds")

        center.authorizationStatus = .denied
        let outage = try await reconciler.refresh(trigger: .foreground)
        XCTAssertEqual(outage.readiness.undeliverableCount, 1)

        center.authorizationStatus = .authorized
        let recovered = try await reconciler.refresh(trigger: .foreground)
        XCTAssertEqual(recovered.transition, .regained)
        XCTAssertEqual(core.requests.map(\.rearmRejectedSchedules), [false, true, false, true])
        XCTAssertEqual(observations.lastObserved(), .usable)
    }

    func testRevocationDuringAnOutageBeforeAnySuccessfulPassReportsTheCoresStoredReminders() async throws {
        core.script(.failure(ReminderReconciliationFailure.coreUnavailable))
        core.setStoredFacts(.success([ReminderFixtures.resolved("a"), ReminderFixtures.resolved("b")]))
        center.authorizationStatus = .denied

        let report = try await makeReconciler().refresh(trigger: .launch)

        XCTAssertEqual(report.reconciliationFailure, .coreUnavailable)
        XCTAssertEqual(report.readiness.undeliverableCount, 2, "stored facts are read even though the pass failed")
        XCTAssertNil(observations.lastObserved())
    }

    func testPromptCandidatesAreOfferedOnlyAsTheAssemblySuppliesThem() async throws {
        let candidate = OptionalPromptCandidate(notificationID: "prompt-1", fireAt: Date(timeIntervalSince1970: 1_900_000_000))
        let candidates = [candidate]
        let reconciler = NotificationPermissionReconciler(
            center: center, reconciliation: core, observations: observations, promptCandidates: { candidates })
        try await reconciler.refresh(trigger: .launch)
        XCTAssertEqual(core.requests.first?.promptCandidates, [candidate])
    }

    func testCancellationIsNotReportedAsAFailureAndRecordsNothing() async throws {
        core.script(.failure(CancellationError()))
        let reconciler = makeReconciler()
        do {
            try await reconciler.refresh(trigger: .launch)
            XCTFail("cancellation must propagate")
        } catch is CancellationError {}
        XCTAssertNil(observations.lastObserved())
    }

    func testConcurrentRefreshesJoinOnePassAndCauseOneEffect() async throws {
        let gate = SuspensionGate()
        core.gate = gate
        let reconciler = makeReconciler()
        let first = Task { try await reconciler.refresh(trigger: .launch) }
        let reachedCore = await eventually { gate.arrivals == 1 }
        XCTAssertTrue(reachedCore)
        let second = Task { try await reconciler.refresh(trigger: .foreground) }
        let third = Task { try await reconciler.refresh(trigger: .foreground) }
        try await Task.sleep(nanoseconds: 50_000_000)
        gate.release()

        let reports = [try await first.value, try await second.value, try await third.value]
        XCTAssertEqual(core.requests.count, 1, "joined refreshes do not start another core pass")
        XCTAssertEqual(Set(reports.map(\.transition)), [.firstObservation])
    }

    func testPermissionChangingDuringAPassStartsAFollowUpPass() async throws {
        let gate = SuspensionGate()
        core.gate = gate
        let reconciler = makeReconciler()
        let first = Task { try await reconciler.refresh(trigger: .launch) }
        let reachedCore = await eventually { gate.arrivals == 1 }
        XCTAssertTrue(reachedCore)

        center.authorizationStatus = .denied
        let second = Task { try await reconciler.refresh(trigger: .foreground) }
        try await Task.sleep(nanoseconds: 50_000_000)
        gate.release()

        let firstReport = try await first.value
        let secondReport = try await second.value
        XCTAssertEqual(firstReport.permission, .usable)
        XCTAssertEqual(secondReport.permission, .denied)
        XCTAssertEqual(secondReport.transition, .lost)
        XCTAssertEqual(core.requests.map(\.permission), [.usable, .denied])
    }

    func testElapsedDueTimesAndAbsentDeliveryEvidenceNeverBecomeDeliveryOrAttention() async throws {
        let pastDue = ReminderFixtures.resolved("a", schedule: "scheduled", delivery: "unknown")
        core.setOutcome(ReminderReconciliationOutcome(
            reminders: [pastDue], promptsGranted: 0, promptsDeferred: 0, converged: true))
        center.delivered = []
        let reconciler = makeReconciler()
        let report = try await reconciler.refresh(trigger: .foreground)

        XCTAssertEqual(report.readiness.reminders.first?.facts.deliveryState, "unknown")
        XCTAssertEqual(report.readiness.reminders.first?.facts.acknowledgmentState, "not_acknowledged")
        XCTAssertEqual(report.readiness.reminders.first?.outlook, .scheduled)
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testPromptCapacityOutcomeIsReportedAsTheCoreReturnedIt() async throws {
        core.setOutcome(ReminderReconciliationOutcome(
            reminders: [ReminderFixtures.unschedulable("late", reason: "capacity_exceeded")],
            promptsGranted: 0, promptsDeferred: 7, converged: true))
        let report = try await makeReconciler().refresh(trigger: .launch)
        XCTAssertEqual(report.promptsDeferred, 7)
        XCTAssertEqual(report.promptsGranted, 0)
        XCTAssertEqual(report.readiness.unscheduledCount, 1)
    }

    func testForegroundHookRefreshesOncePerActivationAndStopsWhenAsked() async throws {
        let hooks = NotificationCenter()
        let received = ReportBox()
        let observer = NotificationPermissionLifecycleObserver(
            reconciler: makeReconciler(), notificationCenter: hooks, onReport: { received.append($0) })
        observer.start(isActive: true)
        let launched = await eventually { received.reports.count == 1 }
        XCTAssertTrue(launched)
        XCTAssertEqual(received.reports.first?.trigger, .launch)

        hooks.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await eventually { received.reports.count == 2 }
        XCTAssertTrue(foregrounded)
        XCTAssertEqual(received.reports.last?.trigger, .foreground)

        observer.stop()
        hooks.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(received.reports.count, 2)
        XCTAssertEqual(core.requests.count, 2)
    }

    private final class ReportBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: [NotificationPermissionReport] = []

        var reports: [NotificationPermissionReport] {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }

        func append(_ report: NotificationPermissionReport) {
            lock.lock()
            defer { lock.unlock() }
            stored.append(report)
        }
    }

    func testPendingRequestsTheCoreCannotNameAreCarriedUnderUniqueStandIns() {
        let prefix = ReminderNativeEffects.unattributablePrefix
        let overlong = String(repeating: "y", count: ReminderNativeEffects.maximumIdentifierBytes + 1)
        let due = Date(timeIntervalSince1970: 4_000_000_000)
        let requests = [overlong, "kept", "", prefix + "0", overlong + "z"].map {
            NotificationCenterPendingRequest(identifier: $0, dueInstant: due, userInfo: [:])
        }

        let carried = ReminderNativeEffects.carried(requests)

        XCTAssertEqual(carried.map(\.identifier), [prefix + "1", "kept", prefix + "2", prefix + "0", prefix + "3"])
        XCTAssertEqual(Set(carried.map(\.identifier)).count, requests.count, "one distinct entry per OS request")
        XCTAssertTrue(carried.allSatisfy { $0.dueInstant == due })
        XCTAssertFalse(carried.contains { $0.identifier.contains("#") }, "no stand-in reads as a reminder identifier")
    }
}
