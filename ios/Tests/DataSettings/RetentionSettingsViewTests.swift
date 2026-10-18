import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real retention screen in a window over the real model. The screen reports where each visible part landed,
/// the words it shows and whether it is enabled; the tests drive the model calls the screen's buttons make and then read
/// what is on screen.
@MainActor
final class RetentionSettingsViewTests: XCTestCase {
    private static let phone = CGSize(width: 390, height: 844)
    private static let minimumTouchTarget: CGFloat = 44
    private static let soleCase = UnresolvedSoleAudioCase(
        itemID: "item-sole", capturedAt: "2026-10-01T08:00:00Z", reason: .transcriptionFailed,
        options: [.retryTranscription, .deleteAudio])

    private static func status(
        periodDays: Int64 = 7, isDefault: Bool = true, pressure: CoreRetentionPressure = .normal,
        unresolved: [UnresolvedSoleAudioCase] = [soleCase]
    ) -> RetentionStatus {
        RetentionStatus(
            policy: RetentionPolicySetting(periodSeconds: periodDays * 86_400, isDefault: isDefault),
            storagePressure: pressure, unresolved: unresolved)
    }

    private var scope: CompositionReadScope = .authenticated
    private var rendered: [String: RetentionRenderedElement] = [:]
    private var window: UIWindow?

    /// Shows the screen over `controls`, lets its first read land, runs `body`, then takes the screen away.
    private func withScreen(
        _ controls: FakeRetentionControls, exportSettings: ExportSettingsModel? = nil,
        _ body: (RetentionSettingsModel) async throws -> Void
    ) async rethrows {
        rendered = [:]
        let model = RetentionSettingsModel(
            controls: controls, readScope: { [unowned self] in scope }, exportSettings: exportSettings)
        let hosting = UIHostingController(
            rootView: NavigationStack {
                RetentionSettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
            })
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = hosting
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        do {
            try await body(model)
        } catch {
            close()
            throw error
        }
        close()
    }

    private func close() {
        window?.isHidden = true
        window = nil
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
        XCTAssertGreaterThanOrEqual(element.frame.height, Self.minimumTouchTarget, identifier, file: file, line: line)
        XCTAssertEqual(element.isEnabled, enabled, "\(identifier) enabled", file: file, line: line)
    }

    private func assertNothingStoredIsShown(file: StaticString = #filePath, line: UInt = #line) {
        let stored = rendered.keys.filter {
            $0 == RetentionSettingsView.policyIdentifier || $0 == RetentionSettingsView.applyIdentifier
                || $0 == RetentionSettingsView.pressureIdentifier || $0.hasPrefix("data-settings.retention.case.")
                || $0.hasPrefix("data-settings.retention.retry.")
        }
        XCTAssertTrue(stored.isEmpty, "nothing stored is shown: \(stored)", file: file, line: line)
    }

