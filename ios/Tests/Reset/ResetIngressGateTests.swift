import XCTest
@testable import OhAndServices

/// The reset boundary and the rejected-capture audio discard, driven through the production `ForegroundIngressService`
/// over real protected-storage files and a scripted importer.
final class ResetIngressGateTests: IngressStorageTestCase {
    private func makeGatedService(
        importer: ForegroundIngressImporting,
        gateFileSystem: IngressFileSystem = FileManagerIngressFileSystem(),
        markers: ResetMarkerStore? = nil
    ) throws -> (ForegroundIngressService, ResetIngressGate) {
        let gate = ResetIngressGate(
            wrapping: importer, layout: layout, markers: markers ?? FileResetMarkerStore(layout: layout),
            fileSystem: gateFileSystem)
        return (try makeService(importer: gate), gate)
    }

    func testAnImportIsHeldUntilTheBoundaryIsReleasedWhileTheCapturesStagedBeforeTheTapPassThrough() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [
            FakeForegroundIngressImporter.confirmation("cap-before", itemID: "item-before"),
            FakeForegroundIngressImporter.confirmation("cap-after", itemID: "item-after"),
        ]
        let (service, gate) = try makeGatedService(importer: importer)
        gate.hold(stagedBeforeTheTap: ["cap-before"])

        var beforeOutcome: IngressOutcome?
        var afterOutcome: IngressOutcome?
        service.submit(textRecord("cap-before")) { beforeOutcome = $0 }
        service.submit(textRecord("cap-after")) { afterOutcome = $0 }

