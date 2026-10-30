import XCTest
@testable import OhAndServices

final class NotificationReadinessTests: XCTestCase {
    private func outlook(_ permission: NotificationPermissionState, _ facts: ReminderFacts) -> ReminderOutlook {
        NotificationReadiness.evaluate(permission: permission, reminders: [facts]).reminders[0].outlook
    }

    func testAuthorizationsMapToUsableOrBlocked() {
        XCTAssertEqual(NotificationPermissionState(.authorized), .usable)
        XCTAssertEqual(NotificationPermissionState(.provisional), .usable)
        XCTAssertEqual(NotificationPermissionState(.ephemeral), .usable)
        XCTAssertEqual(NotificationPermissionState(.notDetermined), .notRequested)
        XCTAssertEqual(NotificationPermissionState(.denied), .denied)
    }

    func testTransitionsBetweenPermissionStates() {
        typealias Transition = NotificationPermissionTransition
        XCTAssertEqual(Transition.between(previous: nil, current: .usable), .firstObservation)
        XCTAssertEqual(Transition.between(previous: .usable, current: .usable), .unchanged)
        XCTAssertEqual(Transition.between(previous: .denied, current: .usable), .regained)
        XCTAssertEqual(Transition.between(previous: .notRequested, current: .usable), .regained)
        XCTAssertEqual(Transition.between(previous: .usable, current: .denied), .lost)
        XCTAssertEqual(Transition.between(previous: .usable, current: .notRequested), .lost)
        XCTAssertEqual(Transition.between(previous: .notRequested, current: .denied), .unusableChanged)
    }

    func testUsablePermissionFollowsTheCoresScheduleState() {
        XCTAssertEqual(outlook(.usable, ReminderFixtures.resolved("a", schedule: "scheduled")), .scheduled)
        XCTAssertEqual(outlook(.usable, ReminderFixtures.resolved("a", schedule: "pending_schedule")), .awaitingInstall)
        XCTAssertEqual(outlook(.usable, ReminderFixtures.resolved("a", schedule: "not_scheduled")), .awaitingInstall)
        XCTAssertEqual(
            outlook(.usable, ReminderFixtures.resolved("a", schedule: "schedule_failed")), .unscheduled(.installFailed))
    }

    func testRevokedOrDeniedPermissionMakesEveryWantedReminderUndeliverableEvenWhenInstalled() {
        for schedule in ["scheduled", "pending_schedule", "schedule_failed", "not_scheduled"] {
            let facts = ReminderFixtures.resolved("a", schedule: schedule)
            XCTAssertEqual(outlook(.denied, facts), .undeliverable(.permissionDenied), schedule)
            XCTAssertEqual(outlook(.notRequested, facts), .undeliverable(.permissionNotRequested), schedule)
        }
    }

    func testCoreHeldBackRemindersKeepTheirOwnReasonWhateverThePermission() {
        for permission in [NotificationPermissionState.usable, .denied, .notRequested] {
            XCTAssertEqual(
                outlook(permission, ReminderFixtures.unschedulable("a", reason: "capacity_exceeded")),
                .unscheduled(.capacityExceeded))
            XCTAssertEqual(
                outlook(permission, ReminderFixtures.unschedulable("a", reason: "time_in_past")),
                .unscheduled(.timeInPast))
            XCTAssertEqual(
                outlook(permission, ReminderFixtures.unschedulable("a", reason: "permission_denied")),
                .unscheduled(.permissionDenied))
            XCTAssertEqual(
                outlook(permission, ReminderFixtures.unschedulable("a", reason: "something_new")),
                .unscheduled(.unrecognized))
        }
    }

    func testRemindersThatWereNeverWantedAreNotApplicable() {
        for state in ["not_requested", "unsupported_recurrence", "cancelled"] {
            let facts = ReminderFacts(
                itemID: "a", requestState: state, scheduleState: nil, deliveryState: nil,
                acknowledgmentState: nil, unschedulableReason: nil)
            XCTAssertEqual(outlook(.denied, facts), .notApplicable, state)
        }
    }

    func testAReminderWithNoTimeNeedsATimeWhateverThePermissionAndIsNotCountedAsUnscheduled() {
        let facts = ReminderFacts(
            itemID: "a", requestState: "not_scheduled_yet", scheduleState: nil, deliveryState: nil,
            acknowledgmentState: nil, unschedulableReason: nil)
        for permission in [NotificationPermissionState.usable, .denied, .notRequested] {
            XCTAssertEqual(outlook(permission, facts), .needsTime, "\(permission)")
        }
        let report = NotificationReadiness.evaluate(permission: .denied, reminders: [facts, ReminderFixtures.resolved("b")])
        XCTAssertEqual(report.needsTimeCount, 1)
        XCTAssertEqual(report.unscheduledCount, 0)
        XCTAssertEqual(report.undeliverableCount, 1, "only the resolved reminder is blocked by the permission")
    }

    func testDeliveryAndAcknowledgmentFactsPassThroughUntouched() {
        let reminders = [
            ReminderFixtures.resolved("unknown", delivery: "unknown"),
            ReminderFixtures.resolved("delivered", delivery: "delivered"),
            ReminderFixtures.resolved("opened", delivery: "opened"),
        ]
        for permission in [NotificationPermissionState.usable, .denied] {
            let report = NotificationReadiness.evaluate(permission: permission, reminders: reminders)
            XCTAssertEqual(report.reminders.map(\.facts), reminders, "no permission state rewrites delivery facts")
        }
    }

    func testCountsAndOptionalPromptAllowance() {
        let reminders = [
            ReminderFixtures.resolved("a"),
            ReminderFixtures.resolved("b", schedule: "pending_schedule"),
            ReminderFixtures.unschedulable("c", reason: "capacity_exceeded"),
        ]
        let usable = NotificationReadiness.evaluate(permission: .usable, reminders: reminders)
        XCTAssertEqual(usable.scheduledCount, 1)
        XCTAssertEqual(usable.awaitingInstallCount, 1)
        XCTAssertEqual(usable.unscheduledCount, 1)
        XCTAssertEqual(usable.undeliverableCount, 0)
        XCTAssertTrue(usable.optionalPromptsAllowed)

        let denied = NotificationReadiness.evaluate(permission: .denied, reminders: reminders)
        XCTAssertEqual(denied.scheduledCount, 0)
        XCTAssertEqual(denied.undeliverableCount, 2)
        XCTAssertEqual(denied.unscheduledCount, 1)
        XCTAssertFalse(denied.optionalPromptsAllowed)
    }

    func testObservationStoreRoundTripsEveryState() throws {
        let suite = "ohand-permission-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = UserDefaultsNotificationPermissionObservationStore(defaults: defaults)
        XCTAssertNil(store.lastObserved())
        for state in [NotificationPermissionState.usable, .notRequested, .denied] {
            store.record(state)
            XCTAssertEqual(store.lastObserved(), state)
        }
    }
}
