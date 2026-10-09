import XCTest
@testable import OhAndServices

/// Service behavior against a scripted importer: ordering of durable steps, cleanup only after confirmation, recovery
/// after interruption, and the single-writer rule.
final class ForegroundIngressServiceTests: IngressStorageTestCase {
    private var importer: FakeForegroundIngressImporter!

    override func setUpWithError() throws {
        try super.setUpWithError()
        importer = FakeForegroundIngressImporter()
    }

    // MARK: Saved only after confirmation

    func testTextIsStagedBeforeTheImportStartsAndRemovedOnlyAfterConfirmation() throws {
        let service = try makeService(importer: importer)
        var recordExistedAtImport = false
        importer.onImport = { [unowned self] record in recordExistedAtImport = exists(recordURL(record.captureID)) }
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-1")]

        let outcome = submit(service, textRecord("capture-1"))

        XCTAssertTrue(recordExistedAtImport, "the staging record is durable before the core is asked to import")
        guard case .saved(let acknowledgment)? = outcome else { return XCTFail("expected saved, got \(String(describing: outcome))") }
        XCTAssertEqual(acknowledgment.itemID, "item-1")
        XCTAssertTrue(acknowledgment.stagingCleanedUp)
        XCTAssertFalse(exists(recordURL("capture-1")), "cleanup happens after confirmation")
    }

    func testEveryUnconfirmedImportKeepsTheRecordAndNeverReportsSaved() throws {
        let failures: [IngressImportFailure] = [
            .notCommitted, .commitUnknown, .coreUnavailable, .rejected(code: "ingress_unknown_route"), .conflictingReuse,
        ]
        for (index, failure) in failures.enumerated() {
            let captureID = "capture-keep-\(index)"
            let service = try makeService(importer: importer)
            importer.results = [.failed(failure)]

            let outcome = submit(service, textRecord(captureID))

            guard case .keptForRetry(let reportedID, _)? = outcome else {
                return XCTFail("\(failure) must keep the record, got \(String(describing: outcome))")
            }
            XCTAssertEqual(reportedID, captureID)
            XCTAssertTrue(exists(recordURL(captureID)), "\(failure) keeps the staged input")
        }
    }

    func testAConfirmationForAnotherCaptureIDIsNotTreatedAsSaved() throws {
        let service = try makeService(importer: importer)
        importer.results = [FakeForegroundIngressImporter.confirmation("someone-else")]

        let outcome = submit(service, textRecord("capture-1"))

        guard case .keptForRetry? = outcome else { return XCTFail("got \(String(describing: outcome))") }
        XCTAssertTrue(exists(recordURL("capture-1")))
    }

    func testDeletedItemDiscardsTheRecordAndIsNotReportedAsSaved() throws {
        let service = try makeService(importer: importer)
        importer.results = [.failed(.itemDeleted)]

        XCTAssertEqual(submit(service, textRecord("capture-1")), .itemDeleted(captureID: "capture-1"))
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testFailedCleanupAfterConfirmationIsStillSavedAndTheLeftoverRecordIsHarmless() throws {
        let failingFileSystem = FailingIngressFileSystem()
        let service = try makeService(importer: importer, fileSystem: failingFileSystem)
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-1")]
        importer.onImport = { _ in failingFileSystem.failing = [.remove] }

        guard case .saved(let acknowledgment)? = submit(service, textRecord("capture-1")) else { return XCTFail("expected saved") }
        XCTAssertFalse(acknowledgment.stagingCleanedUp)
        XCTAssertTrue(exists(recordURL("capture-1")))

        failingFileSystem.failing = []
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-1", already: true)]
        let report = recover(service)
        XCTAssertEqual(importer.importedRecords.count, 2, "the leftover record is re-imported, which is idempotent")
        XCTAssertFalse(exists(recordURL("capture-1")))
        guard case .saved(let again)? = report?.entries.first?.outcome else { return XCTFail("expected saved") }
        XCTAssertTrue(again.alreadyImported)
    }

    // MARK: Nothing durable, nothing acknowledged

    func testStagingWriteFailureReportsNotStagedAndNeverCallsTheCore() throws {
        let failingFileSystem = FailingIngressFileSystem()
        failingFileSystem.failing = [.write]
        let service = try makeService(importer: importer, fileSystem: failingFileSystem)

        guard case .notStaged(let captureID, .storage)? = submit(service, textRecord("capture-1")) else {
            return XCTFail("expected notStaged")
        }
        XCTAssertEqual(captureID, "capture-1")
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testInvalidRecordsAreRefusedBeforeAnythingIsWritten() throws {
        let service = try makeService(importer: importer)
        let unsafeID = textRecord("../escape")
        let empty = textRecord("capture-empty", text: "")
        let neither = IngressRecord(captureID: "capture-none", text: nil, audio: nil, context: context)

        for record in [unsafeID, empty, neither] {
            guard case .notStaged(_, .invalidRecord)? = submit(service, record) else {
                return XCTFail("\(record.captureID) must be refused")
            }
        }
        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: layout.directory(for: .ingressStagingRecords).path), [])
    }

