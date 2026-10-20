import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Lets a test hold a fake's call open until it chooses to let it finish.
final class DeletionCallGate: @unchecked Sendable {
    private let lock = NSLock()
    private var waiting: CheckedContinuation<Void, Never>?
    private var isOpen = false

    func wait() async {
        await withCheckedContinuation { continuation in
            lock.lock()
            if isOpen {
                lock.unlock()
                continuation.resume()
            } else {
                waiting = continuation
                lock.unlock()
            }
        }
    }

    func open() {
        lock.lock()
        isOpen = true
        let continuation = waiting
        waiting = nil
        lock.unlock()
        continuation?.resume()
    }
}

/// The detail read of one synthetic item, answering with a revision and a lifecycle state.
final class DeletionFakeItemReader: SourceDetailReading, @unchecked Sendable {
    var revision = 4
    var lifecycleState = "active"
    var failure: Error?
    private(set) var reads = 0

    func detail(itemID: String) async throws -> CoreItemDetail {
        reads += 1
        if let failure { throw failure }
        return CoreItemDetail(
            itemID: itemID, captureID: "capture-1", revision: revision, itemType: "task", scope: "personal",
            sessionTopic: nil, lifecycleState: lifecycleState, capturedAt: "2026-10-01T09:30:00Z", text: nil,
            textBasis: .original, originalText: nil, correctedText: nil, audioReference: nil, transcript: nil,
            history: [], historyTruncated: false)
    }
}

final class FakeItemDeletion: ItemDeletionControlling, @unchecked Sendable {
    var deleteResults: [Result<ItemDeletionReceipt, Error>] = []
    var progressResults: [Result<ItemDeletionProgress, Error>] = []
    /// What a progress read answers once `progressResults` is used up.
    var progressFallback: ItemDeletionProgress = .notRequested
    /// When set, every progress read fails with it.
    var progressFailure: Error?
    var deleteGate: DeletionCallGate?
    private(set) var deleteCalls: [(itemID: String, revision: Int)] = []
    private(set) var progressCalls = 0

    func delete(itemID: String, expectedRevision: Int) async throws -> ItemDeletionReceipt {
        deleteCalls.append((itemID, expectedRevision))
        await deleteGate?.wait()
        return try deleteResults.removeFirst().get()
    }

    func progress(itemID: String) async throws -> ItemDeletionProgress {
        progressCalls += 1
        if let progressFailure { throw progressFailure }
        if progressResults.isEmpty { return progressFallback }
        return try progressResults.removeFirst().get()
    }
}

final class FakeReset: ResetControlling, @unchecked Sendable {
    var resetResults: [Result<ResetOutcome, Error>] = []
    var finishResults: [Result<ResetOutcome, Error>] = []
    var progressResults: [Result<ResetProgress?, Error>] = []
    var nativeWorkOwed = false
    private(set) var choices: [ResetCredentialChoice] = []
    private(set) var finishCalls = 0

    func reset(credentialChoice: ResetCredentialChoice) async throws -> ResetOutcome {
        choices.append(credentialChoice)
        return try resetResults.removeFirst().get()
    }

    func progress() async throws -> ResetProgress? {
        if progressResults.isEmpty { return nil }
        return try progressResults.removeFirst().get()
    }

    func hasUnfinishedNativeWork() async throws -> Bool { nativeWorkOwed }

    func finishPending() async throws -> ResetOutcome {
        finishCalls += 1
        return try finishResults.removeFirst().get()
    }
}

private func receipt(_ progress: ItemDeletionProgress, settled: Bool) -> ItemDeletionReceipt {
    ItemDeletionReceipt(itemID: "item-1", deletionRevision: 4, replayed: false, progress: progress, cleanupSettled: settled)
}

@MainActor
final class ItemDeletionModelTests: XCTestCase {
    private var scope: CompositionReadScope = .authenticated

    private let reader = DeletionFakeItemReader()

    private func makeModel(_ controls: FakeItemDeletion) -> ItemDeletionModel {
        ItemDeletionModel(itemID: "item-1", controls: controls, reader: reader, readScope: { [unowned self] in scope })
    }

    /// A model that has read the item, as the screen does when it opens.
    private func preparedModel(_ controls: FakeItemDeletion) async -> ItemDeletionModel {
        let model = makeModel(controls)
        await model.prepare()
        return model
    }

