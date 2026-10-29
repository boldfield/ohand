import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// A detail reader whose stored lifecycle the test moves, standing in for the core.
private final class MovingDetailReader: SourceDetailReading, @unchecked Sendable {
    var revision = 3
    var lifecycleState = "active"
    func detail(itemID: String) async throws -> CoreItemDetail {
        CoreItemDetail(
            itemID: itemID, captureID: "capture-1", revision: revision, itemType: "action", scope: "work",
            sessionTopic: nil, lifecycleState: lifecycleState, capturedAt: "2026-10-01T09:30:00Z", text: "words",
            textBasis: .original, originalText: "words", correctedText: nil, audioReference: nil, transcript: nil,
            history: [], historyTruncated: false)
    }
}

private final class NoCorrectionsAllowed: ItemControlsCorrecting, @unchecked Sendable {
    func correctText(commandID: String, itemID: String, expectedRevision: Int, text: String) async throws -> ItemCorrectionReceipt { fatalError() }
    func correctType(commandID: String, itemID: String, expectedRevision: Int, itemType: ItemCorrectionItemType) async throws -> ItemCorrectionReceipt { fatalError() }
    func correctSessionTopic(commandID: String, itemID: String, expectedRevision: Int, sessionTopic: String) async throws -> ItemCorrectionReceipt { fatalError() }
}

/// Moves the reader's lifecycle the way the core does, and answers reopen from a script.
private final class LifecycleStandIn: ItemControlsLifecycleActing, @unchecked Sendable {
    let reader: MovingDetailReader
    var calls: [String] = []
    var reopenResults: [Result<ItemReopenResult, ItemLifecycleControlError>] = []
    init(_ reader: MovingDetailReader) { self.reader = reader }

    private func end(_ name: String, _ state: String, _ revision: Int) -> ItemLifecycleControlReceipt {
        calls.append("\(name):\(revision)")
        reader.lifecycleState = state
        reader.revision = revision + 1
        return ItemLifecycleControlReceipt(itemID: "item-1", revision: revision + 1, replayed: false)
    }
    func complete(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        end("complete", "completed", expectedRevision)
    }
    func cancel(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        end("cancel", "cancelled", expectedRevision)
    }
    func notNow(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt { fatalError() }
    func stopSuggesting(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt { fatalError() }
    func reopen(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemReopenResult {
        calls.append("reopen:\(expectedRevision)")
        let result = reopenResults.isEmpty ? nil : try reopenResults.removeFirst().get()
        if let result {
            if case .reopened = result { reader.lifecycleState = "active"; reader.revision = expectedRevision + 1 }
            return result
        }
        reader.lifecycleState = "active"
        reader.revision = expectedRevision + 1
        return .reopened(
            ItemReopenReceipt(itemID: itemID, revision: expectedRevision + 1, replayed: false, formerReminderInstant: nil))
    }
}

/// Hosts the real controls over the real model with scripted services, in both appearances.
@MainActor
final class ItemReopenControlsTests: XCTestCase {
    private static let clock = Date(timeIntervalSince1970: 1_800_000_000)
    private var onScreen: Set<String> = []
    private var retainedWindow: UIWindow?
    private var reader: MovingDetailReader!
    private var actions: LifecycleStandIn!
    private var controls: ItemControlsModel!

    override func setUp() async throws {
        try await super.setUp()
        reader = MovingDetailReader()
        actions = LifecycleStandIn(reader)
    }

    override func tearDown() async throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        controls = nil
        try await super.tearDown()
    }

    private func host(state: String = "active") async {
        reader.lifecycleState = state
        var counter = 0
        let editor = FakeReminderEditor(
            read: .success(
                ReminderReadAnswer(itemID: "item-1", itemRevision: 4, itemType: "action", lifecycleState: "active", reminder: nil)))
        controls = ItemControlsModel(
            itemID: "item-1", reader: reader, corrector: NoCorrectionsAllowed(), lifecycle: actions,
            makeCommandID: { counter += 1; return "command-\(counter)" },
            makeReminderEdit: { ReminderEditModel(itemID: $0, editor: editor, now: { Self.clock }, deviceTimeZone: { TimeZone(identifier: "UTC")! }) },
            now: { Self.clock })
        let hosting = UIHostingController(
            rootView: ScrollView {
                ItemControlsView(model: controls, renderReporter: { [unowned self] id, shown in
                    if shown { onScreen.insert(id) } else { onScreen.remove(id) }
                })
            })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        await controls.load()
        await settle()
    }

    private func settle() async {
        for _ in 0..<5 {
            retainedWindow?.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 60_000_000)
        }
    }

    private func eachAppearance(_ body: (String) async -> Void) async {
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            await setUp()
            await host()
            retainedWindow?.overrideUserInterfaceStyle = style
            await settle()
            await body(name)
            retainedWindow?.isHidden = true
        }
    }

