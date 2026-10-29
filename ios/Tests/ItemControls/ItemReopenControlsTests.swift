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
        onScreen = []
        reader = MovingDetailReader()
        actions = LifecycleStandIn(reader)
    }

    override func tearDown() async throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        controls = nil
        try await super.tearDown()
    }

    private func host(state: String = "active", style: UIUserInterfaceStyle = .light) async {
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
        window.overrideUserInterfaceStyle = style
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
            try? await setUp()
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

    func testUndoStaysOfferedOnScreenAfterTheConfirmationLineChangesAndAfterAFailedUndo() async {
        await eachAppearance { name in
            await controls.complete()
            await controls.load(resetting: [])
            await settle()
            XCTAssertTrue(onScreen.contains(ItemControlsView.undoIdentifier), "\(name): still offered after a reload")
            actions.reopenResults = [.failure(.invalidRequest)]
            await controls.reopen()
            await settle()
            XCTAssertEqual(controls.outcome, .actionFailed(.reopen), name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.undoIdentifier), "\(name): offered again after a failed Undo")
            await controls.reopen()
            await settle()
            XCTAssertFalse(onScreen.contains(ItemControlsView.undoIdentifier), "\(name): spent once reopened")
        }
    }

    func testAClosedItemShowsReopenInPlaceOfCompleteAndCancelFromEachState() async {
        for state in ["completed", "cancelled"] {
            try? await setUp()
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

    func testAlreadyOpenAndStaleOutcomesShowTheirMessageAndWriteNothingMore() async {
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            try? await setUp()
            await host(state: "completed", style: style)
            actions.reopenResults = [.success(.alreadyOpen), .failure(.staleRevision(currentRevision: 9))]
            reader.lifecycleState = "active"
            await controls.reopen()
            await settle()
            XCTAssertEqual(controls.outcome, .actionAlreadyOpen, name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), "\(name): the message is drawn")
            XCTAssertFalse(onScreen.contains(ItemControlsView.reopenIdentifier), "\(name): the item is open, so no Reopen")
            XCTAssertFalse(onScreen.contains(ItemControlsView.undoIdentifier), name)
            XCTAssertFalse(onScreen.contains(ItemControlsView.reminderOffIdentifier), name)
            XCTAssertEqual(
                ItemControlsView.message(for: .actionAlreadyOpen), "This item is already open. Nothing was changed.")

            reader.lifecycleState = "completed"
            await controls.load()
            await settle()
            XCTAssertTrue(onScreen.contains(ItemControlsView.reopenIdentifier), name)
            onScreen.remove(ItemControlsView.outcomeIdentifier)
            await controls.reopen()
            await settle()
            XCTAssertEqual(controls.outcome, .actionConflict(.reopen, currentRevision: 9), name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), "\(name): the stale message is drawn")
            XCTAssertTrue(onScreen.contains(ItemControlsView.reopenIdentifier), "\(name): Reopen stays for a retry")
            XCTAssertFalse(onScreen.contains(ItemControlsView.reminderOffIdentifier), name)
            XCTAssertFalse(onScreen.contains(ItemControlsView.setReminderAgainIdentifier), name)
            XCTAssertNil(controls.reminderAgainPrompt, name)
            retainedWindow?.isHidden = true
        }
    }

    private func reopenCarrying(_ instant: String?, style: UIUserInterfaceStyle) async {
        actions.reopenResults = [
            .success(.reopened(ItemReopenReceipt(itemID: "item-1", revision: 4, replayed: false, formerReminderInstant: instant)))
        ]
        await host(state: "completed", style: style)
        await controls.reopen()
        await settle()
    }

    func testAFutureFormerReminderIsOfferedAgainAndPrefillsTheEdit() async {
        let future = Self.clock.addingTimeInterval(7200)
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            try? await setUp()
            await reopenCarrying(ReminderInstant.string(from: future), style: style)
            XCTAssertEqual(ItemControlsView.reminderOffNotice, "Its reminder was turned off.")
            XCTAssertTrue(onScreen.contains(ItemControlsView.reminderOffIdentifier), name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.setReminderAgainIdentifier), name)
            XCTAssertNil(controls.reminderEdit, "\(name): nothing is opened or scheduled until asked")
            await controls.setReminderAgain()
            await settle()
            XCTAssertEqual(controls.reminderEdit?.draftDate, future, name)
            XCTAssertEqual(controls.reminderEdit?.reminder == nil, true, "no reminder is stored by opening the edit")
            XCTAssertTrue(onScreen.contains(ItemControlsView.reminderAgainDoneIdentifier), "\(name): the edit is drawn")
            retainedWindow?.isHidden = true
        }
    }

    func testAPastFormerReminderOpensAnEmptyEdit() async {
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            try? await setUp()
            await reopenCarrying(ReminderInstant.string(from: Self.clock.addingTimeInterval(-7200)), style: style)
            XCTAssertTrue(onScreen.contains(ItemControlsView.reminderOffIdentifier), name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.setReminderAgainIdentifier), name)
            await controls.setReminderAgain()
            await settle()
            XCTAssertEqual(
                controls.reminderEdit?.draftDate, Self.clock.addingTimeInterval(3600), "\(name): the default, not the past time")
            XCTAssertTrue(onScreen.contains(ItemControlsView.reminderAgainDoneIdentifier), "\(name): the edit is drawn")
            retainedWindow?.isHidden = true
        }
    }

    func testNoFormerReminderOffersNothing() async {
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            try? await setUp()
            await reopenCarrying(nil, style: style)
            XCTAssertNil(controls.reminderAgainPrompt, name)
            XCTAssertTrue(onScreen.contains(ItemControlsView.outcomeIdentifier), name)
            XCTAssertFalse(onScreen.contains(ItemControlsView.reminderOffIdentifier), name)
            XCTAssertFalse(onScreen.contains(ItemControlsView.setReminderAgainIdentifier), name)
            retainedWindow?.isHidden = true
        }
    }
}
