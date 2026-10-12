import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// `AudioRetentionService` against real files in a temporary protected-storage tree, with a scripted core that removes
/// the real file when it approves, and a file system that fails ledger steps once. The real core boundary is covered by `AudioRetentionCoreIntegrationTests`.
final class AudioRetentionServiceTests: XCTestCase {
    private var storage: RetentionStorage!
    private let faults = FaultPlan()
    private let core = ScriptedRetentionCore()
    private var fileSystem: FaultingRetentionFileSystem!

    override func setUpWithError() throws {
        try super.setUpWithError()
        storage = try RetentionStorage()
        fileSystem = FaultingRetentionFileSystem(faults: faults)
    }

    override func tearDown() {
        storage.tearDown()
        super.tearDown()
    }

    private func makeService(
        pressure: CoreRetentionPressure = .normal, maximumAttempts: Int = 5, maximumRemovalsPerSweep: Int = 50
    ) -> AudioRetentionService {
        AudioRetentionService(
            core: core, layout: storage.layout, fileSystem: fileSystem,
            pressureProbe: FixedPressureProbe(pressure: pressure), maximumAttempts: maximumAttempts,
            maximumRemovalsPerSweep: maximumRemovalsPerSweep)
    }

    private func expire(_ itemID: String, audio fileName: String) throws {
        try storage.writeAudio(fileName)
        core.setListing(core.currentListing + [RetentionFixtures.item(itemID, audio: fileName)])
    }

    private func outcome(_ report: AudioRetentionSweepReport, _ itemID: String) -> AudioRemovalOutcome? {
        report.items.first { $0.item.itemID == itemID }?.removal
    }

    // MARK: Removal

    func testApprovedAudioIsRemovedAndTheCompletionIsRecorded() async throws {
        try expire("item-1", audio: "a.m4a")
        let report = await makeService().sweep()

        XCTAssertEqual(outcome(report, "item-1"), .removed)
        XCTAssertTrue(report.converged)
        XCTAssertFalse(storage.audioExists("a.m4a"))
        let entries = try storage.ledgerEntries()
        XCTAssertEqual(entries.map(\.state), [.removed])
        XCTAssertEqual(entries.first?.itemID, "item-1")
    }

    func testARepeatedSweepReportsAlreadyRemovedAndTouchesNothing() async throws {
        try expire("item-1", audio: "a.m4a")
        let service = makeService()
        _ = await service.sweep()

        let report = await service.sweep()
        XCTAssertEqual(outcome(report, "item-1"), .alreadyRemoved)
        XCTAssertTrue(report.converged)
        XCTAssertEqual(core.removedFileNames, ["a.m4a"])
        XCTAssertEqual(core.removalRequests, ["item-1"], "finished removals are not asked of the core again")
    }

    func testOnlyTheApprovedFileIsRemoved() async throws {
        try expire("item-1", audio: "a.m4a")
        try storage.writeAudio("other.m4a")
        _ = await makeService().sweep()

        XCTAssertFalse(storage.audioExists("a.m4a"))
        XCTAssertTrue(storage.audioExists("other.m4a"))
    }

    // MARK: Interruption

    func testInterruptionBeforeRemovalConvergesOnTheNextSweep() async throws {
        try expire("item-1", audio: "a.m4a")
        core.interruptRemoval(of: "a.m4a")
        let service = makeService()

        let interrupted = await service.sweep()
        XCTAssertEqual(outcome(interrupted, "item-1"), .retryLater(.coreUnavailable))
        XCTAssertFalse(interrupted.converged)
        XCTAssertTrue(storage.audioExists("a.m4a"), "an interrupted removal leaves the audio in place")
        XCTAssertEqual(try storage.ledgerEntries(), [])

        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .removed)
        XCTAssertTrue(next.converged)
        XCTAssertFalse(storage.audioExists("a.m4a"))
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testARemovalTheCoreCannotCarryOutIsRetriedAndLeavesTheAudio() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failEffect(of: "a.m4a")
        let service = makeService()