    func testUndoAfterCompleteAndAfterCancelReopensAtTheNewRevision() async {
        await eachAppearance { name in
            for ending in ["complete", "cancel"] {
                if ending == "complete" { await controls.complete() } else { await controls.cancel() }
                await settle()
                XCTAssertTrue(onScreen.contains(ItemControlsView.undoIdentifier), "\(name) \(ending): Undo is offered")
                XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), name)
                await controls.reopen()
                await settle()
                XCTAssertEqual(controls.outcome, .actionDone(.reopen), name)
                XCTAssertEqual(controls.snapshot?.lifecycleState, "active", name)
                XCTAssertFalse(onScreen.contains(ItemControlsView.undoIdentifier), "\(name): Undo is spent")
            }
            XCTAssertEqual(actions.calls, ["complete:3", "reopen:4", "cancel:5", "reopen:6"], name)
        }
    }

    func testUndoStaysOfferedAfterTheConfirmationLineIsSeenAgain() async {
        await host()
        await controls.complete()
        await controls.load(resetting: [])
        XCTAssertTrue(controls.canUndo)
    }

    func testAClosedItemShowsReopenInPlaceOfCompleteAndCancelFromEachState() async {
        for state in ["completed", "cancelled"] {
            await setUp()
            await host(state: state)
            for style in [UIUserInterfaceStyle.light, .dark] {
                retainedWindow?.overrideUserInterfaceStyle = style
                await settle()
                XCTAssertTrue(onScreen.contains(ItemControlsView.endedNoticeIdentifier), "\(state) \(style)")
                XCTAssertTrue(onScreen.contains(ItemControlsView.reopenIdentifier), "\(state) \(style)")
            }
            XCTAssertEqual(ItemControlsView.endedNotice(state), state == "completed" ? "This item is completed." : "This item is cancelled.")
            XCTAssertTrue(controls.canReopen)
            XCTAssertFalse(controls.canAct)
            XCTAssertFalse(controls.canUndo, "an item closed elsewhere has nothing to undo")
            await controls.reopen()
            await settle()
            XCTAssertEqual(controls.snapshot?.lifecycleState, "active")
            XCTAssertFalse(onScreen.contains(ItemControlsView.reopenIdentifier))
            retainedWindow?.isHidden = true
        }
    }

    func testAlreadyOpenAndStaleOutcomesWriteNothingMore() async {
        await host(state: "completed")
        actions.reopenResults = [.success(.alreadyOpen), .failure(.staleRevision(currentRevision: 9))]
        await controls.reopen()
        XCTAssertEqual(controls.outcome, .actionAlreadyOpen)
        XCTAssertEqual(
            ItemControlsView.message(for: .actionAlreadyOpen), "This item is already open. Nothing was changed.")
        XCTAssertNil(controls.reminderAgainPrompt)
        reader.lifecycleState = "completed"
        await controls.load()
        await controls.reopen()
        XCTAssertEqual(controls.outcome, .actionConflict(.reopen, currentRevision: 9))
        XCTAssertNil(controls.reminderAgainPrompt)
    }

    private func reopenCarrying(_ instant: String?) async {
        actions.reopenResults = [
            .success(.reopened(ItemReopenReceipt(itemID: "item-1", revision: 4, replayed: false, formerReminderInstant: instant)))
        ]
        await host(state: "completed")
        await controls.reopen()
        await settle()
    }

    func testAFutureFormerReminderIsOfferedAgainAndPrefillsTheEdit() async {
        let future = Self.clock.addingTimeInterval(7200)
        await reopenCarrying(ReminderInstant.string(from: future))
        XCTAssertEqual(ItemControlsView.reminderOffNotice, "Its reminder was turned off.")
        for style in [UIUserInterfaceStyle.light, .dark] {
            retainedWindow?.overrideUserInterfaceStyle = style
            await settle()
            XCTAssertTrue(onScreen.contains(ItemControlsView.reminderOffIdentifier))
            XCTAssertTrue(onScreen.contains(ItemControlsView.setReminderAgainIdentifier))
        }
        XCTAssertNil(controls.reminderEdit, "nothing is opened or scheduled until asked")
        await controls.setReminderAgain()
        await settle()
        XCTAssertEqual(controls.reminderEdit?.draftDate, future)
        XCTAssertEqual(controls.reminderEdit?.reminder == nil, true, "no reminder is stored by opening the edit")
    }

    func testAPastFormerReminderOpensAnEmptyEdit() async {
        await reopenCarrying(ReminderInstant.string(from: Self.clock.addingTimeInterval(-7200)))
        XCTAssertTrue(onScreen.contains(ItemControlsView.setReminderAgainIdentifier))
        await controls.setReminderAgain()
        XCTAssertEqual(controls.reminderEdit?.draftDate, Self.clock.addingTimeInterval(3600), "the default, not the past time")
    }

    func testNoFormerReminderOffersNothing() async {
        await reopenCarrying(nil)
        XCTAssertNil(controls.reminderAgainPrompt)
        XCTAssertFalse(onScreen.contains(ItemControlsView.reminderOffIdentifier))
        XCTAssertFalse(onScreen.contains(ItemControlsView.setReminderAgainIdentifier))
    }
}