    func testNothingIsDeletedUntilTheUserConfirmsTheRevisionTheyAreShown() async {
        let controls = FakeItemDeletion()
        let model = await preparedModel(controls)
        XCTAssertEqual(model.revision, 4)

        await model.confirmDeletion()
        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(controls.deleteCalls.isEmpty)

        model.requestDeletion()
        XCTAssertEqual(model.phase, .confirming)
        model.cancelConfirmation()
        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(controls.deleteCalls.isEmpty)
    }

    func testAnItemThatCannotBeReadOrIsAlreadyDeletedOffersNoDeletion() async {
        let controls = FakeItemDeletion()
        let model = makeModel(controls)
        reader.failure = ItemDetailError.notFound
        await model.prepare()
        XCTAssertNil(model.revision)

        reader.failure = nil
        reader.lifecycleState = "deleted"
        await model.prepare()
        XCTAssertNil(model.revision)

        model.requestDeletion()
        await model.confirmDeletion()
        XCTAssertTrue(controls.deleteCalls.isEmpty, "no revision was shown, so nothing is deleted")
    }

    func testSettledCleanupIsComplete() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = await preparedModel(controls)

        model.requestDeletion()
        await model.confirmDeletion()

        XCTAssertEqual(model.phase, .complete)
        XCTAssertEqual(controls.deleteCalls.first?.revision, 4)
        XCTAssertTrue(model.isRecorded)
    }

    func testOutstandingCleanupStaysPendingAndRetryReplaysTheSameRevision() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [
            .success(receipt(.inProgress(pending: [.removeAudio], failed: []), settled: false)),
            .success(receipt(.complete, settled: true)),
        ]
        let model = await preparedModel(controls)

        model.requestDeletion()
        await model.confirmDeletion()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("the recording file"))
        XCTAssertTrue(model.canRetry)
        XCTAssertFalse(model.canCheckProgress)
        XCTAssertNotEqual(model.phase, .complete)
        XCTAssertTrue(model.isRecorded, "the item is deleted and hidden while cleanup is pending")

        reader.revision = 9
        await model.prepare()
        await model.retry()

        XCTAssertEqual(model.phase, .complete)
        XCTAssertEqual(controls.deleteCalls.map(\.revision), [4, 4], "a retry replays the recorded request")
    }

    func testTheDeletionIsReportedAsRecordedBeforeItsCleanupFinishes() async throws {
        let controls = FakeItemDeletion()
        controls.deleteGate = DeletionCallGate()
        controls.progressFallback = .inProgress(pending: [.removeAudio], failed: [])
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = await preparedModel(controls)
        model.requestDeletion()

        let deleting = Task { await model.confirmDeletion() }
        let deadline = Date().addingTimeInterval(5)
        while !model.isRecorded && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }

        XCTAssertTrue(model.isRecorded, "the host is told as soon as the core shows the intent")
        XCTAssertEqual(model.phase, .deleting, "cleanup is still running")
        controls.deleteGate?.open()
        await deleting.value
        XCTAssertEqual(model.phase, .complete)
    }

    func testTheDeletionIsReportedAsRecordedWhenOnlyTheItemReadShowsTheTombstone() async throws {
        let controls = FakeItemDeletion()
        controls.deleteGate = DeletionCallGate()
        controls.progressFailure = ItemDeletionError.unreadable
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = await preparedModel(controls)
        model.requestDeletion()

        let deleting = Task { await model.confirmDeletion() }
        reader.lifecycleState = "deleted"
        let deadline = Date().addingTimeInterval(5)
        while !model.isRecorded && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }

        XCTAssertTrue(model.isRecorded, "the source is dropped even when progress cannot be read")
        XCTAssertEqual(model.phase, .deleting)
        controls.deleteGate?.open()
        await deleting.value
    }

    func testTheDeletionIsReportedAsRecordedWhenTheItemReadAnswersNotFoundAsTheProductionReadDoes() async throws {
        let controls = FakeItemDeletion()
        controls.deleteGate = DeletionCallGate()
        controls.progressFailure = ItemDeletionError.unreadable
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = await preparedModel(controls)
        model.requestDeletion()

        let deleting = Task { await model.confirmDeletion() }
        reader.failure = ItemDetailError.notFound
        let deadline = Date().addingTimeInterval(5)
        while !model.isRecorded && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }

        XCTAssertTrue(model.isRecorded, "a tombstoned item reads as not found, which means the intent is recorded")
        XCTAssertEqual(model.phase, .deleting, "cleanup is still running")
        controls.deleteGate?.open()
        await deleting.value
        XCTAssertEqual(model.phase, .complete)
    }

    func testAFailedItemReadIsNotTakenAsARecordedDeletion() async throws {
        let controls = FakeItemDeletion()
        controls.deleteGate = DeletionCallGate()
        controls.progressFailure = ItemDeletionError.unreadable
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = await preparedModel(controls)
        model.requestDeletion()

        let deleting = Task { await model.confirmDeletion() }
        reader.failure = ItemDetailError.relocked
        try await Task.sleep(nanoseconds: 4 * ItemDeletionModel.recordedPollNanoseconds)

        XCTAssertFalse(model.isRecorded, "only a tombstone or not found answer shows the intent")
        controls.deleteGate?.open()
        await deleting.value
        XCTAssertTrue(model.isRecorded, "the delete call's own answer still reports it")
    }

    func testALiveItemWhoseReadFailedShowsNoDeletionWhenProgressIsUnreadable() async {
        let controls = FakeItemDeletion()
        controls.progressFailure = ItemDeletionError.unreadable
        reader.failure = ItemDetailError.core(.handleClosed)
        let model = await preparedModel(controls)

        await model.refreshProgress()

        XCTAssertEqual(model.phase, .idle, "no deletion is known to exist, so none is implied")
        XCTAssertFalse(model.canCheckProgress)
    }

    func testAnUnreadableProgressForAnUnreadableItemIsShownAndCanBeChecked() async {
        let controls = FakeItemDeletion()
        controls.progressFailure = ItemDeletionError.unreadable
        reader.failure = ItemDetailError.notFound
        let model = await preparedModel(controls)

        await model.refreshProgress()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("could not be read"))
        XCTAssertTrue(model.canCheckProgress)
    }

    func testTerminallyFailedCleanupIsShownAndNotOfferedAsRetry() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(receipt(.failed([.cancelNotifications]), settled: false))]
        let model = await preparedModel(controls)

        model.requestDeletion()
        await model.confirmDeletion()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("will not be retried"))
        XCTAssertFalse(model.canRetry)
        XCTAssertFalse(model.canCheckProgress)
    }

    func testRefusedDeletionIsAnErrorNeverPendingOrCompleteAndAdoptsTheCurrentRevision() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.failure(ItemDeletionError.staleRevision(currentRevision: 6))]
        let model = await preparedModel(controls)

        model.requestDeletion()
        await model.confirmDeletion()

        XCTAssertEqual(model.phase, .failed(DeletionText.staleText))
        XCTAssertFalse(model.canRetry)
        XCTAssertFalse(model.isRecorded)
        XCTAssertEqual(model.revision, 6, "the next confirmation is against the revision the core reported")
    }

    func testARestartShowsTheActualProgressOfAnEarlierDeletionAndOffersACheckNotARetry() async {
        let controls = FakeItemDeletion()
        controls.progressResults = [
            .success(.inProgress(pending: [.clearIngress], failed: [])), .success(.complete),
        ]
        let model = makeModel(controls)

        await model.refreshProgress()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("an unsent copy of the capture"))
        XCTAssertTrue(model.isRecorded, "a reopened screen is told the item is deleted")
        XCTAssertFalse(model.canRetry, "the revision of the earlier request is not known, so no replay is offered")
        XCTAssertTrue(model.canCheckProgress)

        await model.retry()
        XCTAssertTrue(controls.deleteCalls.isEmpty, "a retry that cannot run asks nothing")

        await model.refreshProgress()
        XCTAssertEqual(model.phase, .complete, "checking again shows the finished cleanup")
        XCTAssertFalse(model.canCheckProgress)
    }

    func testAnItemNeverDeletedStaysIdleAfterAProgressRead() async {
        let controls = FakeItemDeletion()
        controls.progressResults = [.success(.notRequested)]
        let model = makeModel(controls)

        await model.refreshProgress()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertFalse(model.isRecorded)
    }

    func testLockedSessionAsksNothingAndShowsLocked() async {
        let controls = FakeItemDeletion()
        scope = .captureOnly
        let model = makeModel(controls)

        model.requestDeletion()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(controls.deleteCalls.isEmpty)
    }

    func testARecordedDeletionKeepsItsProgressAcrossARelock() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(receipt(.inProgress(pending: [.removeAudio], failed: []), settled: false))]
        let model = await preparedModel(controls)
        model.requestDeletion()
        await model.confirmDeletion()

        scope = .captureOnly
        await model.refreshProgress()

        XCTAssertTrue(model.phase.isPending, "the recorded deletion is not forgotten")
        XCTAssertEqual(controls.progressCalls, 0)
    }
}

