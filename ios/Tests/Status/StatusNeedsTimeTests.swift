import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class IdleHealth: ProcessingHealthReading, @unchecked Sendable {
    func read() async -> ProcessingHealthOutcome { .failed(CoreFailure.handleClosed) }
}

private final class GrantedPermissions: PermissionsReading, @unchecked Sendable {
    func refresh() async -> [PermissionEntry] {
        [PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)]
    }
}

private final class NoDrain: ProcessingRetrying {
    func retryNow() {}
}

/// A pass the test can replace, as the next reminder pass would.
private final class ChangingScheduling: ReminderSchedulingReading {
    var report: NotificationPermissionReport?
    init(_ report: NotificationPermissionReport?) { self.report = report }
    func lastReport() -> NotificationPermissionReport? { report }
}

/// Reads notes as the private read does: readable while unlocked, nothing while locked or for a deleted note.
private final class ScriptedNotes: FailedNoteTextReading, @unchecked Sendable {
    var isLocked = false
    var deleted: Set<String> = []
    var capturedAt: [String: Date] = [:]

    func firstLine(itemID: String) async -> String? { await summary(itemID: itemID)?.firstLine }

    func summary(itemID: String) async -> NoteSummary? {
        if isLocked || deleted.contains(itemID) { return nil }
        return NoteSummary(firstLine: "Call about \(itemID)", capturedAt: capturedAt[itemID])
    }
}

private func needsTime(_ itemID: String) -> ReminderFacts {
    ReminderFacts(
        itemID: itemID, requestState: "not_scheduled_yet", scheduleState: nil, deliveryState: nil,
        acknowledgmentState: nil, unschedulableReason: nil)
}

private func pass(_ reminders: [ReminderFacts]) -> NotificationPermissionReport {
    NotificationPermissionReport(
        permission: .usable, transition: .unchanged, trigger: .explicit,
        readiness: NotificationReadiness.evaluate(permission: .usable, reminders: reminders),
        promptsGranted: 0, promptsDeferred: 0, promptsWithheld: 0, reconciliationFailure: nil, converged: true)
}

/// The Status entry for reminders that have no time: its fact, its listed notes and its link into the note.
@MainActor
final class StatusNeedsTimeTests: XCTestCase {
    private var window: UIWindow?
    private var rendered: [String: String] = [:]
    private var generation = 0
    private let appearances: [UIUserInterfaceStyle] = [.light, .dark]
    private let baseDate = Date(timeIntervalSince1970: 1_792_000_000)

    override func tearDownWithError() throws {
        window?.isHidden = true
        window = nil
        try super.tearDownWithError()
    }

    private func notes(count: Int, locked: Bool = false) -> ScriptedNotes {
        let notes = ScriptedNotes()
        notes.isLocked = locked
        // A higher number was captured later.
        for number in 1...max(count, 1) { notes.capturedAt["item-\(number)"] = baseDate.addingTimeInterval(Double(number) * 3600) }
        return notes
    }

    private func makeModel(
        _ scheduling: ChangingScheduling, notes: ScriptedNotes
    ) -> StatusModel {
        StatusModel(
            health: IdleHealth(), permissions: GrantedPermissions(), retrier: NoDrain(), scheduling: scheduling,
            noteText: notes, openSystemSettings: {})
    }

    private func host(_ model: StatusModel, style: UIUserInterfaceStyle, panes: PaneRecorder = PaneRecorder()) {
        window?.rootViewController = nil
        window?.isHidden = true
        window = nil
        rendered = [:]
        generation += 1
        let current = generation
        let screen = StatusView(
            model: model, makeItemPane: { [panes] in panes.record($0) },
            renderReporter: { [unowned self] in if current == generation { rendered = $0 } })
        let shown = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        shown.overrideUserInterfaceStyle = style
        shown.rootViewController = UIHostingController(rootView: NavigationStack { screen })
        shown.makeKeyAndVisible()
        window = shown
    }

    private func wait(_ condition: String, file: StaticString = #filePath, line: UInt = #line, until holds: () -> Bool) {
        waitUntilPumping(condition, timeout: 15, file: file, line: line, holds)
    }

    private var noteKeys: [String] { rendered.keys.filter { $0.hasPrefix("status.needs-time-note.") }.sorted() }

    // MARK: model

    func testTheFactCountsOnlyRemindersWithoutATimeAndIsNotAnUnscheduledReason() async {
        let scheduling = ChangingScheduling(
            pass([needsTime("item-1"), ReminderFixtures.resolved("item-2"), ReminderFixtures.unschedulable("item-3", reason: "capacity_exceeded")]))
        let model = makeModel(scheduling, notes: notes(count: 3))
        await model.refresh()

        let fact = model.facts.first { $0.kind == .remindersNeedTime }
        XCTAssertEqual(fact?.headline, "1 reminder needs a time")
        XCTAssertEqual(fact?.detail, "You asked to be reminded but didn't say when. Open the note to set a time.")
        XCTAssertEqual(fact?.tone, .needsAttention)
        let notificationsDetail = model.facts.first { $0.kind == .notifications }?.detail ?? ""
        XCTAssertTrue(notificationsDetail.hasPrefix("1 saved reminder is not scheduled"), "only the capacity reminder is unscheduled: \(notificationsDetail)")
        XCTAssertEqual(scheduling.report?.readiness.unscheduledCount, 1)
    }

    func testNoFactWithoutAReminderThatNeedsATime() async {
        let model = makeModel(ChangingScheduling(pass([ReminderFixtures.resolved("item-1")])), notes: notes(count: 1))
        await model.refresh()
        XCTAssertNil(model.facts.first { $0.kind == .remindersNeedTime })
        XCTAssertTrue(model.needsTimeNotes.isEmpty)

        let unchecked = makeModel(ChangingScheduling(nil), notes: notes(count: 1))
        await unchecked.refresh()
        XCTAssertNil(unchecked.facts.first { $0.kind == .remindersNeedTime })
    }

