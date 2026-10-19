import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class StubDetailReader: SourceDetailReading, @unchecked Sendable {
    var detail: CoreItemDetail
    var failure: ItemDetailError?
    init(_ detail: CoreItemDetail) { self.detail = detail }
    func detail(itemID: String) async throws -> CoreItemDetail {
        if let failure { throw failure }
        return detail
    }
}

private final class RecordingCorrector: ItemControlsCorrecting, @unchecked Sendable {
    enum Call: Equatable {
        case text(command: String, revision: Int, text: String)
        case type(command: String, revision: Int, type: ItemCorrectionItemType)
        case topic(command: String, revision: Int, topic: String)
    }

    var calls: [Call] = []
    var results: [Result<ItemCorrectionReceipt, ItemCorrectionError>] = []

    private func next(_ itemID: String) throws -> ItemCorrectionReceipt {
        if results.isEmpty { return ItemCorrectionReceipt(itemID: itemID, revision: 0, replayed: false, reminderCancelled: false) }
        return try results.removeFirst().get()
    }

    func correctText(commandID: String, itemID: String, expectedRevision: Int, text: String) async throws -> ItemCorrectionReceipt {
        calls.append(.text(command: commandID, revision: expectedRevision, text: text))
        return try next(itemID)
    }

    func correctType(commandID: String, itemID: String, expectedRevision: Int, itemType: ItemCorrectionItemType) async throws -> ItemCorrectionReceipt {
        calls.append(.type(command: commandID, revision: expectedRevision, type: itemType))
        return try next(itemID)
    }

    func correctSessionTopic(commandID: String, itemID: String, expectedRevision: Int, sessionTopic: String) async throws -> ItemCorrectionReceipt {
        calls.append(.topic(command: commandID, revision: expectedRevision, topic: sessionTopic))
        return try next(itemID)
    }
}

@MainActor
final class ItemControlsModelTests: XCTestCase {
    private static let itemID = "item-controls-1"

    private func detail(revision: Int = 3, type: String? = "idea", topic: String? = "garage", text: String = "fix the shelf") -> CoreItemDetail {
        CoreItemDetail(
            itemID: Self.itemID, captureID: "capture-1", revision: revision, itemType: type, scope: "work",
            sessionTopic: topic, lifecycleState: "active", capturedAt: "2026-10-01T09:30:00Z", text: text,
            textBasis: .original, originalText: text, correctedText: nil, audioReference: nil, transcript: nil,
            history: [], historyTruncated: false)
    }

    private func make(_ reader: StubDetailReader, _ corrector: RecordingCorrector) -> ItemControlsModel {
        var counter = 0
        return ItemControlsModel(itemID: Self.itemID, reader: reader, corrector: corrector, makeCommandID: {
            counter += 1
            return "command-\(counter)"
        })
    }

    func testLoadSeedsDraftsFromTheStoredItem() async {
        let model = make(StubDetailReader(detail()), RecordingCorrector())
        await model.load()
        XCTAssertEqual(model.textDraft, "fix the shelf")
        XCTAssertEqual(model.topicDraft, "garage")
        XCTAssertEqual(model.pendingType, .idea)
        XCTAssertFalse(model.canSaveText)
        XCTAssertFalse(model.canApplyType)
    }

    /// Stored scope and type are checked against the real core in `ItemControlsRealCoreTests`; this only pins what is sent.
    func testTopicChangeSendsOnlyATopicCorrection() async {
        let corrector = RecordingCorrector()
        let model = make(StubDetailReader(detail()), corrector)
        await model.load()
        model.topicDraft = "kitchen"
        await model.saveSessionTopic()
        XCTAssertEqual(corrector.calls, [.topic(command: "command-1", revision: 3, topic: "kitchen")])
    }

    func testSavingOneFieldKeepsUnsavedEditsInTheOthers() async {
        let reader = StubDetailReader(detail(type: "idea", topic: "garage", text: "old"))
        let corrector = RecordingCorrector()
        let model = make(reader, corrector)
        await model.load()
        model.topicDraft = "kitchen"
        model.pendingType = .note
        model.textDraft = "new words"
        reader.detail = detail(revision: 4, type: "idea", topic: "garage", text: "new words")
        await model.saveText()
        XCTAssertEqual(model.textDraft, "new words")
        XCTAssertEqual(model.topicDraft, "kitchen", "the unsaved topic edit survives saving the words")
        XCTAssertEqual(model.pendingType, .note, "the unapplied type pick survives saving the words")
        XCTAssertTrue(model.canSaveTopic)
        XCTAssertTrue(model.canApplyType)
    }

    func testAFailedRefreshAfterASaveKeepsOtherDraftsAndTheSavedOutcome() async {
        let reader = StubDetailReader(detail(type: "idea", topic: "garage", text: "old"))
        let model = make(reader, RecordingCorrector())
        await model.load()
        model.topicDraft = "kitchen"
        model.pendingType = .note
        model.textDraft = "new words"
        reader.failure = .relocked
        await model.saveText()
        XCTAssertEqual(model.outcome, .saved(.text))
        XCTAssertEqual(model.topicDraft, "kitchen")
        XCTAssertEqual(model.pendingType, .note)
        XCTAssertEqual(model.textDraft, "new words")
    }