@MainActor
final class ResetSettingsModelTests: XCTestCase {
    private var scope: CompositionReadScope = .authenticated

    private func makeModel(_ controls: FakeReset) -> ResetSettingsModel {
        ResetSettingsModel(controls: controls, readScope: { [unowned self] in scope })
    }

    private static func progress(phase: ResetPhase, pending: Int = 0, failed: Int = 0) -> ResetProgress {
        ResetProgress(
            generation: 1, phase: phase, boundaryRowID: 3, itemsAwaitingTombstone: 0, cleanupPending: pending,
            cleanupFailed: failed)
    }

    private static func outcome(_ state: ResetState, blockers: [ResetBlocker] = [], progress: ResetProgress? = nil) -> ResetOutcome {
        ResetOutcome(state: state, progress: progress, blockers: blockers)
    }

    func testTheCredentialChoiceIsExplicitAndAResetNeedsAConfirmation() async {
        let controls = FakeReset()
        let model = makeModel(controls)

        XCTAssertNil(model.credentialChoice, "no choice is preselected")
        XCTAssertFalse(model.canContinue)
        model.beginConfirmation()
        XCTAssertEqual(model.phase, .idle)

        model.credentialChoice = .remove
        XCTAssertTrue(model.canContinue)
        await model.confirmReset()
        XCTAssertEqual(model.phase, .idle, "confirming is a separate step")
        XCTAssertTrue(controls.choices.isEmpty)
    }

