import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class FakeRetentionControls: RetentionControlling, @unchecked Sendable {
    var statusResult: Result<RetentionStatus, Error>
    var setPeriodResult: Result<RetentionPolicySetting, Error> = .failure(RetentionControlError.unreadable)
    var retryResult: Result<String, Error> = .success("job-1")
    var deleteResult: Result<AudioDeletionReceipt, Error> = .success(.removed)
    private(set) var periodRequests: [Int64] = []
    private(set) var retryRequests: [String] = []
    private(set) var deleteRequests: [String] = []
    /// Invoked in the middle of `status`, so a test can change the session while a read is in flight.
    var duringStatus: (() -> Void)?

    init(status: RetentionStatus) { statusResult = .success(status) }

    func status() async throws -> RetentionStatus {
        duringStatus?()
        return try statusResult.get()
    }

    func setPeriod(seconds: Int64) async throws -> RetentionPolicySetting {
        periodRequests.append(seconds)
        return try setPeriodResult.get()
    }

    func retryTranscription(itemID: String) async throws -> String {
        retryRequests.append(itemID)
        return try retryResult.get()
    }

    func deleteAudio(itemID: String) async throws -> AudioDeletionReceipt {
        deleteRequests.append(itemID)
        return try deleteResult.get()
    }
}

@MainActor
final class RetentionSettingsModelTests: XCTestCase {
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

    private func makeModel(_ controls: FakeRetentionControls) -> RetentionSettingsModel {
        RetentionSettingsModel(controls: controls, readScope: { [unowned self] in scope })
    }

