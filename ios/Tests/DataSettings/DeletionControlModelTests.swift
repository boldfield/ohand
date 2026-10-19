import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class FakeItemDeletion: ItemDeletionControlling, @unchecked Sendable {
    var deleteResults: [Result<ItemDeletionReceipt, Error>] = []
    var progressResults: [Result<ItemDeletionProgress, Error>] = []
    private(set) var deleteCalls: [(itemID: String, revision: Int)] = []
    private(set) var progressCalls = 0

    func delete(itemID: String, expectedRevision: Int) async throws -> ItemDeletionReceipt {
        deleteCalls.append((itemID, expectedRevision))
        return try deleteResults.removeFirst().get()
    }

    func progress(itemID: String) async throws -> ItemDeletionProgress {
        progressCalls += 1
        return try progressResults.removeFirst().get()
    }
}

private final class FakeReset: ResetControlling, @unchecked Sendable {
    var resetResults: [Result<ResetOutcome, Error>] = []
    var progressResults: [Result<ResetProgress?, Error>] = []
    private(set) var choices: [ResetCredentialChoice] = []

    func reset(credentialChoice: ResetCredentialChoice) async throws -> ResetOutcome {
        choices.append(credentialChoice)
        return try resetResults.removeFirst().get()
    }

    func progress() async throws -> ResetProgress? {
        try progressResults.removeFirst().get()
    }
}

private func receipt(_ progress: ItemDeletionProgress, settled: Bool) -> ItemDeletionReceipt {
    ItemDeletionReceipt(itemID: "item-1", deletionRevision: 4, replayed: false, progress: progress, cleanupSettled: settled)
}

@MainActor
final class ItemDeletionModelTests: XCTestCase {
    private var scope: CompositionReadScope = .authenticated

    private func makeModel(_ controls: FakeItemDeletion) -> ItemDeletionModel {
        ItemDeletionModel(itemID: "item-1", controls: controls, readScope: { [unowned self] in scope })
    }

    func testNothingIsDeletedUntilTheUserConfirmsTheRevisionTheySaw() async {
        let controls = FakeItemDeletion()
        let model = makeModel(controls)

        await model.confirmDeletion(revision: 4)
        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(controls.deleteCalls.isEmpty)

        model.requestDeletion()
        XCTAssertEqual(model.phase, .confirming)
        model.cancelConfirmation()
        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(controls.deleteCalls.isEmpty)
    }

    func testSettledCleanupIsComplete() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(receipt(.complete, settled: true))]
        let model = makeModel(controls)

        model.requestDeletion()
        await model.confirmDeletion(revision: 4)

        XCTAssertEqual(model.phase, .complete)
        XCTAssertEqual(controls.deleteCalls.first?.revision, 4)
    }

    func testOutstandingCleanupStaysPendingAndRetryReplaysTheSameRevision() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [
            .success(receipt(.inProgress(pending: [.removeAudio], failed: []), settled: false)),
            .success(receipt(.complete, settled: true)),
        ]
        let model = makeModel(controls)

        model.requestDeletion()
        await model.confirmDeletion(revision: 4)

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("the recording file"))
        XCTAssertTrue(model.canRetry)
        XCTAssertNotEqual(model.phase, .complete)

        await model.retry()

        XCTAssertEqual(model.phase, .complete)
        XCTAssertEqual(controls.deleteCalls.map(\.revision), [4, 4], "a retry replays the recorded request")
    }

    func testTerminallyFailedCleanupIsShownAndNotOfferedAsRetry() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(receipt(.failed([.cancelNotifications]), settled: false))]
        let model = makeModel(controls)

        model.requestDeletion()
        await model.confirmDeletion(revision: 4)

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("will not be retried"))
        XCTAssertFalse(model.canRetry)
    }

    func testRefusedDeletionIsAnErrorNeverPendingOrComplete() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.failure(ItemDeletionError.staleRevision(currentRevision: 6))]
        let model = makeModel(controls)

        model.requestDeletion()
        await model.confirmDeletion(revision: 4)

        XCTAssertEqual(model.phase, .failed(DeletionText.staleText))
        XCTAssertFalse(model.canRetry)
    }

    func testARestartShowsTheActualProgressOfAnEarlierDeletion() async {
        let controls = FakeItemDeletion()
        controls.progressResults = [.success(.inProgress(pending: [.clearIngress], failed: []))]
        let model = makeModel(controls)

        await model.refreshProgress()

        guard case .pending(let text) = model.phase else { return XCTFail("got \(model.phase)") }
        XCTAssertTrue(text.contains("an unsent copy of the capture"))
    }

    func testAnItemNeverDeletedStaysIdleAfterAProgressRead() async {
        let controls = FakeItemDeletion()
        controls.progressResults = [.success(.notRequested)]
        let model = makeModel(controls)

        await model.refreshProgress()

        XCTAssertEqual(model.phase, .idle)
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
        let model = makeModel(controls)
        model.requestDeletion()
        await model.confirmDeletion(revision: 4)

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
            .success(Self.outcome(.completed, progress: Self.progress(phase: .completed))),
        ]
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
        XCTAssertEqual(controls.choices, [.keep, .keep])
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
        XCTAssertFalse(model.canRetry, "the user states the credential choice again before retrying")
        model.credentialChoice = .keep
        XCTAssertTrue(model.canRetry)
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