    func testTheChosenCredentialOptionReachesTheService() async {
        for choice in [ResetCredentialChoice.keep, .remove] {
            let controls = FakeReset()
            controls.resetResults = [.success(Self.outcome(.completed, progress: Self.progress(phase: .completed)))]
            let model = makeModel(controls)
            model.credentialChoice = choice

            model.beginConfirmation()
            await model.confirmReset()

            XCTAssertEqual(controls.choices, [choice])
            XCTAssertEqual(model.phase, .completed)
        }
    }

    func testAnIncompleteResetIsPendingNotCompletedAndRetryFinishesIt() async {
        let controls = FakeReset()
        controls.resetResults = [
            .success(Self.outcome(
                .pending, blockers: [.itemCleanupIncomplete], progress: Self.progress(phase: .pending, pending: 2))),
        ]
        controls.finishResults = [.success(Self.outcome(.completed, progress: Self.progress(phase: .completed)))]
        let model = makeModel(controls)
        model.credentialChoice = .keep
        model.beginConfirmation()
        await model.confirmReset()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("still have recordings, files or reminders"))
        XCTAssertTrue(text.contains("2 cleanup steps pending"))
        XCTAssertTrue(model.canRetry)

        await model.retry()

        XCTAssertEqual(model.phase, .completed)
        XCTAssertEqual(controls.choices, [.keep], "a retry starts no further reset")
        XCTAssertEqual(controls.finishCalls, 1)
    }

    func testARelockDuringARetryKeepsThePendingResetVisible() async {
        let controls = FakeReset()
        controls.resetResults = [
            .success(Self.outcome(
                .pending, blockers: [.itemCleanupIncomplete], progress: Self.progress(phase: .pending, pending: 2))),
        ]
        controls.finishResults = [.failure(ResetControlError.notAuthenticated)]
        let model = makeModel(controls)
        model.credentialChoice = .keep
        model.beginConfirmation()
        await model.confirmReset()
        let pending = model.phase

        await model.retry()

        XCTAssertEqual(model.phase, pending, "the started reset is not hidden behind a locked note")
        XCTAssertTrue(model.canRetry)
        XCTAssertEqual(controls.finishCalls, 1)
    }

    func testARetryNeverTakesANewCredentialChoice() async {
        let controls = FakeReset()
        controls.resetResults = [
            .success(Self.outcome(.pending, blockers: [.credentialRemovalIncomplete])),
        ]
        controls.finishResults = [.success(Self.outcome(.completed, progress: Self.progress(phase: .completed)))]
        let model = makeModel(controls)
        model.credentialChoice = .remove
        model.beginConfirmation()
        await model.confirmReset()
        XCTAssertTrue(model.phase.isPending)

        model.credentialChoice = .keep
        await model.retry()

        XCTAssertEqual(model.phase, .completed)
        XCTAssertEqual(controls.choices, [.remove], "only the original choice was ever sent")
        XCTAssertEqual(controls.finishCalls, 1)
    }

    func testTheCredentialChoiceClosesOnceAResetHasStarted() async {
        let controls = FakeReset()
        controls.resetResults = [.success(Self.outcome(.pending, blockers: [.credentialRemovalIncomplete]))]
        let model = makeModel(controls)
        XCTAssertTrue(model.isChoiceOpen)
        model.choose(.remove)
        XCTAssertEqual(model.credentialChoice, .remove)
        model.beginConfirmation()
        XCTAssertTrue(model.isChoiceOpen, "the choice is still shown, disabled, while confirming")
        model.choose(.keep)
        XCTAssertEqual(model.credentialChoice, .remove, "confirming does not change the choice")
        await model.confirmReset()

        XCTAssertTrue(model.phase.isPending)
        XCTAssertFalse(model.isChoiceOpen)
        model.choose(.keep)
        XCTAssertEqual(model.credentialChoice, .remove, "a started reset takes no new choice")
    }

    func testAResetThatStartedNothingIsAnErrorWithItsReason() async {
        let controls = FakeReset()
        controls.resetResults = [.success(Self.outcome(.notStarted, blockers: [.credentialsUnreadable]))]
        let model = makeModel(controls)
        model.credentialChoice = .remove
        model.beginConfirmation()
        await model.confirmReset()

        guard case .failed(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("did not start"))
    }

    func testARestartShowsAnInterruptedResetAsPending() async {
        let controls = FakeReset()
        controls.progressResults = [.success(Self.progress(phase: .pending, pending: 1))]
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertTrue(model.phase.isPending)
        XCTAssertTrue(model.canRetry, "a retry finishes the reset under its original choice")
    }

    func testAnUnreadableResetProgressIsNotShownAsAFreshStart() async {
        let controls = FakeReset()
        controls.progressResults = [.failure(CocoaError(.fileReadUnknown))]
        let model = makeModel(controls)

        await model.refresh()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("could not be read"))
        XCTAssertTrue(model.canRetry)
    }

    func testARestartShowsNativeCleanupThatTheCoreDoesNotReport() async {
        let controls = FakeReset()
        controls.progressResults = [.success(Self.progress(phase: .completed))]
        controls.nativeWorkOwed = true
        controls.finishResults = [.success(Self.outcome(.completed, progress: Self.progress(phase: .completed)))]
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertEqual(model.phase, .pending(DeletionText.nativeWorkOwedText))
        XCTAssertTrue(model.canRetry)

        await model.retry()
        XCTAssertEqual(model.phase, .completed)
    }

    func testARecordedTapTheCoreNeverStartedIsShownAsPendingAfterARestart() async {
        let controls = FakeReset()
        controls.progressResults = [.success(nil)]
        controls.nativeWorkOwed = true
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertEqual(model.phase, .pending(DeletionText.nativeWorkOwedText))
    }

    func testNoResetEverStartedLeavesTheSectionIdle() async {
        let controls = FakeReset()
        controls.progressResults = [.success(nil)]
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertEqual(model.phase, .idle)
    }

    func testLockedSessionRefusesToStartAndAsksNothing() async {
        let controls = FakeReset()
        let model = makeModel(controls)
        model.credentialChoice = .keep
        scope = .captureOnly

        model.beginConfirmation()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(controls.choices.isEmpty)
    }

    func testTheExplanationNeverPromisesRecallOfExternalCopies() {
        for text in [DeletionText.resetLimitsNote, DeletionText.itemLimitsNote] {
            XCTAssertTrue(text.contains("cannot recall"))
            XCTAssertTrue(text.contains("backups"))
        }
        XCTAssertFalse(DeletionText.completedText.lowercased().contains("backup"))
        XCTAssertTrue(DeletionText.resetCaptureNote.contains("new content"))
    }
}