    func testRefreshShowsTheSevenDayDefaultTheSoleSourceCaseAndTheChosenPeriod() async {
        let controls = FakeRetentionControls(status: Self.status())
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.status?.unresolved.map(\.itemID), ["item-sole"])
        XCTAssertEqual(model.chosenDays, 7)
        XCTAssertEqual(model.status.map { RetentionSettingsText.periodSummary($0.policy) }, "7 days (default)")
        XCTAssertTrue(RetentionSettingsText.policyExplanation(model.status!.policy).contains("kept for 7 days"))
        XCTAssertFalse(RetentionSettingsText.policyExplanation(model.status!.policy).contains("default is"))
        XCTAssertTrue(RetentionSettingsText.soleSourceExplanation.contains("never removed automatically"))
    }

    func testPolicyExplanationFollowsTheStoredPeriodAndNamesSevenDaysOnlyAsDefault() {
        let custom = RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false)
        let text = RetentionSettingsText.policyExplanation(custom)
        XCTAssertTrue(text.contains("kept for 30 days"))
        XCTAssertFalse(text.contains("kept for 7 days"))
        XCTAssertTrue(text.contains("default is 7 days"))
    }

    func testNoAudioOnlyDeleteIsOfferedForSoleSourceCases() async {
        let model = makeModel(FakeRetentionControls(status: Self.status()))
        await model.refresh()
        let entry = model.status!.unresolved[0]
        XCTAssertTrue(entry.options.contains(.deleteAudio), "the core still lists it")
        XCTAssertFalse(RetentionSettingsText.offersDeleteAudio(entry))
    }

    func testAnUnsavedPeriodChoiceSurvivesARefresh() async {
        let model = makeModel(FakeRetentionControls(status: Self.status()))
        await model.refresh()
        model.chosenDays = 21
        await model.refresh()
        XCTAssertEqual(model.chosenDays, 21)
    }

    func testLowStoragePressureIsReportedWithoutOfferingToDeleteSoleSourceAudio() async {
        let model = makeModel(FakeRetentionControls(status: Self.status(pressure: .low)))

        await model.refresh()

        XCTAssertEqual(model.status?.storagePressure, .low)
        XCTAssertTrue(RetentionSettingsText.pressureLowNotice.contains("only copy"))
        XCTAssertEqual(model.status?.unresolved.count, 1, "pressure does not drop the case")
    }

    func testALockedSessionShowsNothingAndNeverAsksTheCore() async {
        scope = .captureOnly
        let controls = FakeRetentionControls(status: Self.status())
        controls.duringStatus = { XCTFail("the core must not be asked") }
        let model = makeModel(controls)

        await model.refresh()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.status)
    }

    func testARelockWhileReadingDropsTheAnswerUnseen() async {
        let controls = FakeRetentionControls(status: Self.status())
        let model = makeModel(controls)
        controls.duringStatus = { [unowned self] in scope = .captureOnly }

        await model.refresh()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.status)
        XCTAssertTrue(model.actions.isEmpty)
    }

    func testAppliedPeriodIsTheCoresAnswerAndAFailureLeavesTheShownPeriodUnchanged() async {
        let controls = FakeRetentionControls(status: Self.status())
        let model = makeModel(controls)
        await model.refresh()

        model.chosenDays = 30
        controls.setPeriodResult = .failure(RetentionControlError.audioRemovalUnavailable)
        await model.applyPeriod()
        XCTAssertEqual(controls.periodRequests, [30 * 86_400])
        XCTAssertEqual(model.status?.policy.periodSeconds, 7 * 86_400, "the shown period is the stored one")
        XCTAssertEqual(model.periodState, .failed(RetentionSettingsText.failure(RetentionControlError.audioRemovalUnavailable)))

        controls.setPeriodResult = .success(RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))
        controls.statusResult = .success(Self.status(periodDays: 30, isDefault: false))
        await model.applyPeriod()
        XCTAssertEqual(model.status?.policy, RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))
        XCTAssertNil(model.periodState)
        XCTAssertEqual(model.chosenDays, 30)
    }

    func testAPeriodOutsideTheBoundsIsNotSent() async {
        let controls = FakeRetentionControls(status: Self.status())
        let model = makeModel(controls)
        await model.refresh()

        model.chosenDays = RetentionSettingsModel.maximumDays + 1
        await model.applyPeriod()

        XCTAssertTrue(controls.periodRequests.isEmpty)
        XCTAssertNotNil(model.periodState?.message)
    }

    func testRetryQueuesAndKeepsTheRecordingListed() async {
        let controls = FakeRetentionControls(status: Self.status())
        let model = makeModel(controls)
        await model.refresh()

        await model.retryTranscription(itemID: "item-sole")

        XCTAssertEqual(controls.retryRequests, ["item-sole"])
        XCTAssertEqual(model.actions["item-sole"], .queued)
        XCTAssertEqual(model.status?.unresolved.map(\.itemID), ["item-sole"])
    }

    func testARefusedDeleteOfSoleSourceAudioSaysWhyAndNothingIsReportedRemoved() async {
        let controls = FakeRetentionControls(status: Self.status())
        controls.deleteResult = .failure(RetentionControlError.soleSourceUseItemDeletion)
        let model = makeModel(controls)
        await model.refresh()

        await model.deleteAudio(itemID: "item-sole")

        XCTAssertEqual(controls.deleteRequests, ["item-sole"])
        XCTAssertEqual(
            model.actions["item-sole"],
            .failed(RetentionSettingsText.failure(RetentionControlError.soleSourceUseItemDeletion)))
        XCTAssertNotEqual(model.actions["item-sole"], .removed)
        XCTAssertEqual(model.status?.unresolved.map(\.itemID), ["item-sole"])
    }

    func testAnIncompleteRemovalIsAnErrorNotACompletion() async {
        let controls = FakeRetentionControls(status: Self.status())
        controls.deleteResult = .failure(RetentionControlError.audioRemovalIncomplete(.ledgerUnavailable))
        let model = makeModel(controls)
        await model.refresh()

        await model.deleteAudio(itemID: "item-sole")

        guard case .failed(let message)? = model.actions["item-sole"] else { return XCTFail("expected a failure") }
        XCTAssertTrue(message.contains("did not finish"))
    }

    func testARelockDuringAnActionClearsEverything() async {
        let controls = FakeRetentionControls(status: Self.status())
        controls.retryResult = .failure(RetentionControlError.notAuthenticated)
        let model = makeModel(controls)
        await model.refresh()

        await model.retryTranscription(itemID: "item-sole")

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.status)
        XCTAssertTrue(model.actions.isEmpty)
    }

    func testClearDropsStoredDetails() async {
        let model = makeModel(FakeRetentionControls(status: Self.status()))
        await model.refresh()

        model.clear()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertNil(model.status)
    }
}
