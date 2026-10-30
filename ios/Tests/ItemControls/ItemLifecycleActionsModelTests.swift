import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class FixedDetailReader: SourceDetailReading, @unchecked Sendable {
    var detail: CoreItemDetail
    init(_ detail: CoreItemDetail) { self.detail = detail }
    func detail(itemID: String) async throws -> CoreItemDetail { detail }
}

private final class NoCorrections: ItemControlsCorrecting, @unchecked Sendable {
    func correctText(commandID: String, itemID: String, expectedRevision: Int, text: String) async throws -> ItemCorrectionReceipt { fatalError() }
    func correctType(commandID: String, itemID: String, expectedRevision: Int, itemType: ItemCorrectionItemType) async throws -> ItemCorrectionReceipt { fatalError() }
    func correctSessionTopic(commandID: String, itemID: String, expectedRevision: Int, sessionTopic: String) async throws -> ItemCorrectionReceipt { fatalError() }
}

private final class RecordingActions: ItemControlsLifecycleActing, @unchecked Sendable {
    var calls: [String] = []
    var results: [Result<ItemLifecycleControlReceipt, ItemLifecycleControlError>] = []

    private func next(_ name: String, _ command: String, _ revision: Int) throws -> ItemLifecycleControlReceipt {
        calls.append("\(name):\(command):\(revision)")
        if results.isEmpty { return ItemLifecycleControlReceipt(itemID: "item-1", revision: revision + 1, replayed: false) }
        return try results.removeFirst().get()
    }
    func complete(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        try next("complete", commandID, expectedRevision)
    }
    func cancel(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        try next("cancel", commandID, expectedRevision)
    }
    func notNow(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        try next("notNow", commandID, expectedRevision)
    }
    func stopSuggesting(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemLifecycleControlReceipt {
        try next("stop", commandID, expectedRevision)
    }
    var reopenResults: [Result<ItemReopenResult, ItemLifecycleControlError>] = []
    func reopen(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemReopenResult {
        calls.append("reopen:\(commandID):\(expectedRevision)")
        if reopenResults.isEmpty {
            return .reopened(
                ItemReopenReceipt(
                    itemID: itemID, revision: expectedRevision + 1, replayed: false, formerReminderInstant: nil))
        }
        return try reopenResults.removeFirst().get()
    }
}

@MainActor
final class ItemLifecycleActionsModelTests: XCTestCase {
    private func detail(revision: Int = 3, state: String = "active") -> CoreItemDetail {
        CoreItemDetail(
            itemID: "item-1", captureID: "capture-1", revision: revision, itemType: "idea", scope: "work",
            sessionTopic: nil, lifecycleState: state, capturedAt: "2026-10-01T09:30:00Z", text: "words",
            textBasis: .original, originalText: "words", correctedText: nil, audioReference: nil, transcript: nil,
            history: [], historyTruncated: false)
    }

    private func make(_ reader: FixedDetailReader, _ actions: RecordingActions) -> ItemControlsModel {
        var counter = 0
        return ItemControlsModel(
            itemID: "item-1", reader: reader, corrector: NoCorrections(), lifecycle: actions,
            makeCommandID: { counter += 1; return "command-\(counter)" })
    }

    func testEachActionNamesTheRevisionTheUserSawAndKeepsDrafts() async {
        let actions = RecordingActions()
        let model = make(FixedDetailReader(detail()), actions)
        await model.load()
        model.textDraft = "unsaved edit"
        await model.notNow()
        await model.stopSuggesting()
        XCTAssertEqual(actions.calls, ["notNow:command-1:3", "stop:command-2:3"])
        XCTAssertEqual(model.outcome, .actionDone(.stopSuggesting))
        XCTAssertEqual(model.textDraft, "unsaved edit")
    }

    func testRetryAfterAnUnknownOutcomeReusesTheCommandID() async {
        let actions = RecordingActions()
        actions.results = [.failure(.unreadable)]
        let model = make(FixedDetailReader(detail()), actions)
        await model.load()
        await model.complete()
        XCTAssertEqual(model.outcome, .actionFailed(.complete))
        await model.complete()
        XCTAssertEqual(actions.calls, ["complete:command-1:3", "complete:command-1:3"])
        XCTAssertEqual(model.outcome, .actionDone(.complete))
    }

    func testStaleRevisionIsAConflictAndEndedItemsOfferNoActions() async {
        let actions = RecordingActions()
        actions.results = [.failure(.staleRevision(currentRevision: 5))]
        let reader = FixedDetailReader(detail())
        let model = make(reader, actions)
        await model.load()
        reader.detail = detail(revision: 5)
        await model.cancel()
        XCTAssertEqual(model.outcome, .actionConflict(.cancel, currentRevision: 5))
        XCTAssertEqual(model.snapshot?.revision, 5)

        actions.results = [.failure(.itemEnded(lifecycle: "completed"))]
        reader.detail = detail(revision: 6, state: "completed")
        await model.complete()
        XCTAssertEqual(model.outcome, .actionItemEnded(lifecycle: "completed"))
        XCTAssertFalse(model.canAct)
        await model.notNow()
        XCTAssertEqual(actions.calls.count, 2, "no action is sent for an ended item")
    }

    func testADeletedTargetIsReportedAsGone() async {
        let actions = RecordingActions()
        actions.results = [.failure(.itemDeleted)]
        let model = make(FixedDetailReader(detail()), actions)
        await model.load()
        await model.stopSuggesting()
        XCTAssertEqual(model.outcome, .itemGone)
    }

    func testExplanationsMentionCooldownAndPullOnlyBehavior() {
        XCTAssertTrue(ItemControlsView.notNowExplanation.contains("an hour"))
        XCTAssertTrue(ItemControlsView.stopSuggestingExplanation.contains("searchable"))
        XCTAssertTrue(ItemControlsView.stopSuggestingExplanation.contains("reminder"))
    }
}