    func testAFailedRefreshAfterAConflictKeepsTheRejectedDraftAndTheConflict() async {
        let reader = StubDetailReader(detail())
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.staleRevision(currentRevision: 5))]
        let model = make(reader, corrector)
        await model.load()
        model.topicDraft = "kitchen"
        reader.failure = .notAuthenticated
        await model.saveSessionTopic()
        XCTAssertEqual(model.outcome, .conflict(.sessionTopic, currentRevision: 5))
        XCTAssertEqual(model.topicDraft, "kitchen")
    }

    func testAConflictRefreshesTheHostSoItAgreesWithTheNewerValues() async {
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.staleRevision(currentRevision: 5))]
        var refreshes = 0
        let model = ItemControlsModel(
            itemID: Self.itemID, reader: StubDetailReader(detail()), corrector: corrector, onCorrected: { refreshes += 1 })
        await model.load()
        model.topicDraft = "kitchen"
        await model.saveSessionTopic()
        XCTAssertEqual(model.outcome, .conflict(.sessionTopic, currentRevision: 5))
        XCTAssertEqual(refreshes, 1)
    }

    func testEveryOutcomeHasAMessageAndTheReminderOneSaysSo() {
        XCTAssertTrue(ItemControlsView.message(for: .savedAndReminderCancelled).contains("reminder"))
        XCTAssertTrue(ItemControlsView.message(for: .conflict(.text, currentRevision: 2)).contains("nothing was saved"))
        XCTAssertFalse(ItemControlsView.message(for: .loadFailed).isEmpty)
    }

    func testPickingActionDoesNotApplyItUntilTheUserConfirms() async {
        let corrector = RecordingCorrector()
        let model = make(StubDetailReader(detail(type: "idea")), corrector)
        await model.load()
        model.pendingType = .action
        XCTAssertTrue(model.pendingTypeMakesAction)
        XCTAssertTrue(corrector.calls.isEmpty)
        await model.applyType()
        XCTAssertEqual(corrector.calls, [.type(command: "command-1", revision: 3, type: .action)])
    }

    func testTextAndTopicEditsDoNotChangeTheType() async {
        let corrector = RecordingCorrector()
        let model = make(StubDetailReader(detail(type: "idea")), corrector)
        await model.load()
        model.textDraft = "call the roofer tomorrow"
        await model.saveText()
        model.topicDraft = "roof"
        await model.saveSessionTopic()
        for call in corrector.calls {
            if case .type = call { XCTFail("a text or topic edit must not send a type correction") }
        }
    }

    func testStaleRevisionShowsAConflictKeepsTheDraftAndReloadsNewerValues() async {
        let reader = StubDetailReader(detail(revision: 3, text: "old"))
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.staleRevision(currentRevision: 5))]
        let model = make(reader, corrector)
        await model.load()
        model.textDraft = "my edit"
        reader.detail = detail(revision: 5, text: "someone else's edit")
        await model.saveText()
        XCTAssertEqual(model.outcome, .conflict(.text, currentRevision: 5))
        XCTAssertEqual(model.textDraft, "my edit", "the user's draft is not overwritten")
        XCTAssertEqual(model.snapshot?.revision, 5)
        XCTAssertEqual(model.snapshot?.text, "someone else's edit")
        XCTAssertEqual(corrector.calls.count, 1, "nothing is resent until the user asks again")
        await model.saveText()
        XCTAssertEqual(corrector.calls.last, .text(command: "command-2", revision: 5, text: "my edit"))
    }

    func testRetryAfterAnUnknownOutcomeReusesTheCommandID() async {
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.unreadable)]
        let model = make(StubDetailReader(detail()), corrector)
        await model.load()
        model.topicDraft = "kitchen"
        await model.saveSessionTopic()
        XCTAssertEqual(model.outcome, .failed(.sessionTopic))
        await model.saveSessionTopic()
        XCTAssertEqual(corrector.calls.map { call -> String in
            if case .topic(let command, _, _) = call { return command }
            return ""
        }, ["command-1", "command-1"])
    }

    func testTypeChangeThatCancelledTheReminderIsReported() async {
        let corrector = RecordingCorrector()
        corrector.results = [.success(ItemCorrectionReceipt(itemID: Self.itemID, revision: 4, replayed: false, reminderCancelled: true))]
        let model = make(StubDetailReader(detail(type: "action")), corrector)
        await model.load()
        model.pendingType = .note
        await model.applyType()
        XCTAssertEqual(model.outcome, .savedAndReminderCancelled)
    }

    func testBlankDraftsCannotBeSaved() async {
        let model = make(StubDetailReader(detail()), RecordingCorrector())
        await model.load()
        model.textDraft = "   "
        model.topicDraft = ""
        XCTAssertFalse(model.canSaveText)
        XCTAssertFalse(model.canSaveTopic)
    }

    func testLockedSessionIsReportedWithoutChangingTheItem() async {
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.notAuthenticated)]
        let model = make(StubDetailReader(detail()), corrector)
        await model.load()
        model.topicDraft = "kitchen"
        await model.saveSessionTopic()
        XCTAssertEqual(model.outcome, .locked)
    }
}