        let failed = await service.sweep()
        XCTAssertEqual(outcome(failed, "item-1"), .retryLater(.storageUnavailable))
        XCTAssertFalse(failed.converged)
        XCTAssertTrue(storage.audioExists("a.m4a"))
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.failing])

        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .removed)
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testInterruptionAfterRemovalIsNotReportedUntilTheCompletionIsRecorded() async throws {
        try expire("item-1", audio: "a.m4a")
        faults.arm("fs.replace")
        let service = makeService()

        let interrupted = await service.sweep()
        XCTAssertFalse(storage.audioExists("a.m4a"), "the file was removed")
        XCTAssertEqual(outcome(interrupted, "item-1"), .retryLater(.ledgerUnavailable))
        XCTAssertFalse(interrupted.converged, "an unrecorded completion is not a success")
        XCTAssertEqual(try storage.ledgerEntries(), [])

        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .alreadyRemoved, "the file was already absent")
        XCTAssertTrue(next.converged)
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testANewServiceAfterAnInterruptionConvergesFromTheFilesAlone() async throws {
        try expire("item-1", audio: "a.m4a")
        faults.arm("fs.replace")
        _ = await makeService().sweep()

        let afterRestart = await makeService().sweep()
        XCTAssertEqual(outcome(afterRestart, "item-1"), .alreadyRemoved)
        XCTAssertTrue(afterRestart.converged)
    }

    func testAFileThatSurvivesRemovalIsNeverReportedRemoved() async throws {
        try expire("item-1", audio: "a.m4a")
        core.keepSilently("a.m4a")

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .retryLater(.storageUnavailable))
        XCTAssertFalse(report.converged)
        XCTAssertTrue(storage.audioExists("a.m4a"))
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.failing])
    }

    // MARK: Authority stays with the core

    func testALeaseThatExpiresBeforeTheEffectRemovesNothingAndConvergesOnTheNextSweep() async throws {
        try expire("item-1", audio: "a.m4a")
        core.expireLeaseBeforeEffect(of: "a.m4a")
        let service = makeService()

        let expired = await service.sweep()
        XCTAssertTrue(storage.audioExists("a.m4a"), "no effect runs without a pinned fence")
        XCTAssertEqual(outcome(expired, "item-1"), .retryLater(.coreUnavailable))
        XCTAssertFalse(expired.converged)
        XCTAssertEqual(try storage.ledgerEntries(), [])

        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .removed)
        XCTAssertFalse(storage.audioExists("a.m4a"))
        XCTAssertTrue(next.converged)
    }

    func testALapsedLeaseAfterTheFileWentIsNotRecordedAndConvergesOnTheNextSweep() async throws {
        try expire("item-1", audio: "a.m4a")
        core.lapseLease(of: "a.m4a")
        let service = makeService()

        let lapsed = await service.sweep()
        XCTAssertFalse(storage.audioExists("a.m4a"), "the effect ran before the core reported the lapse")
        XCTAssertEqual(outcome(lapsed, "item-1"), .retryLater(.coreUnavailable))
        XCTAssertFalse(lapsed.converged, "a removal the core could not vouch for is not a success")
        XCTAssertEqual(try storage.ledgerEntries(), [])

        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .alreadyRemoved)
        XCTAssertTrue(next.converged)
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testAStaleListingCannotRemoveAudioTheCoreNowRefuses() async throws {
        try expire("item-1", audio: "a.m4a")
        core.setDecision(.denied(.unresolved(.transcriptionPending)), itemID: "item-1")

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .denied(.unresolved(.transcriptionPending)))
        XCTAssertTrue(report.converged, "a refusal is settled, not a failure")
        XCTAssertTrue(storage.audioExists("a.m4a"))
        XCTAssertEqual(core.removalRequests, ["item-1"], "the core was asked again at removal time")
        XCTAssertEqual(try storage.ledgerEntries(), [])
    }

    func testEveryDenialLeavesTheAudioInPlace() async throws {
        let denials: [AudioRemovalDenial] = [
            .itemNotFound, .audioReferenceMismatch, .deletionOwnsCleanup, .notYetExpired,
            .unresolved(.transcriptEmpty),
        ]
        for (index, denial) in denials.enumerated() {
            let name = "denied-\(index).m4a"
            try expire("item-\(index)", audio: name)
            core.setDecision(.denied(denial), itemID: "item-\(index)")
        }
        let report = await makeService().sweep()
        for (index, denial) in denials.enumerated() {
            XCTAssertEqual(outcome(report, "item-\(index)"), .denied(denial))
            XCTAssertTrue(storage.audioExists("denied-\(index).m4a"))
        }
    }

    func testAnApprovalFromAnEarlierSweepIsNotTrustedAfterTheAudioBecomesProtected() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failEffect(of: "a.m4a")
        let service = makeService()
        _ = await service.sweep()

        // Between sweeps the item gained a new protected state; the first attempt's approval must not carry over.
        core.setDecision(.denied(.deletionOwnsCleanup), itemID: "item-1")
        let next = await service.sweep()
        XCTAssertEqual(outcome(next, "item-1"), .denied(.deletionOwnsCleanup))
        XCTAssertTrue(storage.audioExists("a.m4a"))
    }

    func testSoleSourceAndUnresolvedAudioIsNeverTouched() async throws {
        try storage.writeAudio("sole.m4a")
        try storage.writeAudio("pending.m4a")
        try storage.writeAudio("fresh.m4a")
        core.setListing([
            RetentionFixtures.item(
                "sole", audio: "sole.m4a", status: .expired(expiredAt: "2026-10-05T09:00:00Z"), soleSource: true),
            RetentionFixtures.item("pending", audio: "pending.m4a", status: .unresolved(.transcriptionPending)),
            RetentionFixtures.item("fresh", audio: "fresh.m4a", status: .retained(expiresAt: "2026-10-20T09:00:00Z")),
        ])

        let report = await makeService().sweep()
        XCTAssertTrue(report.converged)
        XCTAssertTrue(report.items.allSatisfy { $0.removal == nil })
        XCTAssertEqual(core.removalRequests, [])
        XCTAssertTrue(["sole.m4a", "pending.m4a", "fresh.m4a"].allSatisfy(storage.audioExists))
        XCTAssertEqual(core.removedFileNames, [])
    }

    // MARK: Core and ledger failures

    func testAnUnreadableCoreIsUnknownNotNothingToDo() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failSweep(RetentionFixtureError())

        let report = await makeService().sweep()
        XCTAssertEqual(report.listingFailure, .coreUnavailable)
        XCTAssertFalse(report.converged)
        XCTAssertTrue(storage.audioExists("a.m4a"))
    }

    func testACoreThatFailsAtRemovalTimeRemovesNothing() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failDecisions(RetentionFixtureError())

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .retryLater(.coreUnavailable))
        XCTAssertFalse(report.converged)
        XCTAssertTrue(storage.audioExists("a.m4a"))
    }

    func testACancelledCoreIsReportedAsCancelled() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failDecisions(CoreAudioRetentionClient.shutDownFailure)

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .retryLater(.cancelled))
    }

    func testACorruptLedgerIsTreatedAsEmptyAndRepairedByTheNextRecord() async throws {
        try expire("item-1", audio: "a.m4a")
        try Data("{not json".utf8).write(to: storage.ledgerURL)

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .removed)
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testAFileThatWasAlreadyAbsentIsAlreadyRemovedWhateverTheLedgerSays() async throws {
        try expire("item-1", audio: "a.m4a")
        _ = await makeService().sweep()
        try Data("{not json".utf8).write(to: storage.ledgerURL)

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .alreadyRemoved)
        XCTAssertTrue(report.converged)
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed], "the ledger is repaired")
    }

    func testAnUnreadableLedgerBlocksRemovalInsteadOfBeingTreatedAsEmpty() async throws {
        try expire("item-1", audio: "a.m4a")
        try Data("{}".utf8).write(to: storage.ledgerURL)
        faults.arm("fs.read:\(AudioRetentionLedger.fileName)")

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .retryLater(.ledgerUnavailable))
        XCTAssertTrue(storage.audioExists("a.m4a"))
        XCTAssertEqual(core.removalRequests, [], "nothing is removed without a place to record it")
    }

    // MARK: Bounded failure

    func testRepeatedFailuresBecomePermanentAndStayVisible() async throws {
        try expire("item-1", audio: "a.m4a")
        core.keepSilently("a.m4a")
        let service = makeService(maximumAttempts: 3)

        let first = await service.sweep()
        let second = await service.sweep()
        let third = await service.sweep()
        XCTAssertEqual(outcome(first, "item-1"), .retryLater(.storageUnavailable))
        XCTAssertEqual(outcome(second, "item-1"), .retryLater(.storageUnavailable))
        XCTAssertEqual(outcome(third, "item-1"), .failedPermanently(.attemptsExhausted))

        let later = await service.sweep()
        XCTAssertEqual(outcome(later, "item-1"), .failedPermanently(.attemptsExhausted), "still visible")
        XCTAssertFalse(later.converged)
        XCTAssertEqual(core.removalRequests.count, 3, "no further attempt is made")
        XCTAssertEqual(core.authorizedRequests, ["item-1"], "the core is only asked whether it still approves")
        XCTAssertTrue(storage.audioExists("a.m4a"))
    }

    func testAPermanentFailureIsNotReportedOnceTheCoreNoLongerApprovesTheRemoval() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failEffect(of: "a.m4a")
        let service = makeService(maximumAttempts: 1)
        let failed = await service.sweep()
        XCTAssertEqual(outcome(failed, "item-1"), .failedPermanently(.attemptsExhausted))

        core.setDecision(.denied(.deletionOwnsCleanup), itemID: "item-1")
        let later = await service.sweep()
        XCTAssertEqual(outcome(later, "item-1"), .denied(.deletionOwnsCleanup))
        XCTAssertTrue(later.converged)
        XCTAssertEqual(core.removalRequests.count, 1)
    }

    func testAResetFailureRearmsTheRemoval() async throws {
        try expire("item-1", audio: "a.m4a")
        core.failEffect(of: "a.m4a")
        let service = makeService(maximumAttempts: 1)
        let failed = await service.sweep()
        XCTAssertEqual(outcome(failed, "item-1"), .failedPermanently(.attemptsExhausted))

        try await service.resetFailure(itemID: "item-1", audioReference: RetentionFixtures.audioReference("a.m4a"))
        let rearmed = await service.sweep()
        XCTAssertEqual(outcome(rearmed, "item-1"), .removed)
        XCTAssertFalse(storage.audioExists("a.m4a"))
    }

    func testAResetDoesNotForgetACompletedRemoval() async throws {
        try expire("item-1", audio: "a.m4a")
        let service = makeService()
        _ = await service.sweep()

        try await service.resetFailure(itemID: "item-1", audioReference: RetentionFixtures.audioReference("a.m4a"))
        XCTAssertEqual(try storage.ledgerEntries().map(\.state), [.removed])
    }

    func testAReferenceOutsideTheFinalizedStoreFailsAtOnceAndRemovesNothing() async throws {
        try storage.writeAudio("keep.m4a")
        let escaping = AudioRetentionItem(
            itemID: "item-1", audioReference: "FinalizedAudio/../Configuration/audio-retention-ledger.json",
            status: .expired(expiredAt: "2026-10-05T09:00:00Z"), isSoleSource: false, options: [.deleteAudio],
            cleanup: .removeAudio(
                audioReference: "FinalizedAudio/../Configuration/audio-retention-ledger.json",
                expiredAt: "2026-10-05T09:00:00Z"),
            storageNotice: nil)
        core.setListing([escaping])

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .failedPermanently(.audioReferenceUnresolvable))
        XCTAssertTrue(storage.audioExists("keep.m4a"))
        XCTAssertEqual(core.removalRequests, [])
        XCTAssertEqual(core.removedFileNames, [])
    }

    // MARK: Bounded sweep

    func testASweepRemovesAtMostItsLimitAndReportsTheRest() async throws {
        for index in 0..<5 { try expire("item-\(index)", audio: "a\(index).m4a") }
        let service = makeService(maximumRemovalsPerSweep: 2)

        let first = await service.sweep()
        XCTAssertEqual(first.deferredRemovals, 3)
        XCTAssertFalse(first.converged)
        XCTAssertEqual((0..<5).filter { storage.audioExists("a\($0).m4a") }.count, 3)

        // The core keeps offering what it already approved; finished removals must not starve the rest.
        let second = await service.sweep()
        XCTAssertEqual(second.deferredRemovals, 1)
        let third = await service.sweep()
        XCTAssertEqual(third.deferredRemovals, 0)
        XCTAssertTrue(third.converged)
        XCTAssertTrue((0..<5).allSatisfy { !storage.audioExists("a\($0).m4a") })
    }

    func testOneFailureDoesNotStopTheOtherItems() async throws {
        try expire("item-1", audio: "a.m4a")
        try expire("item-2", audio: "b.m4a")
        core.failEffect(of: "a.m4a")

        let report = await makeService().sweep()
        XCTAssertEqual(outcome(report, "item-1"), .retryLater(.storageUnavailable))
        XCTAssertEqual(outcome(report, "item-2"), .removed)
        XCTAssertTrue(storage.audioExists("a.m4a"))
        XCTAssertFalse(storage.audioExists("b.m4a"))
    }

    // MARK: Pressure

    func testStoragePressureIsReportedAndNeverMakesAudioEligible() async throws {
        try storage.writeAudio("fresh.m4a")
        core.setListing([
            RetentionFixtures.item("fresh", audio: "fresh.m4a", status: .retained(expiresAt: "2026-10-20T09:00:00Z"))
        ])

        let report = await makeService(pressure: .low).sweep()
        XCTAssertEqual(report.pressure, .low)
        XCTAssertEqual(core.sweepPressures, [.low])
        XCTAssertTrue(storage.audioExists("fresh.m4a"))
        XCTAssertEqual(core.removalRequests, [])
    }

    // MARK: Ledger size and reads

    func testASweepReadsTheLedgerOnceHoweverManyRemovalsAreFinished() async throws {
        for index in 0..<40 { try expire("item-\(index)", audio: "a\(index).m4a") }
        let service = makeService()
        _ = await service.sweep()
        XCTAssertEqual(try storage.ledgerEntries().count, 40)
        let readsBefore = fileSystem.readCount(of: AudioRetentionLedger.fileName)

        let repeated = await service.sweep()
        XCTAssertTrue(repeated.converged)
        XCTAssertEqual(repeated.items.compactMap(\.removal).filter { $0 == .alreadyRemoved }.count, 40)
        XCTAssertEqual(fileSystem.readCount(of: AudioRetentionLedger.fileName) - readsBefore, 1)
    }

    func testEntriesForAudioTheCoreNoLongerListsAreForgotten() async throws {
        try expire("item-1", audio: "a.m4a")
        try expire("item-2", audio: "b.m4a")
        let service = makeService()
        _ = await service.sweep()
        XCTAssertEqual(try storage.ledgerEntries().map(\.itemID), ["item-1", "item-2"])

        core.setListing([RetentionFixtures.item("item-2", audio: "b.m4a")])
        let report = await service.sweep()
        XCTAssertEqual(outcome(report, "item-2"), .alreadyRemoved)
        XCTAssertEqual(try storage.ledgerEntries().map(\.itemID), ["item-2"])
    }

    func testNothingIsForgottenWhenTheCoreCouldNotBeRead() async throws {
        try expire("item-1", audio: "a.m4a")
        let service = makeService()
        _ = await service.sweep()

        core.failSweep(RetentionFixtureError())
        _ = await service.sweep()
        XCTAssertEqual(try storage.ledgerEntries().map(\.itemID), ["item-1"])
    }

    // MARK: Serialization

    func testConcurrentSweepsDoNotRemoveTheSameAudioTwice() async throws {
        for index in 0..<10 { try expire("item-\(index)", audio: "a\(index).m4a") }
        let service = makeService()

        async let first = service.sweep()
        async let second = service.sweep()
        let reports = await [first, second]

        XCTAssertEqual(core.removedFileNames.sorted(), (0..<10).map { "a\($0).m4a" }.sorted())
        XCTAssertTrue(reports.allSatisfy(\.converged))
        let outcomes = reports.flatMap { $0.items.compactMap(\.removal) }
        XCTAssertEqual(outcomes.filter { $0 == .removed }.count, 10)
        XCTAssertEqual(outcomes.filter { $0 == .alreadyRemoved }.count, 10)
    }
}