    func testTheDefaultPolicySoleSourceRuleAndTheCoresChoicesAreShown() async {
        await withScreen(FakeRetentionControls(status: Self.status())) { _ in
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "7 days (default)")
            XCTAssertTrue(RetentionSettingsText.policyExplanation(Self.status().policy)
                .contains("kept for 7 days after they are successfully turned into text"))
            XCTAssertEqual(text(RetentionSettingsView.footerIdentifier), RetentionSettingsView.footerText)
            XCTAssertEqual(text(RetentionSettingsView.periodStepperIdentifier), "7 days")
            assertTappable(RetentionSettingsView.applyIdentifier)

            XCTAssertNotNil(rendered[RetentionSettingsView.caseIdentifier("item-sole")])
            assertTappable(RetentionSettingsView.retryIdentifier("item-sole"))
            XCTAssertEqual(
                text(RetentionSettingsView.deleteNoteIdentifier("item-sole")), RetentionSettingsText.deleteItemNote)
            XCTAssertFalse(
                rendered.values.contains { $0.text == "Delete audio" },
                "no audio-only delete is offered for an item's only recording")
            XCTAssertNil(rendered[RetentionSettingsView.pressureIdentifier], "normal storage shows no pressure notice")
            XCTAssertNil(rendered[RetentionSettingsView.noUnresolvedIdentifier])
        }
    }

    func testACustomStoredPeriodIsShownAndSevenDaysIsNamedOnlyAsTheDefault() async {
        await withScreen(FakeRetentionControls(status: Self.status(periodDays: 30, isDefault: false))) { _ in
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "30 days")
            let explanation = RetentionSettingsText.policyExplanation(Self.status(periodDays: 30, isDefault: false).policy)
            XCTAssertTrue(explanation.contains("kept for 30 days"))
            XCTAssertFalse(explanation.contains("kept for 7 days"))
            XCTAssertTrue(explanation.contains("The default is 7 days."))
            XCTAssertEqual(text(RetentionSettingsView.periodStepperIdentifier), "30 days")
        }
    }

    func testAnyPeriodTheCoreSupportsCanBeChosenNotOnlyThePresets() async {
        await withScreen(FakeRetentionControls(status: Self.status())) { model in
            XCTAssertFalse(RetentionSettingsView.periodChoiceDays.contains(21))
            model.chosenDays = 21
            await settle()
            XCTAssertEqual(text(RetentionSettingsView.periodStepperIdentifier), "21 days")
            assertTappable(RetentionSettingsView.stepperIdentifier)
            XCTAssertEqual(RetentionSettingsModel.minimumDays, 1)
            XCTAssertEqual(RetentionSettingsModel.maximumDays, Int(RetentionPolicySetting.maximumPeriodSeconds / 86_400))
        }
    }

    func testTheInfoSheetCarriesTheFullExplanationTheScreenShowedBefore() async {
        var paragraphs: [String] = []
        let status = Self.status()
        let sheet = SettingsInfoSheet(
            title: "Audio retention", paragraphs: RetentionSettingsView.infoParagraphs(for: status),
            renderReporter: { paragraphs = $0 })
        let shown = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        shown.rootViewController = UIHostingController(rootView: sheet)
        shown.makeKeyAndVisible()
        window = shown
        await settle()
        close()
        XCTAssertEqual(paragraphs.count, 3)
        XCTAssertTrue(paragraphs.first?.contains("kept for 7 days after they are successfully turned into text") == true)
        XCTAssertTrue(paragraphs.contains(RetentionSettingsText.soleSourceExplanation))
        XCTAssertTrue(paragraphs.contains(RetentionSettingsText.periodChangeNote))
    }

    func testLowStorageShowsThePressureNoticeAndKeepsTheSoleSourceCase() async {
        await withScreen(FakeRetentionControls(status: Self.status(pressure: .low))) { _ in
            XCTAssertEqual(text(RetentionSettingsView.pressureIdentifier), RetentionSettingsText.pressureLowNotice)
            XCTAssertNotNil(rendered[RetentionSettingsView.caseIdentifier("item-sole")])
            XCTAssertFalse(rendered.values.contains { $0.text == "Delete audio" })
        }
    }

    func testRowControlsFollowTheCoresOptionsAndAnEmptyListSaysCapturesAreSaved() async {
        let noRetry = UnresolvedSoleAudioCase(
            itemID: "item-unsupported", capturedAt: "2026-10-02T08:00:00Z", reason: .transcriptionUnsupported,
            options: [.deleteAudio])
        await withScreen(FakeRetentionControls(status: Self.status(unresolved: [noRetry]))) { _ in
            XCTAssertEqual(
                text(RetentionSettingsView.caseIdentifier("item-unsupported")),
                RetentionSettingsText.reason(.transcriptionUnsupported))
            XCTAssertNil(rendered[RetentionSettingsView.retryIdentifier("item-unsupported")])
            XCTAssertNotNil(rendered[RetentionSettingsView.deleteNoteIdentifier("item-unsupported")])
        }
        await withScreen(FakeRetentionControls(status: Self.status(unresolved: []))) { _ in
            XCTAssertEqual(text(RetentionSettingsView.noUnresolvedIdentifier), RetentionSettingsText.noUnresolvedText)
        }
    }

    func testARetryShowsItsQueuedStateAndAFailedRetryShowsItsErrorWithTheRecordingStillListed() async {
        let controls = FakeRetentionControls(status: Self.status())
        await withScreen(controls) { model in
            let gate = RetentionOperationGate()
            controls.operationGate = gate
            let retry = Task { await model.retryTranscription(itemID: "item-sole") }
            await gate.waitForArrival()
            await settle()
            assertTappable(RetentionSettingsView.retryIdentifier("item-sole"), enabled: false)
            XCTAssertEqual(text(RetentionSettingsView.actionStateIdentifier("item-sole")), "Working…")
            gate.open()
            await retry.value
            await settle()
            XCTAssertEqual(
                text(RetentionSettingsView.actionStateIdentifier("item-sole")), RetentionActionState.queued.message)
            assertTappable(RetentionSettingsView.retryIdentifier("item-sole"))

            controls.operationGate = nil
            controls.retryResult = .failure(RetentionControlError.jobInFlight)
            await model.retryTranscription(itemID: "item-sole")
            await settle()
            XCTAssertEqual(
                text(RetentionSettingsView.actionStateIdentifier("item-sole")),
                RetentionSettingsText.failure(RetentionControlError.jobInFlight))
            XCTAssertNotNil(rendered[RetentionSettingsView.caseIdentifier("item-sole")])
        }
    }

    func testSavingAPeriodDisablesSaveWhileWorkingThenShowsTheCoresStoredPeriod() async {
        let controls = FakeRetentionControls(status: Self.status())
        await withScreen(controls) { model in
            model.chosenDays = 30
            await settle()
            XCTAssertEqual(text(RetentionSettingsView.periodStepperIdentifier), "30 days")
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "7 days (default)", "unsaved is not stored")

            let gate = RetentionOperationGate()
            controls.operationGate = gate
            controls.setPeriodResult = .success(RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))
            controls.statusResult = .success(Self.status(periodDays: 30, isDefault: false))
            let save = Task { await model.applyPeriod() }
            await gate.waitForArrival()
            await settle()
            assertTappable(RetentionSettingsView.applyIdentifier, enabled: false)
            XCTAssertEqual(text(RetentionSettingsView.periodStateIdentifier), "Working…")
            gate.open()
            await save.value
            await settle()
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "30 days")
            XCTAssertNil(rendered[RetentionSettingsView.periodStateIdentifier])
            assertTappable(RetentionSettingsView.applyIdentifier)
        }
    }

    func testARefusedPeriodChangeShowsWhyAndKeepsTheStoredPeriod() async {
        let controls = FakeRetentionControls(status: Self.status())
        controls.setPeriodResult = .failure(RetentionControlError.audioRemovalUnavailable)
        await withScreen(controls) { model in
            model.chosenDays = 30
            await model.applyPeriod()
            await settle()
            XCTAssertEqual(
                text(RetentionSettingsView.periodStateIdentifier),
                RetentionSettingsText.failure(RetentionControlError.audioRemovalUnavailable))
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "7 days (default)")
        }
    }

    func testALockedSessionShowsOnlyTheLockedMessage() async {
        scope = .captureOnly
        let controls = FakeRetentionControls(status: Self.status())
        await withScreen(controls) { _ in
            XCTAssertEqual(text(RetentionSettingsView.lockedIdentifier), RetentionSettingsText.lockedText)
            assertNothingStoredIsShown()
            XCTAssertEqual(controls.statusReads, 0, "the core is not asked while locked")
        }
    }

    func testARelockAfterLoadingEmptiesTheScreen() async {
        await withScreen(FakeRetentionControls(status: Self.status())) { model in
            XCTAssertNotNil(rendered[RetentionSettingsView.policyIdentifier])
            scope = .captureOnly
            await model.refresh()
            await settle()
            XCTAssertEqual(text(RetentionSettingsView.lockedIdentifier), RetentionSettingsText.lockedText)
            assertNothingStoredIsShown()
        }
    }

    func testAReadFailureShowsItsMessageAndATryAgainControl() async {
        let controls = FakeRetentionControls(status: Self.status())
        controls.statusResult = .failure(RetentionControlError.unreadable)
        await withScreen(controls) { model in
            XCTAssertEqual(
                text(RetentionSettingsView.failureIdentifier), RetentionSettingsText.failure(RetentionControlError.unreadable))
            assertTappable(RetentionSettingsView.tryAgainIdentifier)
            assertNothingStoredIsShown()

            controls.statusResult = .success(Self.status())
            await model.refresh()
            await settle()
            XCTAssertEqual(text(RetentionSettingsView.policyIdentifier), "7 days (default)")
            XCTAssertNil(rendered[RetentionSettingsView.failureIdentifier])
        }
    }

    func testTheExportLinkIsShownOnlyWhenTheAppAssemblesAnExportSection() async throws {
        await withScreen(FakeRetentionControls(status: Self.status())) { _ in
            XCTAssertNil(rendered[RetentionSettingsView.exportLinkIdentifier])
        }

        let world = try ExportWorld()
        addTeardownBlock { world.tearDown() }
        let exportSettings = ExportSettingsModel(controls: world.makeService(), readScope: { .authenticated })
        await withScreen(FakeRetentionControls(status: Self.status()), exportSettings: exportSettings) { _ in
            XCTAssertEqual(text(RetentionSettingsView.exportLinkIdentifier), "Export a copy of your notes")
            assertTappable(RetentionSettingsView.exportLinkIdentifier)
            XCTAssertEqual(world.core.requests, [], "showing the link reads nothing")
        }
    }
}
