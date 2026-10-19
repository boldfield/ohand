import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real reminder section in a window over the real model (with a scripted editor) and reads what each
/// identified part shows.
@MainActor
final class ReminderEditViewTests: XCTestCase {
    private static let clock = Date(timeIntervalSince1970: 1_800_000_000)
    private var rendered: [String: String] = [:]
    private var retainedWindow: UIWindow?

    override func tearDown() async throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        try await super.tearDown()
    }

    private func record(
        state: String, reason: String? = nil, schedule: String = "pending_schedule", due: Date?, zone: String?
    ) -> ReminderRecordAnswer {
        ReminderRecordAnswer(
            reminderID: "reminder-1", requestState: state, unschedulableReason: reason, scheduleState: schedule,
            deliveryState: "unknown", resolvedInstant: due.map(ReminderInstant.string(from:)), timezoneID: zone,
            scheduleGeneration: 1, stateVersion: 1, updatedAt: "2026-10-01T09:30:00Z")
    }

    private func host(_ reminder: ReminderRecordAnswer?) async {
        let editor = FakeReminderEditor(
            read: .success(
                ReminderReadAnswer(
                    itemID: "item-1", itemRevision: 3, itemType: "action", lifecycleState: "active", reminder: reminder)))
        let model = ReminderEditModel(
            itemID: "item-1", editor: editor, now: { Self.clock }, deviceTimeZone: { TimeZone(identifier: "UTC")! })
        let hosting = UIHostingController(
            rootView: ScrollView { ReminderEditSection(model: model, renderReporter: { [unowned self] in rendered = $0 }) })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        await model.load()
        for _ in 0..<5 {
            hosting.view.setNeedsLayout()
            hosting.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.2))
        }
    }

    func testAPassedStoredTimeIsShownInItsZoneWithTheExpiredStateAndNoCancelOffer() async {
        await host(
            record(
                state: "unschedulable", reason: "time_in_past", due: Self.clock.addingTimeInterval(-3600),
                zone: "Asia/Tokyo"))

        XCTAssertTrue(rendered[ReminderEditSection.storedTimeIdentifier]?.hasPrefix("Stored time: 2027-01-15 16:00 ") == true)
        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Time has passed") == true)
        XCTAssertEqual(rendered[ReminderEditSection.saveIdentifier], "Save time")
    }

    func testAResolvedReminderShowsItsZoneStoredTimeAndHonestSavedStateWithCancel() async {
        await host(record(state: "resolved", due: Self.clock.addingTimeInterval(7200), zone: "UTC"))

        XCTAssertEqual(rendered[ReminderEditSection.zoneIdentifier], "Time zone of this reminder: UTC")
        XCTAssertEqual(rendered[ReminderEditSection.storedTimeIdentifier], "Stored time: 2027-01-15 10:00 UTC")
        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Saved") == true)
        XCTAssertEqual(rendered[ReminderEditSection.cancelIdentifier], "Cancel reminder")
    }

    func testAnUnsupportedRepeatIsNamedAndNothingIsStored() async {
        await host(record(state: "unsupported_recurrence", due: nil, zone: nil))

        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Repeat not supported") == true)
        XCTAssertNil(rendered[ReminderEditSection.storedTimeIdentifier])
    }
}
