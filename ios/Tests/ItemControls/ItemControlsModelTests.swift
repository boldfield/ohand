import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class StubDetailReader: SourceDetailReading, @unchecked Sendable {
    var detail: CoreItemDetail
    var failure: ItemDetailError?
    /// Runs on every read before it answers, so a test can look at the model while a refresh is still pending.
    var duringRead: (@MainActor () async -> Void)?
    init(_ detail: CoreItemDetail) { self.detail = detail }
    func detail(itemID: String) async throws -> CoreItemDetail {
        if let duringRead { await duringRead() }
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

    func testAFailedRefreshAfterASaveKeepsDraftsAndTheSavedOutcomeButPausesEditing() async {
        let reader = StubDetailReader(detail(type: "idea", topic: "garage", text: "old"))
        let corrector = RecordingCorrector()
        let model = make(reader, corrector)
        await model.load()
        model.topicDraft = "kitchen"
        model.pendingType = .note
        model.textDraft = "new words"
        reader.failure = .core(.handleClosed)
        await model.saveText()
        XCTAssertEqual(model.outcome, .saved(.text))
        XCTAssertEqual(model.refreshFailure, .unreadable)
        XCTAssertEqual(model.topicDraft, "kitchen")
        XCTAssertEqual(model.pendingType, .note)
        XCTAssertEqual(model.textDraft, "new words")
        XCTAssertEqual(model.snapshot?.revision, 3, "the snapshot still names the revision from before the save")
        XCTAssertFalse(model.canEdit)
        XCTAssertFalse(model.canSaveText, "a second press cannot resend against the old revision")
        XCTAssertFalse(model.canSaveTopic)
        XCTAssertFalse(model.canApplyType)
        XCTAssertTrue(model.canReload)
        await model.saveText()
        XCTAssertEqual(corrector.calls.count, 1, "nothing is sent while the snapshot is out of date")
        XCTAssertEqual(ItemControlsView.message(for: model.outcome!, newerValuesShown: model.refreshFailure == nil),
                       "Saved. Your original is kept.")

        // Reading again picks up the new revision, keeps the unsaved drafts, and allows editing again.
        reader.failure = nil
        reader.detail = detail(revision: 4, type: "idea", topic: "garage", text: "new words")
        await model.reload()
        XCTAssertNil(model.refreshFailure)
        XCTAssertEqual(model.outcome, .saved(.text), "the result of the save stays shown")
        XCTAssertEqual(model.snapshot?.revision, 4)
        XCTAssertEqual(model.topicDraft, "kitchen")
        XCTAssertEqual(model.pendingType, .note)
        XCTAssertFalse(model.canSaveText, "the saved words match what is stored")
        XCTAssertTrue(model.canSaveTopic)
        await model.saveSessionTopic()
        XCTAssertEqual(corrector.calls.last, .topic(command: "command-2", revision: 4, topic: "kitchen"))
    }

    func testAFailedRefreshAfterAConflictKeepsTheDraftAndDoesNotClaimNewerValuesAreShown() async {
        let reader = StubDetailReader(detail(revision: 3, text: "old"))
        let corrector = RecordingCorrector()
        corrector.results = [.failure(.staleRevision(currentRevision: 5))]
        let model = make(reader, corrector)
        await model.load()
        model.topicDraft = "kitchen"
        reader.failure = .relocked
        await model.saveSessionTopic()
        XCTAssertEqual(model.outcome, .conflict(.sessionTopic, currentRevision: 5))
        XCTAssertEqual(model.refreshFailure, .locked)
        XCTAssertEqual(model.topicDraft, "kitchen")
        XCTAssertEqual(model.snapshot?.revision, 3, "the newer values were not loaded")
        XCTAssertFalse(model.canSaveTopic)
        XCTAssertTrue(model.canReload)
        let conflictMessage = ItemControlsView.message(
            for: model.outcome!, newerValuesShown: model.refreshFailure == nil)
        XCTAssertFalse(conflictMessage.contains("newer values are shown"))
        XCTAssertTrue(conflictMessage.contains("could not be loaded"))
        XCTAssertTrue(ItemControlsView.message(for: ItemControlsRefreshFailure.locked).contains("locked"))

        // A reload that still fails keeps everything; one that succeeds shows the newer values under the conflict.
        await model.reload()
        XCTAssertEqual(model.refreshFailure, .locked)
        XCTAssertEqual(model.topicDraft, "kitchen")
        reader.failure = nil
        reader.detail = detail(revision: 5, topic: "someone else's topic", text: "old")
        await model.reload()
        XCTAssertNil(model.refreshFailure)
        XCTAssertEqual(model.snapshot?.revision, 5)
        XCTAssertEqual(model.topicDraft, "kitchen", "the rejected draft is still kept after the reload")
        XCTAssertEqual(model.outcome, .conflict(.sessionTopic, currentRevision: 5))
        XCTAssertTrue(ItemControlsView.message(for: model.outcome!, newerValuesShown: true).contains("newer values are shown"))
        await model.saveSessionTopic()
        XCTAssertEqual(corrector.calls.last, .topic(command: "command-2", revision: 5, topic: "kitchen"))
    }

    func testAnItemGoneDuringTheRefreshOffersNoReload() async {
        let reader = StubDetailReader(detail())
        let model = make(reader, RecordingCorrector())
        await model.load()
        model.textDraft = "new words"
        reader.failure = .notFound
        await model.saveText()
        XCTAssertEqual(model.refreshFailure, .itemGone)
        XCTAssertFalse(model.canReload)
        XCTAssertFalse(model.canEdit)
    }

    func testEditingStaysPausedWhileTheRefreshAfterASubmitIsPending() async {
        for stale in [false, true] {
            let reader = StubDetailReader(detail(revision: 3, text: "old"))
            let corrector = RecordingCorrector()
            if stale { corrector.results = [.failure(.staleRevision(currentRevision: 5))] }
            let model = make(reader, corrector)
            await model.load()
            model.textDraft = "new words"
            var observed: (isWorking: Bool, canSaveText: Bool)?
            reader.duringRead = { [unowned model] in
                reader.duringRead = nil
                observed = (model.isWorking, model.canSaveText)
                // A second press while the refresh is pending sends nothing.
                await model.saveText()
            }
            reader.detail = detail(revision: stale ? 5 : 4, text: stale ? "someone else's words" : "new words")
            await model.saveText()
            XCTAssertEqual(observed?.isWorking, true, "stale=\(stale)")
            XCTAssertEqual(observed?.canSaveText, false, "stale=\(stale)")
            XCTAssertEqual(corrector.calls.count, 1, "stale=\(stale): only the first press was sent")
            XCTAssertFalse(model.isWorking)
        }
    }

    func testAnEditMadeWhileItsSaveIsInFlightIsNotReplacedByTheStoredValue() async {
        for field in [ItemControlsField.text, .type, .sessionTopic] {
            let reader = StubDetailReader(detail(revision: 3, type: "idea", topic: "garage", text: "old"))
            let corrector = RecordingCorrector()
            let model = make(reader, corrector)
            await model.load()
            reader.detail = detail(revision: 4, type: field == .type ? "note" : "idea",
                                   topic: field == .sessionTopic ? "kitchen" : "garage",
                                   text: field == .text ? "first words" : "old")
            var pausedDuringRead: Bool?
            reader.duringRead = { [unowned model] in
                reader.duringRead = nil
                pausedDuringRead = !model.canEdit
                // A change that reaches the draft while the save is answered, as typing would if it got through.
                switch field {
                case .text: model.textDraft = "second words"
                case .type: model.pendingType = .broadIntention
                case .sessionTopic: model.topicDraft = "attic"
                }
            }
            switch field {
            case .text:
                model.textDraft = "first words"
                await model.saveText()
                XCTAssertEqual(model.textDraft, "second words")
            case .type:
                model.pendingType = .note
                await model.applyType()
                XCTAssertEqual(model.pendingType, .broadIntention)
            case .sessionTopic:
                model.topicDraft = "kitchen"
                await model.saveSessionTopic()
                XCTAssertEqual(model.topicDraft, "attic")
            }
            XCTAssertEqual(pausedDuringRead, true, "\(field): the controls are disabled while the save is answered")
            XCTAssertEqual(model.outcome, .saved(field))
            XCTAssertEqual(model.snapshot?.revision, 4)
            XCTAssertEqual(corrector.calls.count, 1, "\(field): only the first value was sent")
        }
    }

    func testAnEditMadeBeforeAFailedRefreshSurvivesTheReload() async {
        let reader = StubDetailReader(detail(revision: 3, text: "old"))
        let corrector = RecordingCorrector()
        let model = make(reader, corrector)
        await model.load()
        reader.duringRead = { [unowned model] in
            reader.duringRead = nil
            model.textDraft = "second words"
        }
        reader.failure = .relocked
        model.textDraft = "first words"
        await model.saveText()
        XCTAssertEqual(model.refreshFailure, .locked)
        reader.failure = nil
        reader.detail = detail(revision: 4, text: "first words")
        await model.reload()
        XCTAssertNil(model.refreshFailure)
        XCTAssertEqual(model.textDraft, "second words", "the later edit is kept after the reload")
    }

    func testAnUnchangedDraftIsStillReplacedByTheStoredValueAfterASave() async {
        let reader = StubDetailReader(detail(revision: 3, text: "old"))
        let model = make(reader, RecordingCorrector())
        await model.load()
        reader.detail = detail(revision: 4, text: "first words (stored)")
        model.textDraft = "first words"
        await model.saveText()
        XCTAssertEqual(model.textDraft, "first words (stored)")
        XCTAssertFalse(model.canSaveText)
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