        guard case .saved(_)? = beforeOutcome else { return XCTFail("a capture from before the tap imports: \(String(describing: beforeOutcome))") }
        XCTAssertNil(afterOutcome, "a capture made after the tap waits")
        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, ["cap-before"])
        XCTAssertEqual(gate.heldImportCount, 1)
        XCTAssertTrue(exists(recordURL("cap-after")), "the held capture is already durable")

        gate.release()

        guard case .saved(let acknowledgment)? = afterOutcome else {
            return XCTFail("the held capture imports after the release: \(String(describing: afterOutcome))")
        }
        XCTAssertEqual(acknowledgment.itemID, "item-after")
        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, ["cap-before", "cap-after"])
        XCTAssertEqual(gate.heldImportCount, 0)
        XCTAssertFalse(exists(recordURL("cap-after")))
    }

    func testAfterTheReleaseImportsPassStraightThroughAndAReleaseWithNothingHeldIsHarmless() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a")]
        let (service, gate) = try makeGatedService(importer: importer)
        gate.release()
        gate.hold(stagedBeforeTheTap: [])
        gate.release()
        gate.release()

        var outcome: IngressOutcome?
        service.submit(textRecord("cap-a")) { outcome = $0 }

        guard case .saved(_)? = outcome else { return XCTFail("an unheld gate imports at once: \(String(describing: outcome))") }
    }

    func testALaterTapStartsTheWaitingImportsItListsAndKeepsHoldingTheRest() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [FakeForegroundIngressImporter.confirmation("cap-listed", itemID: "item-listed")]
        importer.holdsCompletions = true
        let (service, gate) = try makeGatedService(importer: importer)
        gate.hold(stagedBeforeTheTap: [])

        var parkedOutcome: IngressOutcome?
        var laterOutcome: IngressOutcome?
        service.submit(textRecord("cap-listed")) { parkedOutcome = $0 }
        service.submit(textRecord("cap-later")) { laterOutcome = $0 }
        XCTAssertEqual(gate.heldImportCount, 2)
        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, [])

        gate.hold(stagedBeforeTheTap: ["cap-listed"])

        XCTAssertEqual(
            importer.importedRecords.map { $0.captureID }, ["cap-listed"],
            "a waiting capture the new tap lists is its pre-tap input, so its import starts at once")
        XCTAssertEqual(gate.heldImportCount, 1, "a waiting capture the new tap does not list keeps waiting")

        // The reset's own import of the listed capture joins the attempt in flight, which no release has to unblock.
        var resetImportOutcome: IngressOutcome?
        service.submit(textRecord("cap-listed")) { resetImportOutcome = $0 }
        importer.release()

        guard case .saved(_)? = resetImportOutcome, case .saved(_)? = parkedOutcome else {
            return XCTFail("both completions of the listed capture arrive without a release: \(String(describing: resetImportOutcome)), \(String(describing: parkedOutcome))")
        }
        XCTAssertNil(laterOutcome)
        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, ["cap-listed"], "one import served both")
    }

    // MARK: Launch

    func testAGateCreatedWhileATapIsRecordedHoldsFromLaunchAtTheRecordedList() throws {
        let markers = FileResetMarkerStore(layout: layout)
        try markers.recordRequest(ResetRequest(
            credentialChoice: .remove, stagedBeforeTheTap: ResetStagingSnapshot(captureIDs: ["cap-before"])))
        let importer = FakeForegroundIngressImporter()
        importer.results = [
            FakeForegroundIngressImporter.confirmation("cap-before", itemID: "item-before"),
            FakeForegroundIngressImporter.confirmation("cap-after", itemID: "item-after"),
        ]
        let (service, gate) = try makeGatedService(importer: importer, markers: markers)

        var beforeOutcome: IngressOutcome?
        var afterOutcome: IngressOutcome?
        service.submit(textRecord("cap-before")) { beforeOutcome = $0 }
        service.submit(textRecord("cap-after")) { afterOutcome = $0 }

        guard case .saved(_)? = beforeOutcome else {
            return XCTFail("a capture listed at the recorded tap imports: \(String(describing: beforeOutcome))")
        }
        XCTAssertNil(afterOutcome, "a capture made after a tap recorded by an earlier process waits from launch")
        XCTAssertEqual(gate.heldImportCount, 1)
        XCTAssertTrue(exists(recordURL("cap-after")))

        gate.release()

        guard case .saved(_)? = afterOutcome else {
            return XCTFail("the held capture imports after the release: \(String(describing: afterOutcome))")
        }
    }

    func testAGateWhoseMarkersCannotBeReadHoldsEveryImport() throws {
        let faults = FaultPlan()
        faults.arm("intent.read")
        let importer = FakeForegroundIngressImporter()
        importer.results = [FakeForegroundIngressImporter.confirmation("cap-a", itemID: "item-a")]
        let (service, gate) = try makeGatedService(importer: importer, markers: MemoryResetMarkerStore(faults: faults))

        var outcome: IngressOutcome?
        service.submit(textRecord("cap-a")) { outcome = $0 }

        XCTAssertNil(outcome, "a recorded tap cannot be ruled out, so nothing imports before reset resolves it")
        XCTAssertEqual(importer.importedRecords.map { $0.captureID }, [])
        XCTAssertEqual(gate.heldImportCount, 1)
    }

    // MARK: Rejected captures

    func testARejectedLateVoiceHandoffRemovesBothTheRecordAndTheAudioItFinalized() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [.failed(.itemDeleted)]
        let (service, _) = try makeGatedService(importer: importer)
        let record = try audioRecord("cap-a")

        var outcome: IngressOutcome?
        service.submit(record) { outcome = $0 }

        XCTAssertEqual(outcome, .itemDeleted(captureID: "cap-a"))
        XCTAssertFalse(exists(recordURL("cap-a")))
        XCTAssertFalse(exists(finalizedAudioURL("cap-a.m4a")), "the audio finalized just before the rejection is removed")
        XCTAssertFalse(exists(inProgressAudioURL("cap-a.m4a")))
    }

    func testAnAudioDiscardThatCannotBeConfirmedKeepsTheRecordForARetryThatFinishesIt() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [.failed(.itemDeleted), .failed(.itemDeleted)]
        let failingFileSystem = FailingIngressFileSystem()
        let (service, _) = try makeGatedService(importer: importer, gateFileSystem: failingFileSystem)
        let record = try audioRecord("cap-a")
        failingFileSystem.failing = [.remove]

        var first: IngressOutcome?
        service.submit(record) { first = $0 }

        XCTAssertEqual(
            first, .keptForRetry(captureID: "cap-a", problem: .rejected(code: ResetIngressGate.audioDiscardIncompleteCode)))
        XCTAssertTrue(exists(recordURL("cap-a")), "the record still names the audio left to remove")
        XCTAssertTrue(exists(finalizedAudioURL("cap-a.m4a")))

        failingFileSystem.failing = []
        var recovery: IngressRecoveryReport?
        service.recover { recovery = $0 }

        XCTAssertEqual(recovery?.entries.map { $0.outcome }, [.itemDeleted(captureID: "cap-a")])
        XCTAssertFalse(exists(recordURL("cap-a")))
        XCTAssertFalse(exists(finalizedAudioURL("cap-a.m4a")))
    }

    func testOtherFailuresAndConfirmationsKeepTheirAudio() throws {
        let importer = FakeForegroundIngressImporter()
        importer.results = [.failed(.notCommitted)]
        let (service, _) = try makeGatedService(importer: importer)
        let record = try audioRecord("cap-a")

        var outcome: IngressOutcome?
        service.submit(record) { outcome = $0 }

        XCTAssertEqual(outcome, .keptForRetry(captureID: "cap-a", problem: .notCommitted))
        XCTAssertTrue(exists(recordURL("cap-a")))
        XCTAssertTrue(exists(finalizedAudioURL("cap-a.m4a")), "audio is discarded only for a rejected, deleted capture")
    }
}
