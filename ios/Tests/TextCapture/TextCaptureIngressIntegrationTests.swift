import XCTest
@testable import OhAnd
@testable import OhAndServices

/// The capture surface over the real foreground ingress service and protected staging store, with only the core
/// import scripted. Nothing here touches a network: a save needs the staging store and the core, never a provider.
final class TextCaptureIngressIntegrationTests: IngressStorageTestCase {
    private var importer: FakeForegroundIngressImporter!
    private var service: ForegroundIngressService!
    private var model: TextCaptureModel!
    private var generatedIDs = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        importer = FakeForegroundIngressImporter()
        service = try makeService(importer: importer)
        model = makeModel(service: service)
    }

    override func tearDown() {
        model = nil
        service = nil
        super.tearDown()
    }

    private func makeModel(service: ForegroundIngressService) -> TextCaptureModel {
        let adapter = ForegroundIngressTextCaptureAdapter(service: service, context: { [unowned self] _ in context })
        return TextCaptureModel(
            ingress: adapter,
            makeCaptureID: { [unowned self] in
                generatedIDs += 1
                return "text-capture-\(generatedIDs)"
            },
            now: { Date(timeIntervalSince1970: 1_790_000_000) })
    }

    private func stagedText(_ captureID: String) throws -> String? {
        let data = try Data(contentsOf: recordURL(captureID))
        return try JSONDecoder().decode(IngressRecord.self, from: data).text
    }

    func testConfirmedSaveReportsSavedAndLeavesNoStagingRecord() throws {
        importer.results = [FakeForegroundIngressImporter.confirmation("text-capture-1", itemID: "item-9")]
        model.updateText("buy oat milk")

        model.save()

        XCTAssertEqual(model.status, .saved(alreadySaved: false))
        XCTAssertEqual(importer.importedRecords.map(\.text), ["buy oat milk"])
        XCTAssertEqual(importer.importedRecords.first?.captureID, "text-capture-1")
        XCTAssertFalse(exists(recordURL("text-capture-1")))
        XCTAssertEqual(model.text, "")
    }

    func testOfflineSaveKeepsTheTextDurablyAndDoesNotClaimItIsSaved() throws {
        importer.results = [.failed(.coreUnavailable)]
        model.updateText("water the plants")

        model.save()

        XCTAssertEqual(model.status, .keptOnDevice)
        XCTAssertEqual(try stagedText("text-capture-1"), "water the plants")
    }

    func testInterruptedSaveIsImportedOnceAfterRelaunchUnderTheSameCaptureID() throws {
        importer.results = [.failed(.notCommitted)]
        model.updateText("renew passport")
        model.save()
        XCTAssertEqual(model.status, .keptOnDevice)

        model = nil
        service = nil
        let relaunchedImporter = FakeForegroundIngressImporter()
        relaunchedImporter.results = [FakeForegroundIngressImporter.confirmation("text-capture-1")]
        let relaunched = try makeService(importer: relaunchedImporter)
        let report = recover(relaunched)

        guard case .saved(let acknowledgment)? = report?.entries.first?.outcome else {
            return XCTFail("expected the staged capture to be imported, got \(String(describing: report))")
        }
        XCTAssertEqual(acknowledgment.captureID, "text-capture-1")
        XCTAssertEqual(relaunchedImporter.importedRecords.map(\.text), ["renew passport"])
        XCTAssertEqual(report?.entries.count, 1)
        XCTAssertFalse(exists(recordURL("text-capture-1")))
    }

    func testCommitThatWasNotConfirmedConvergesOnTheSameItemAfterRelaunch() throws {
        importer.results = [.failed(.commitUnknown)]
        model.updateText("pay rent")
        model.save()
        XCTAssertEqual(model.status, .keptOnDevice)

        model = nil
        service = nil
        let relaunchedImporter = FakeForegroundIngressImporter()
        relaunchedImporter.results = [FakeForegroundIngressImporter.confirmation("text-capture-1", itemID: "item-1", already: true)]
        let relaunched = try makeService(importer: relaunchedImporter)

        guard case .saved(let acknowledgment)? = recover(relaunched)?.entries.first?.outcome else {
            return XCTFail("expected recovery to confirm the capture")
        }
        XCTAssertTrue(acknowledgment.alreadyImported)
        XCTAssertEqual(acknowledgment.itemID, "item-1")
        XCTAssertEqual(relaunchedImporter.importedRecords.count, 1)
    }

    func testStagingFailureKeepsTheTextAndTheRetryIsTheSameRecord() throws {
        model = nil
        service = nil
        let failingFileSystem = FailingIngressFileSystem()
        failingFileSystem.failing = [.write]
        service = try makeService(importer: importer, fileSystem: failingFileSystem)
        model = makeModel(service: service)
        importer.results = [FakeForegroundIngressImporter.confirmation("text-capture-1")]
        model.updateText("book dentist")

        model.save()

        XCTAssertEqual(model.status, .notSaved)
        XCTAssertEqual(model.text, "book dentist", "nothing durable was written, so the text stays")
        XCTAssertTrue(importer.importedRecords.isEmpty, "an unstaged capture is never imported")

        failingFileSystem.failing = []
        model.save()

        XCTAssertEqual(model.status, .saved(alreadySaved: false))
        XCTAssertEqual(importer.importedRecords.map(\.captureID), ["text-capture-1"])
        XCTAssertEqual(importer.importedRecords.map(\.text), ["book dentist"])
    }

    func testRepeatedTapWhileTheImportIsInFlightImportsOneRecord() throws {
        importer.holdsCompletions = true
        importer.results = [FakeForegroundIngressImporter.confirmation("text-capture-1")]
        model.updateText("call mum")

        model.save()
        model.save()
        importer.release()

        XCTAssertEqual(importer.importedRecords.count, 1)
        XCTAssertEqual(model.status, .saved(alreadySaved: false))
    }

    func testAdapterMapsEveryIngressOutcomeHonestly() {
        let acknowledgment = IngressSaveAcknowledgment(
            captureID: "c", itemID: "i", savedAt: "2026-10-08T09:30:02Z", alreadyImported: true,
            stagingCleanedUp: true, audioProtectionFailures: [])
        let adapter = ForegroundIngressTextCaptureAdapter.outcome(for:)

        XCTAssertEqual(adapter(.saved(acknowledgment)), .saved(captureID: "c", itemID: "i", alreadySaved: true))
        XCTAssertEqual(adapter(.keptForRetry(captureID: "c", problem: .commitUnknown)), .keptOnDevice(captureID: "c"))
        XCTAssertEqual(adapter(.notStaged(captureID: "c", failure: .corruptRecord)), .notStaged(captureID: "c"))
        XCTAssertEqual(adapter(.itemDeleted(captureID: "c")), .itemDeleted(captureID: "c"))
    }
}