    func testDifferentContentUnderAnExistingCaptureIDIsRefusedAndTheOriginalIsKept() throws {
        let service = try makeService(importer: importer)
        importer.results = [.failed(.notCommitted)]
        _ = submit(service, textRecord("capture-1", text: "original words"))

        guard case .notStaged(_, .conflictingStagingRecord)? = submit(service, textRecord("capture-1", text: "other words")) else {
            return XCTFail("expected conflict")
        }
        XCTAssertEqual(importer.importedRecords.count, 1)
        let stored = try JSONDecoder().decode(IngressRecord.self, from: Data(contentsOf: recordURL("capture-1")))
        XCTAssertEqual(stored.text, "original words")
    }

    // MARK: Repeated handoff

    func testRepeatedHandoffOfTheSameRecordConvergesOnOneRecordAndOneImportedCapture() throws {
        let service = try makeService(importer: importer)
        importer.results = [.failed(.commitUnknown), FakeForegroundIngressImporter.confirmation("capture-1")]

        _ = submit(service, textRecord("capture-1"))
        let second = submit(service, textRecord("capture-1"))

        guard case .saved? = second else { return XCTFail("expected saved, got \(String(describing: second))") }
        XCTAssertEqual(Set(importer.importedRecords.map { $0.captureID }), ["capture-1"])
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testConcurrentSubmissionOfTheSameCaptureIDSharesOneImport() throws {
        let service = try makeService(importer: importer)
        importer.holdsCompletions = true
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-1")]
        var outcomes: [IngressOutcome] = []

        service.submit(textRecord("capture-1")) { outcomes.append($0) }
        service.submit(textRecord("capture-1")) { outcomes.append($0) }
        XCTAssertEqual(importer.importedRecords.count, 1, "the second handoff joined the running import")
        importer.release()

        XCTAssertEqual(outcomes.count, 2)
        XCTAssertEqual(outcomes[0], outcomes[1])
    }

    // MARK: Process interruption and recovery

    func testInterruptionAfterStagingIsRecoveredByTheNextService() throws {
        var beforeInterruption: ForegroundIngressService? = try makeService(importer: importer)
        importer.results = [.failed(.coreUnavailable)]
        _ = submit(try XCTUnwrap(beforeInterruption), textRecord("capture-1"))
        beforeInterruption = nil

        let relaunchedImporter = FakeForegroundIngressImporter()
        relaunchedImporter.results = [FakeForegroundIngressImporter.confirmation("capture-1")]
        let relaunched = try makeService(importer: relaunchedImporter)

        let report = recover(relaunched)
        XCTAssertEqual(report?.entries.count, 1)
        guard case .saved? = report?.entries.first?.outcome else { return XCTFail("expected recovered save") }
        XCTAssertFalse(exists(recordURL("capture-1")))
    }

    func testRecoveryReportsIncompleteWritesAndUnclaimedAudioWithoutDeletingThem() throws {
        let service = try makeService(importer: importer)
        let incomplete = layout.directory(for: .ingressStagingRecords).appendingPathComponent("capture-9.json.incomplete")
        try Data("partial".utf8).write(to: incomplete)
        try Data("orphan audio".utf8).write(to: inProgressAudioURL("orphan.m4a"))

        let report = recover(service)

        XCTAssertEqual(report?.incompleteWrites, ["capture-9.json.incomplete"])
        XCTAssertEqual(report?.unclaimedInProgressAudio, ["orphan.m4a"])
        XCTAssertTrue(exists(incomplete))
        XCTAssertTrue(exists(inProgressAudioURL("orphan.m4a")))
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    func testCorruptAndUnreadableRecordsAreSurfacedNotDiscarded() throws {
        let service = try makeService(importer: importer)
        try Data("not json".utf8).write(to: recordURL("capture-corrupt"))

        let report = recover(service)

        guard case .keptForRetry(_, .recordUnreadable(.corruptRecord))? = report?.entries.first?.outcome else {
            return XCTFail("got \(String(describing: report))")
        }
        XCTAssertTrue(exists(recordURL("capture-corrupt")))
        XCTAssertEqual(report?.unclaimedInProgressAudio, [], "orphans are not reported while a record is unreadable")
    }

    func testUnlistableStagingDirectoryIsReportedAsUnknownNotEmpty() throws {
        let service = try makeService(importer: importer)
        try FileManager.default.removeItem(at: layout.directory(for: .ingressStagingRecords))

        let report = recover(service)

        XCTAssertNotNil(report?.listingFailure)
        XCTAssertTrue(report?.entries.isEmpty ?? false)
    }

    // MARK: Audio

    func testAudioIsMovedToFinalizedStoreBeforeImportAndTheReferenceStaysValidAfterCleanup() throws {
        let service = try makeService(importer: importer)
        let record = try audioRecord("capture-audio", bytes: Data("synthetic recording".utf8))
        var importedReference: String?
        var finalizedAtImport = false
        importer.onImport = { [unowned self] imported in
            importedReference = imported.coreAudioReference
            finalizedAtImport = exists(finalizedAudioURL("capture-audio.m4a")) && !exists(inProgressAudioURL("capture-audio.m4a"))
        }
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-audio")]

        guard case .saved? = submit(service, record) else { return XCTFail("expected saved") }

        XCTAssertTrue(finalizedAtImport)
        XCTAssertEqual(importedReference, "FinalizedAudio/capture-audio.m4a")
        XCTAssertFalse(exists(recordURL("capture-audio")))
        let referenced = layout.rootDirectory.appendingPathComponent(try XCTUnwrap(importedReference))
        XCTAssertEqual(try Data(contentsOf: referenced), Data("synthetic recording".utf8), "cleanup left the referenced audio intact")
    }

    func testAudioMoveFailureKeepsSourceAndRecordAndNeverCallsTheCore() throws {
        let failingFileSystem = FailingIngressFileSystem()
        let service = try makeService(importer: importer, fileSystem: failingFileSystem)
        let record = try audioRecord("capture-audio")
        failingFileSystem.failing = [.move]

        guard case .keptForRetry(_, .sourceUnavailable(.storage))? = submit(service, record) else { return XCTFail("expected kept") }

        XCTAssertTrue(importer.importedRecords.isEmpty)
        XCTAssertTrue(exists(inProgressAudioURL("capture-audio.m4a")))
        XCTAssertTrue(exists(recordURL("capture-audio")))

        failingFileSystem.failing = []
        importer.results = [FakeForegroundIngressImporter.confirmation("capture-audio")]
        guard case .saved? = recover(service)?.entries.first?.outcome else { return XCTFail("expected recovery to save") }
        XCTAssertTrue(exists(finalizedAudioURL("capture-audio.m4a")))
    }

    func testInterruptionBetweenAudioMoveAndImportIsRecoveredWithoutLosingTheAudio() throws {
        let service = try makeService(importer: importer)
        let record = try audioRecord("capture-audio")
        importer.results = [.failed(.coreUnavailable)]
        _ = submit(service, record)
        XCTAssertTrue(exists(finalizedAudioURL("capture-audio.m4a")))
        XCTAssertFalse(exists(inProgressAudioURL("capture-audio.m4a")))

        importer.results = [FakeForegroundIngressImporter.confirmation("capture-audio")]
        guard case .saved? = recover(service)?.entries.first?.outcome else { return XCTFail("expected saved") }
        XCTAssertTrue(exists(finalizedAudioURL("capture-audio.m4a")))
    }

    func testMissingOrEmptyAudioIsAnHonestFailureNotASave() throws {
        let service = try makeService(importer: importer)
        let missing = IngressRecord(
            captureID: "capture-missing", text: nil,
            audio: IngressAudioHandoff(inProgressFileName: "gone.m4a", finalizedFileName: "gone.m4a"), context: context)
        let empty = try audioRecord("capture-empty", bytes: Data())

        guard case .keptForRetry(_, .sourceUnavailable(.audioSourceMissing))? = submit(service, missing) else { return XCTFail("missing") }
        guard case .keptForRetry(_, .sourceUnavailable(.audioSourceEmpty))? = submit(service, empty) else { return XCTFail("empty") }
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    // MARK: One writer

    func testASecondServiceCannotWriteWhileTheFirstIsActive() throws {
        let first = try makeService(importer: importer)

        XCTAssertThrowsError(try makeService(importer: FakeForegroundIngressImporter())) { error in
            XCTAssertEqual(error as? IngressWriterLock.AcquisitionError, .anotherWriterActive)
        }
        withExtendedLifetime(first) {}
    }

    func testTheWriterLockIsReleasedWhenTheServiceIsDiscarded() throws {
        var first: ForegroundIngressService? = try makeService(importer: importer)
        XCTAssertNotNil(first)
        first = nil
        XCTAssertNoThrow(try makeService(importer: FakeForegroundIngressImporter()))
    }
}
