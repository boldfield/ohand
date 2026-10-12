import XCTest
@testable import OhAnd
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The entry surface over the real foreground ingress service and protected staging store, with only the core import
/// scripted. A save needs the staging store and the core: no network, provider, management shell or webview.
final class CaptureEntryIngressIntegrationTests: IngressStorageTestCase {
    private var importer: FakeForegroundIngressImporter!
    private var service: ForegroundIngressService!
    private var voice: FakeVoiceController!
    private var reader: ScriptedProgressReader!
    private var model: CaptureEntryModel!
    private var captureEndCount = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        importer = FakeForegroundIngressImporter()
        service = try makeService(importer: importer)
        voice = FakeVoiceController()
        reader = ScriptedProgressReader()
        captureEndCount = 0
        let adapter = ForegroundIngressTextCaptureAdapter(service: service, context: { [unowned self] _ in context })
        model = CaptureEntryModel(
            textIngress: adapter, voice: voice, progressReader: reader, defaultMode: .silent,
            access: { CaptureEntryFixtures.openAccess }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-capture-1" }, now: { Date(timeIntervalSince1970: 1_790_000_000) })
        model.captureDidEnd = { [unowned self] in captureEndCount += 1 }
    }

    override func tearDown() {
        model = nil
        service = nil
        super.tearDown()
    }

    func testAConfirmedSaveIsAcknowledgedWithoutAnyReminderClaim() {
        importer.results = [FakeForegroundIngressImporter.confirmation("text-capture-1", itemID: "item-9")]
        model.textModel.updateText("buy oat milk")

        model.saveText()

        XCTAssertEqual(importer.importedRecords.map(\.text), ["buy oat milk"])
        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-9")
        XCTAssertEqual(
            model.acknowledgmentMessage,
            "Text saved. It has not been processed yet. No reminder has been confirmed.")
        XCTAssertEqual(captureEndCount, 1)
    }

    func testAnOfflineSaveIsKeptDurablyAndNeverCalledSaved() throws {
        importer.results = [.failed(.coreUnavailable)]
        model.textModel.updateText("water the plants")

        model.saveText()

        XCTAssertTrue(model.acknowledgmentMessage?.contains("not confirmed saved") ?? false)
        XCTAssertTrue(exists(recordURL("text-capture-1")), "the text is durable in the staging store")
        XCTAssertTrue(reader.requestedItemIDs.isEmpty)
        XCTAssertEqual(captureEndCount, 1)
    }

    func testAStagingFailureKeepsTheTextAndAcknowledgesNothingSaved() throws {
        let failingFileSystem = FailingIngressFileSystem()
        failingFileSystem.failing = [.write]
        model = nil
        service = nil
        service = try makeService(importer: importer, fileSystem: failingFileSystem)
        let adapter = ForegroundIngressTextCaptureAdapter(service: service, context: { [unowned self] _ in context })
        model = CaptureEntryModel(
            textIngress: adapter, voice: voice, defaultMode: .silent,
            access: { CaptureEntryFixtures.openAccess }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-capture-1" }, now: { Date(timeIntervalSince1970: 1_790_000_000) })
        model.textModel.updateText("book dentist")

        model.saveText()

        XCTAssertEqual(model.acknowledgmentMessage, "Not saved. Your text is still here.")
        XCTAssertEqual(model.textModel.text, "book dentist")
        XCTAssertTrue(importer.importedRecords.isEmpty)
    }

    func testTheCoreStatusReportMapsToAnHonestAcknowledgment() throws {
        importer.results = [FakeForegroundIngressImporter.confirmation("text-capture-1", itemID: "item-9")]
        let report = try JSONDecoder().decode(ItemStatusReport.self, from: Data("""
        {"operation_id": 7, "item_id": "item-9", "save_state": "saved_local", "sync_state": "not_configured",
         "processing_state": "processed", "transcription_state": "not_applicable",
         "processing_job_status": null, "reminder_request_state": "unschedulable",
         "reminder_schedule_state": "not_scheduled", "reminder_delivery_state": null,
         "reminder_acknowledgment_state": null, "unschedulable_reason": "time_in_past"}
        """.utf8))
        reader.result = .status(CaptureItemStatus(report: report, installedDueInstant: nil))
        model.textModel.updateText("call mum yesterday")

        model.saveText()

        XCTAssertEqual(reader.requestedItemIDs, ["item-9"])
        XCTAssertEqual(
            model.acknowledgmentMessage, "Text saved. Reminder not scheduled: that time has already passed.")
    }
}
