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
    private var lastModel: ReminderEditModel?

    override func tearDown() async throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        try await super.tearDown()
    }

    private var diagnostics: String {
        "rendered=\(rendered) reminder=\(String(describing: lastModel?.reminder)) unavailable=\(String(describing: lastModel?.unavailable))"
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
        // Give the hosted view time to take the loaded state and report it, yielding so its own tasks can run.
        for _ in 0..<60 {
            hosting.view.setNeedsLayout()
            hosting.view.layoutIfNeeded()
            if rendered[ReminderEditSection.standingIdentifier] != nil { break }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        try? await Task.sleep(nanoseconds: 200_000_000)
        hosting.view.layoutIfNeeded()
        lastModel = model
    }

    func testAPassedStoredTimeIsShownInItsZoneWithTheExpiredStateAndNoCancelOffer() async {
        await host(
            record(
                state: "unschedulable", reason: "time_in_past", due: Self.clock.addingTimeInterval(-3600),
                zone: "Asia/Tokyo"))

        XCTAssertTrue(rendered[ReminderEditSection.storedTimeIdentifier]?.hasPrefix("Stored time: 2027-01-15 16:00 ") == true, diagnostics)
        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Time has passed") == true, diagnostics)
        XCTAssertEqual(rendered[ReminderEditSection.saveIdentifier], "Save time", diagnostics)
    }

    func testAResolvedReminderShowsItsZoneStoredTimeAndHonestSavedStateWithCancel() async {
        await host(record(state: "resolved", due: Self.clock.addingTimeInterval(7200), zone: "UTC"))

        XCTAssertEqual(rendered[ReminderEditSection.zoneIdentifier], "Time zone of this reminder: UTC", diagnostics)
        XCTAssertTrue(rendered[ReminderEditSection.storedTimeIdentifier]?.hasPrefix("Stored time: 2027-01-15 10:00 ") == true, diagnostics)
        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Saved") == true, diagnostics)
        XCTAssertEqual(rendered[ReminderEditSection.cancelIdentifier], "Cancel reminder", diagnostics)
    }

    func testAnUnsupportedRepeatIsNamedAndNothingIsStored() async {
        await host(record(state: "unsupported_recurrence", due: nil, zone: nil))

        XCTAssertTrue(rendered[ReminderEditSection.standingIdentifier]?.hasPrefix("Repeat not supported") == true, diagnostics)
        XCTAssertNil(rendered[ReminderEditSection.storedTimeIdentifier])
    }
}
