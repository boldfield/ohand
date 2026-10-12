import SQLite3
import XCTest
@testable import OhAnd
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Reads item status from the real Rust core with `CoreHandle.startItemStatus`, the way a composition root must. The
/// importer owns the writer core's single event handler, so reads go through a second handle on the same store file.
final class CoreStatusProgressReader: CaptureProgressReading {
    private let core: CoreHandle
    private var pendingByOperationID: [UInt64: (CaptureProgressReadResult) -> Void] = [:]
    private var nextOperationID: UInt64 = 1
    private(set) var requestedItemIDs: [String] = []

    init(path: String) throws {
        core = try CoreHandle(path: path)
        try core.setEventHandler { [weak self] event in self?.deliver(event) }
    }

    func readStatus(itemID: String, completion: @escaping (CaptureProgressReadResult) -> Void) {
        requestedItemIDs.append(itemID)
        let operationID = nextOperationID
        nextOperationID += 1
        pendingByOperationID[operationID] = completion
        do {
            try core.startItemStatus(operationID: operationID, itemID: itemID)
        } catch {
            pendingByOperationID[operationID] = nil
            completion(.unavailable)
        }
    }

    func close() {
        core.close()
    }

    private func deliver(_ event: CoreEvent) {
        guard let completion = pendingByOperationID.removeValue(forKey: event.operationID) else { return }
        guard case .success = event.outcome, let report = try? event.decode(ItemStatusReport.self) else {
            completion(.unavailable)
            return
        }
        completion(.status(CaptureItemStatus(report: report, installedDueInstant: nil)))
    }
}

/// The entry surface over the real Rust core on the simulator: a save goes through the real foreground ingress
/// service and core import, and the acknowledgment's progress comes from a real status read of the created item. Only
/// the recorder is synthetic (microphone capture is device evidence). Nothing here asserts a reminder: an item that has
/// just been saved has no confirmed reminder, and the acknowledgment must say so.
final class CaptureEntryCoreIntegrationTests: IngressStorageTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var writerCore: CoreHandle!
    private var service: ForegroundIngressService!
    private var reader: CoreStatusProgressReader!
    private var voice: FakeVoiceController!
    private var model: CaptureEntryModel!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount

        let seedCore = try CoreHandle(path: layout.databaseURL.path)
        seedCore.close()
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        let statement = "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
            + "VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')"
        XCTAssertEqual(sqlite3_exec(database, statement, nil, nil, nil), SQLITE_OK)
        sqlite3_close(database)

        writerCore = try CoreHandle(path: layout.databaseURL.path)
        service = try ForegroundIngressService(
            layout: layout, importer: try CoreIngressImporter(core: writerCore))
        reader = try CoreStatusProgressReader(path: layout.databaseURL.path)
        voice = FakeVoiceController()
        let adapter = ForegroundIngressTextCaptureAdapter(service: service, context: { [unowned self] _ in context })
        model = CaptureEntryModel(
            textIngress: adapter, voice: voice, progressReader: reader, defaultMode: .silent,
            access: { CaptureEntryFixtures.openAccess }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-core-1" }, now: { Date(timeIntervalSince1970: 1_790_000_000) })
    }

    override func tearDown() {
        model = nil
        service = nil
        reader?.close()
        reader = nil
        writerCore?.close()
        writerCore = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func waitUntil(_ description: String, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(30)
        while !condition() && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        XCTAssertTrue(condition(), "timed out waiting for \(description)")
    }

    func testASavedTextIsReadBackFromTheRealCoreAndAcknowledgedWithoutAReminderClaim() throws {
        model.textModel.updateText("buy oat milk")

        model.saveText()
        waitUntil("the status of the saved item") { model.acknowledgment?.progress != nil }

        let itemID = try XCTUnwrap(model.acknowledgment?.savedItemID, "the core created an item")
        XCTAssertEqual(reader.requestedItemIDs, [itemID], "the status was read from the core for the saved item")
        XCTAssertEqual(
            model.acknowledgment?.progress,
            CaptureProgress(transcription: .notApplicable, interpretation: .pending, reminder: .notConfirmed))
        XCTAssertEqual(
            model.acknowledgmentMessage,
            "Text saved. It has not been processed yet. No reminder has been confirmed.")
        XCTAssertFalse(exists(recordURL("text-core-1")), "the staging record is removed once the core committed")
    }

    func testASavedRecordingIsReadBackFromTheRealCoreAsPendingTranscriptionWithNoReminder() throws {
        let record = try audioRecord("voice-core-1")
        var outcome: IngressOutcome?
        service.submit(record) { outcome = $0 }
        waitUntil("the audio import") { outcome != nil }
        guard case .saved(let saved)? = outcome else { return XCTFail("expected saved, got \(String(describing: outcome))") }

        voice.finish(with: FakeVoiceController.saved(itemID: saved.itemID))
        waitUntil("the status of the saved item") { model.acknowledgment?.progress != nil }

        XCTAssertEqual(reader.requestedItemIDs, [saved.itemID])
        XCTAssertEqual(model.acknowledgment?.progress?.transcription, .pending)
        XCTAssertEqual(model.acknowledgment?.progress?.reminder, .notConfirmed)
        XCTAssertEqual(
            model.acknowledgmentMessage,
            "Audio saved. It has not been transcribed yet. No reminder has been confirmed.")
    }
}
