import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real delete controls in a window over the real models and reads what each phase puts on screen. The tests
/// drive the model calls the buttons make, so what is checked here is the words and controls a person would see.
@MainActor
final class DeletionViewTests: XCTestCase {
    private static let phone = CGSize(width: 390, height: 844)
    private static let minimumTouchTarget: CGFloat = 44

    private var scope: CompositionReadScope = .authenticated
    private var rendered: [String: RetentionRenderedElement] = [:]
    private var window: UIWindow?

    private func show<Content: View>(_ content: Content) async {
        rendered = [:]
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = UIHostingController(rootView: content)
        shown.makeKeyAndVisible()
        window = shown
        await settle()
    }

    override func tearDown() {
        window?.isHidden = true
        window = nil
        super.tearDown()
    }

    private func settle() async {
        for _ in 0..<3 {
            window?.rootViewController?.view.setNeedsLayout()
            window?.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        window?.rootViewController?.view.layoutIfNeeded()
    }

    private func text(_ identifier: String) -> String? { rendered[identifier]?.text }

    private func assertTappable(
        _ identifier: String, enabled: Bool = true, file: StaticString = #filePath, line: UInt = #line
    ) {
        guard let element = rendered[identifier] else {
            return XCTFail("\(identifier) is shown", file: file, line: line)
        }
        XCTAssertGreaterThanOrEqual(element.frame.height, Self.minimumTouchTarget - 0.5, identifier, file: file, line: line)
        XCTAssertEqual(element.isEnabled, enabled, "\(identifier) enabled", file: file, line: line)
    }

    // MARK: item deletion

    private func itemReceipt(_ progress: ItemDeletionProgress, settled: Bool) -> ItemDeletionReceipt {
        ItemDeletionReceipt(itemID: "item-1", deletionRevision: 4, replayed: false, progress: progress, cleanupSettled: settled)
    }

    private func withItemSection(
        _ controls: FakeItemDeletion, reader: DeletionFakeItemReader = DeletionFakeItemReader(),
        onRecorded: @escaping () -> Void = {}, _ body: (ItemDeletionModel) async -> Void
    ) async {
        let model = ItemDeletionModel(
            itemID: "item-1", controls: controls, reader: reader, readScope: { [unowned self] in scope })
        await show(
            ItemDeletionSection(
                model: model, onRecorded: onRecorded, renderReporter: { [unowned self] in rendered = $0 }))
        await body(model)
    }

    func testTheDeleteButtonIsOfferedOnlyForAnItemTheControlHasRead() async {
        let reader = DeletionFakeItemReader()
        reader.failure = ItemDetailError.notFound
        await withItemSection(FakeItemDeletion(), reader: reader) { _ in
            XCTAssertNil(rendered[ItemDeletionSection.deleteIdentifier], "an unreadable item offers no deletion")
        }
        await withItemSection(FakeItemDeletion()) { _ in
            XCTAssertEqual(text(ItemDeletionSection.deleteIdentifier), DeletionText.itemHeading)
            assertTappable(ItemDeletionSection.deleteIdentifier)
        }
    }

    func testConfirmingExplainsTheLimitsAndOffersAnExplicitConfirmAndCancel() async {
        await withItemSection(FakeItemDeletion()) { model in
            model.requestDeletion()
            await settle()
            XCTAssertEqual(text(ItemDeletionSection.explanationIdentifier), DeletionText.itemExplanation)
            XCTAssertEqual(text(ItemDeletionSection.limitsIdentifier), DeletionText.itemLimitsNote)
            XCTAssertTrue(DeletionText.itemLimitsNote.contains("cannot recall"))
            assertTappable(ItemDeletionSection.confirmIdentifier)
            assertTappable(ItemDeletionSection.cancelIdentifier)
            XCTAssertNil(rendered[ItemDeletionSection.deleteIdentifier])
        }
    }

    func testPendingCleanupShowsWhatIsOutstandingAndARetryAndNeverACompletedMessage() async {
        let controls = FakeItemDeletion()
        controls.deleteResults = [.success(itemReceipt(.inProgress(pending: [.removeAudio], failed: []), settled: false))]
        var recorded = 0
        await withItemSection(controls, onRecorded: { recorded += 1 }) { model in
            model.requestDeletion()
            await model.confirmDeletion()
            await settle()
            XCTAssertTrue(text(ItemDeletionSection.stateIdentifier)?.contains("the recording file") == true)
            XCTAssertNotEqual(text(ItemDeletionSection.stateIdentifier), DeletionText.completeText)
            assertTappable(ItemDeletionSection.retryIdentifier)
            XCTAssertNil(rendered[ItemDeletionSection.checkProgressIdentifier])
            XCTAssertNil(rendered[ItemDeletionSection.deleteIdentifier], "a deleted item cannot be deleted again")
            XCTAssertGreaterThan(recorded, 0, "the host was told to read the item again")
        }
    }

    func testAfterARestartTheControlShowsProgressAndACheckButtonNotARetry() async {
        let controls = FakeItemDeletion()
        controls.progressFallback = .inProgress(pending: [.cancelNotifications], failed: [])
        let reader = DeletionFakeItemReader()
        reader.lifecycleState = "deleted"
        var recorded = 0
        await withItemSection(controls, reader: reader, onRecorded: { recorded += 1 }) { _ in
            XCTAssertTrue(text(ItemDeletionSection.stateIdentifier)?.contains("its reminder notifications") == true)
            assertTappable(ItemDeletionSection.checkProgressIdentifier)
            XCTAssertNil(rendered[ItemDeletionSection.retryIdentifier])
            XCTAssertGreaterThan(recorded, 0, "a reopened screen reads the deleted item and shows nothing of it")
        }
    }

    func testACompleteDeletionAndARefusedOneAreShownDifferently() async {
        let done = FakeItemDeletion()
        done.deleteResults = [.success(itemReceipt(.complete, settled: true))]
        await withItemSection(done) { model in
            model.requestDeletion()
            await model.confirmDeletion()
            await settle()
            XCTAssertEqual(text(ItemDeletionSection.stateIdentifier), DeletionText.completeText)
            XCTAssertNil(rendered[ItemDeletionSection.retryIdentifier])
        }
        let refused = FakeItemDeletion()
        refused.deleteResults = [.failure(ItemDeletionError.staleRevision(currentRevision: 6))]
        await withItemSection(refused) { model in
            model.requestDeletion()
            await model.confirmDeletion()
            await settle()
            XCTAssertEqual(text(ItemDeletionSection.stateIdentifier), DeletionText.staleText)
            assertTappable(ItemDeletionSection.deleteIdentifier)
        }
    }

    func testALockedSessionShowsOnlyTheLockedNote() async {
        scope = .captureOnly
        await withItemSection(FakeItemDeletion()) { model in
            model.requestDeletion()
            await settle()
            XCTAssertEqual(text(ItemDeletionSection.stateIdentifier), DeletionText.lockedText)
            XCTAssertNil(rendered[ItemDeletionSection.confirmIdentifier])
            XCTAssertNil(rendered[ItemDeletionSection.deleteIdentifier])
        }
    }

    // MARK: reset

    private static func resetProgress(_ phase: ResetPhase) -> ResetProgress {
        ResetProgress(
            generation: 1, phase: phase, boundaryRowID: 3, itemsAwaitingTombstone: 0, cleanupPending: 1, cleanupFailed: 0)
    }

    private func withResetScreen(_ controls: FakeReset, _ body: (ResetSettingsModel) async -> Void) async {
        let model = ResetSettingsModel(controls: controls, readScope: { [unowned self] in scope })
        await show(
            NavigationStack {
                ResetSettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
            })
        await body(model)
    }

    func testTheResetScreenExplainsLimitsAndRequiresAnExplicitCredentialChoice() async {
        await withResetScreen(FakeReset()) { model in
            XCTAssertEqual(text(ResetSettingsView.explanationIdentifier), DeletionText.resetExplanation)
            XCTAssertEqual(text(ResetSettingsView.limitsIdentifier), DeletionText.resetLimitsNote)
            XCTAssertEqual(text(ResetSettingsView.captureNoteIdentifier), DeletionText.resetCaptureNote)
            XCTAssertEqual(text(ResetSettingsView.choiceRequiredIdentifier), DeletionText.choiceRequiredNote)
            assertTappable(ResetSettingsView.keepIdentifier)
            assertTappable(ResetSettingsView.removeIdentifier)
            assertTappable(ResetSettingsView.continueIdentifier, enabled: false)

            model.credentialChoice = .remove
            await settle()
            XCTAssertNil(rendered[ResetSettingsView.choiceRequiredIdentifier])
            assertTappable(ResetSettingsView.continueIdentifier)

            model.beginConfirmation()
            await settle()
            assertTappable(ResetSettingsView.confirmIdentifier)
            assertTappable(ResetSettingsView.cancelIdentifier)
            assertTappable(ResetSettingsView.removeIdentifier, enabled: false)
        }
    }

    func testAPendingResetShowsWhatIsOutstandingAndATryAgainButton() async {
        let controls = FakeReset()
        controls.resetResults = [
            .success(ResetOutcome(
                state: .pending, progress: Self.resetProgress(.pending), blockers: [.credentialRemovalIncomplete])),
        ]
        await withResetScreen(controls) { model in
            model.credentialChoice = .remove
            model.beginConfirmation()
            await model.confirmReset()
            await settle()
            let state = text(ResetSettingsView.stateIdentifier)
            XCTAssertTrue(state?.contains("Some provider keys have not been removed yet.") == true)
            XCTAssertNotEqual(state, DeletionText.completedText)
            assertTappable(ResetSettingsView.retryIdentifier)
        }
    }

    func testARestartedScreenShowsNativeCleanupTheCoreDoesNotReport() async {
        let controls = FakeReset()
        controls.progressResults = [.success(Self.resetProgress(.completed))]
        controls.nativeWorkOwed = true
        await withResetScreen(controls) { _ in
            XCTAssertEqual(text(ResetSettingsView.stateIdentifier), DeletionText.nativeWorkOwedText)
            assertTappable(ResetSettingsView.retryIdentifier)
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier], "a restarted pending reset offers no new choice")
            XCTAssertNil(rendered[ResetSettingsView.removeIdentifier])
            XCTAssertNil(rendered[ResetSettingsView.choiceRequiredIdentifier])
            XCTAssertEqual(text(ResetSettingsView.choiceFixedIdentifier), DeletionText.choiceFixedNote)
        }
    }

    func testAPendingResetNeverShowsACredentialChoiceItsRetryWouldNotCarryOut() async {
        let controls = FakeReset()
        let pending = ResetOutcome(
            state: .pending, progress: Self.resetProgress(.pending), blockers: [.credentialRemovalIncomplete])
        controls.resetResults = [.success(pending)]
        controls.finishResults = [.success(pending)]
        await withResetScreen(controls) { model in
            model.choose(.remove)
            model.beginConfirmation()
            await model.confirmReset()
            await settle()
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier], "Keep cannot be selected once Remove has started")
            XCTAssertNil(rendered[ResetSettingsView.removeIdentifier])
            XCTAssertEqual(text(ResetSettingsView.choiceFixedIdentifier), DeletionText.choiceFixedNote)

            model.choose(.keep)
            await settle()
            XCTAssertEqual(model.credentialChoice, .remove, "a pending reset takes no new choice")
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier])

            await model.retry()
            await settle()
            XCTAssertEqual(controls.finishCalls, 1)
            XCTAssertEqual(controls.choices, [.remove], "the retry starts no reset under another choice")
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier])
            XCTAssertNil(rendered[ResetSettingsView.removeIdentifier])
            XCTAssertEqual(text(ResetSettingsView.choiceFixedIdentifier), DeletionText.choiceFixedNote)
            assertTappable(ResetSettingsView.retryIdentifier)
        }
    }

    func testACompletedResetAndALockedSessionAreShown() async {
        let controls = FakeReset()
        controls.resetResults = [
            .success(ResetOutcome(state: .completed, progress: Self.resetProgress(.completed), blockers: [])),
        ]
        await withResetScreen(controls) { model in
            model.credentialChoice = .keep
            model.beginConfirmation()
            await model.confirmReset()
            await settle()
            XCTAssertEqual(text(ResetSettingsView.stateIdentifier), DeletionText.completedText)
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier])
            XCTAssertEqual(text(ResetSettingsView.choiceFixedIdentifier), DeletionText.choiceFixedNote)
        }
        scope = .captureOnly
        await withResetScreen(FakeReset()) { _ in
            XCTAssertEqual(text(ResetSettingsView.lockedIdentifier), DeletionText.lockedText)
            XCTAssertNil(rendered[ResetSettingsView.keepIdentifier])
        }
    }

    func testTheResetScreenIsReachedFromTheAudioSettingsScreen() async {
        let resetSettings = ResetSettingsModel(controls: FakeReset(), readScope: { .authenticated })
        let retention = RetentionSettingsModel(
            controls: FakeRetentionControls(
                status: RetentionStatus(
                    policy: RetentionPolicySetting(periodSeconds: 7 * 86_400, isDefault: true), storagePressure: .normal,
                    unresolved: [])),
            readScope: { .authenticated }, resetSettings: resetSettings)
        await show(
            NavigationStack {
                RetentionSettingsView(model: retention, renderReporter: { [unowned self] in rendered = $0 })
            })
        XCTAssertEqual(text(RetentionSettingsView.resetLinkIdentifier), DeletionText.resetHeading)
        assertTappable(RetentionSettingsView.resetLinkIdentifier)

        let withoutReset = RetentionSettingsModel(
            controls: FakeRetentionControls(
                status: RetentionStatus(
                    policy: RetentionPolicySetting(periodSeconds: 7 * 86_400, isDefault: true), storagePressure: .normal,
                    unresolved: [])),
            readScope: { .authenticated })
        await show(
            NavigationStack {
                RetentionSettingsView(model: withoutReset, renderReporter: { [unowned self] in rendered = $0 })
            })
        XCTAssertNil(rendered[RetentionSettingsView.resetLinkIdentifier])
    }
}