    func testTheNewestFiveAreListedNewestFirstAndTheHeadlineCountsAll() async {
        let reminders = (1...7).map { needsTime("item-\($0)") }
        let model = makeModel(ChangingScheduling(pass(reminders)), notes: notes(count: 7))
        await model.refresh()

        XCTAssertEqual(model.facts.first { $0.kind == .remindersNeedTime }?.headline, "7 reminders need a time")
        XCTAssertEqual(model.needsTimeNotes.map(\.itemID), ["item-7", "item-6", "item-5", "item-4", "item-3"])
    }

    func testLockedShowsOnlyTheCountAndUnlockingListsTheNotes() async {
        let scripted = notes(count: 2, locked: true)
        let model = makeModel(ChangingScheduling(pass([needsTime("item-1"), needsTime("item-2")])), notes: scripted)
        await model.refresh()
        XCTAssertEqual(model.facts.first { $0.kind == .remindersNeedTime }?.headline, "2 reminders need a time")
        XCTAssertTrue(model.needsTimeNotes.isEmpty, "no note is named while locked")

        scripted.isLocked = false
        await model.refresh()
        XCTAssertEqual(model.needsTimeNotes.map(\.itemID), ["item-2", "item-1"])
    }

    func testTheEntryGoesAwayWhenATimeIsSetTheReminderIsCancelledOrTheNoteIsDeleted() async {
        let scripted = notes(count: 3)
        let scheduling = ChangingScheduling(pass([needsTime("item-1"), needsTime("item-2"), needsTime("item-3")]))
        let model = makeModel(scheduling, notes: scripted)
        await model.refresh()
        XCTAssertEqual(model.needsTimeNotes.count, 3)

        // Item 1 got a time and resolved; item 2's reminder was cancelled. The core reports neither as needing a time.
        let cancelled = ReminderFacts(
            itemID: "item-2", requestState: "cancelled", scheduleState: nil, deliveryState: nil,
            acknowledgmentState: nil, unschedulableReason: nil)
        scheduling.report = pass([ReminderFixtures.resolved("item-1"), cancelled, needsTime("item-3")])
        await model.refresh()
        XCTAssertEqual(model.facts.first { $0.kind == .remindersNeedTime }?.headline, "1 reminder needs a time")
        XCTAssertEqual(model.needsTimeNotes.map(\.itemID), ["item-3"])

        // Item 3's note was deleted: the core's facts cover active items only, so it is gone from the pass.
        scripted.deleted = ["item-3"]
        scheduling.report = pass([ReminderFixtures.resolved("item-1"), cancelled])
        await model.refresh()
        XCTAssertNil(model.facts.first { $0.kind == .remindersNeedTime })
        XCTAssertTrue(model.needsTimeNotes.isEmpty)
    }

    // MARK: hosted

    func testLockedHostedScreenShowsTheCountWithNoNote() {
        for style in appearances {
            let model = makeModel(ChangingScheduling(pass([needsTime("item-1")])), notes: notes(count: 1, locked: true))
            host(model, style: style)
            let row = StatusView.rowIdentifier(.remindersNeedTime)
            wait("the count is shown, \(style.rawValue)") { self.rendered[row] != nil }

            XCTAssertEqual(
                rendered[row],
                "1 reminder needs a time | You asked to be reminded but didn't say when. Open the note to set a time.")
            XCTAssertEqual(noteKeys, [], "no note is named while locked")
        }
    }

    func testUnlockedHostedScreenListsOneNote() {
        for style in appearances {
            let model = makeModel(ChangingScheduling(pass([needsTime("item-1")])), notes: notes(count: 1))
            host(model, style: style)
            let identifier = StatusView.needsTimeNoteIdentifier("item-1")
            wait("the note is listed, \(style.rawValue)") { self.rendered[identifier] != nil }

            let captured = StatusText.capturedLabel(baseDate.addingTimeInterval(3600))
            XCTAssertEqual(rendered[identifier], "Call about item-1 | \(captured)")
        }
    }

    func testUnlockedHostedScreenListsAtMostFiveNewestFirst() {
        for style in appearances {
            let model = makeModel(ChangingScheduling(pass((1...7).map { needsTime("item-\($0)") })), notes: notes(count: 7))
            host(model, style: style)
            wait("five notes are listed, \(style.rawValue)") { self.noteKeys.count == 5 }

            XCTAssertEqual(
                noteKeys, (3...7).map { StatusView.needsTimeNoteIdentifier("item-\($0)") }.sorted(),
                "the two oldest are not listed")
            XCTAssertTrue(rendered[StatusView.rowIdentifier(.remindersNeedTime)]?.hasPrefix("7 reminders need a time") == true)
        }
    }

    func testARowLeadsToThatNotesDetail() {
        let panes = PaneRecorder()
        let row = NeedsTimeNoteRow(
            note: NeedsTimeNote(itemID: "item-9", firstLine: "Call Joe", capturedAt: nil), makeItemPane: { panes.record($0) })
        _ = row.destination
        XCTAssertEqual(panes.itemIDs, ["item-9"], "the destination is the item pane for the tapped note")
    }
}

/// Stands in for the catalog's item destination and remembers which item it was built for.
private final class PaneRecorder: @unchecked Sendable {
    private(set) var itemIDs: [String] = []

    func record(_ itemID: String) -> AnyView {
        itemIDs.append(itemID)
        return AnyView(Text("Item \(itemID)"))
    }
}
