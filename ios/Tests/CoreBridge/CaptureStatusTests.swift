import Foundation
import SQLite3
import XCTest
import OhandCoreC
@testable import OhAndCoreBridge

/// Saves and reads real captures and statuses through `CoreHandle` against the real Rust core
/// on the simulator. Outcomes are events delivered on the main queue, where XCTest runs.
///
/// Saving writes only the capture. Items are created by the foreground import (C02a), so the
/// status tests stand in for it by inserting item rows into the closed store file directly.
final class CaptureStatusTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var storeDirectory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-capture-status-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: storeDirectory)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private var storePath: String { storeDirectory.appendingPathComponent("core.sqlite").path }

    private func makeCapture(id: String, text: String? = "buy oat milk") -> CaptureRecord {
        CaptureRecord(
            captureID: id,
            text: text,
            captureInstant: "2026-10-08T09:30:00Z",
            timezoneID: "UTC",
            utcOffsetMinutes: 0,
            locale: "en_US",
            calendar: "gregorian",
            itemScope: "personal",
            routeID: "route-default",
            entryLocked: false,
            createdAt: "2026-10-08T09:30:01Z"
        )
    }

    /// Runs SQL against the store file while no core has it open (a stand-in for the import).
    private func seed(_ statements: [String]) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(storePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
        }
    }

    private func itemInsert(itemID: String, captureID: String, transcription: String = "not_applicable") -> String {
        "INSERT INTO items (item_id, capture_id, revision, lifecycle_state, save_state, sync_state, "
            + "processing_state, transcription_state, created_at, updated_at) VALUES "
            + "('\(itemID)', '\(captureID)', 0, 'active', 'saved_local', 'not_configured', 'unprocessed', "
            + "'\(transcription)', '2026-10-08T09:30:01Z', '2026-10-08T09:30:01Z')"
    }

    /// Opens a core over `path` and records its events in arrival order.
    private final class Session {
        let core: CoreHandle
        private(set) var events: [CoreEvent] = []
        private var nextOperationID: UInt64 = 1

        init(path: String) throws {
            core = try CoreHandle(path: path)
            try core.setEventHandler { [unowned self] event in
                XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
                self.events.append(event)
            }
        }

        deinit { core.close() }

        func close() { core.close() }

        /// Starts one operation and returns its outcome once the event arrives.
        func run(_ start: (UInt64) throws -> Void, timeout: TimeInterval = 20) throws -> CoreEvent {
            let operationID = nextOperationID
            nextOperationID += 1
            try start(operationID)
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if let event = events.first(where: { $0.operationID == operationID }) { return event }
                RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
            }
            throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no event for the operation")
        }

        func save(_ capture: CaptureRecord) throws -> CoreEvent {
            try run { try core.startSaveCapture(operationID: $0, capture: capture) }
        }

        func read(_ captureID: String) throws -> CoreEvent {
            try run { try core.startGetCapture(operationID: $0, captureID: captureID) }
        }

        func status(_ itemID: String) throws -> CoreEvent {
            try run { try core.startItemStatus(operationID: $0, itemID: itemID) }
        }
    }

    private func failure(of event: CoreEvent, file: StaticString = #filePath, line: UInt = #line) -> CoreFailure? {
        guard case .failure(let failure) = event.outcome else {
            XCTFail("expected a failure outcome", file: file, line: line)
            return nil
        }
        return failure
    }

    // MARK: Save and read

    func testSavedCaptureIsReadBack() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        let capture = makeCapture(id: "capture-1")

        let acknowledgment = try session.save(capture).decode(SaveCaptureAcknowledgment.self)
        XCTAssertFalse(acknowledgment.alreadySaved)
        XCTAssertEqual(acknowledgment.capture, capture)

        let readout = try session.read("capture-1").decode(CaptureReadout.self)
        XCTAssertEqual(readout.capture, capture)

        let status = try session.status("capture-1")
        XCTAssertEqual(failure(of: status)?.code, "not_found", "saving a capture creates no item; the import does")
    }

    func testStatusesOfASavedCaptureAreReadIndependently() throws {
        let first = try Session(path: storePath)
        _ = try first.save(makeCapture(id: "capture-status", text: "call the dentist tomorrow"))
        var audioOnly = makeCapture(id: "capture-audio", text: nil)
        audioOnly.audioReference = "staging/capture-audio.m4a"
        _ = try first.save(audioOnly)
        first.close()

        try seed([
            itemInsert(itemID: "7d1c5a1e-0000-4000-8000-000000000001", captureID: "capture-status"),
            itemInsert(itemID: "7d1c5a1e-0000-4000-8000-000000000002", captureID: "capture-audio",
                       transcription: "audio_pending"),
            "UPDATE items SET processing_state = 'uninterpreted' "
                + "WHERE item_id = '7d1c5a1e-0000-4000-8000-000000000001'",
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, unschedulable_reason, created_at, updated_at) VALUES "
                + "('reminder-1', '7d1c5a1e-0000-4000-8000-000000000001', 'unschedulable', 'not_scheduled', "
                + "'unknown', 'not_acknowledged', 'permission_denied', '2026-10-08T09:30:02Z', "
                + "'2026-10-08T09:30:02Z')",
        ])

        let second = try Session(path: storePath)
        defer { second.close() }
        let status = try second.status("7d1c5a1e-0000-4000-8000-000000000001").decode(ItemStatusReport.self)
        XCTAssertEqual(status.saveState, "saved_local")
        XCTAssertEqual(status.syncState, "not_configured")
        XCTAssertEqual(status.processingState, "uninterpreted")
        XCTAssertEqual(status.transcriptionState, "not_applicable")
        XCTAssertNil(status.processingJobStatus)
        XCTAssertEqual(status.reminderRequestState, "unschedulable")
        XCTAssertEqual(status.reminderScheduleState, "not_scheduled")
        XCTAssertEqual(status.reminderDeliveryState, "unknown")
        XCTAssertEqual(status.reminderAcknowledgmentState, "not_acknowledged")
        XCTAssertEqual(status.unschedulableReason, "permission_denied")

        let audio = try second.status("7d1c5a1e-0000-4000-8000-000000000002").decode(ItemStatusReport.self)
        XCTAssertEqual(audio.transcriptionState, "audio_pending")
        XCTAssertNil(audio.reminderRequestState, "no reminder exists, so no reminder status is invented")
        XCTAssertNil(audio.reminderScheduleState)
    }

    func testUnicodeTextWithInteriorNulRoundTripsUnchanged() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        let words = "caf\u{e9} \u{1F469}\u{200D}\u{1F4BB} \u{5E9}\u{5DC}\u{5D5}\u{5DD}\0tail"
        _ = try session.save(makeCapture(id: "capture-unicode", text: words))
        let readout = try session.read("capture-unicode").decode(CaptureReadout.self)
        XCTAssertEqual(readout.capture.text, words)
    }

    // MARK: Restart

    func testCaptureSurvivesClosingAndReopeningTheStore() throws {
        let capture = makeCapture(id: "capture-restart", text: "still here after restart")
        let first = try Session(path: storePath)
        _ = try first.save(capture).decode(SaveCaptureAcknowledgment.self)
        first.close()
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)

        let second = try Session(path: storePath)
        defer { second.close() }
        XCTAssertEqual(try second.read("capture-restart").decode(CaptureReadout.self).capture, capture)

        let retry = try second.save(capture).decode(SaveCaptureAcknowledgment.self)
        XCTAssertTrue(retry.alreadySaved, "re-delivery after a restart converges on the stored capture")
    }

    // MARK: Duplicate IDs and normalized failures

    func testIdenticalResaveConvergesAndConflictingReuseKeepsTheOriginal() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        let original = makeCapture(id: "capture-dup", text: "original words")
        XCTAssertFalse(try session.save(original).decode(SaveCaptureAcknowledgment.self).alreadySaved)
        XCTAssertTrue(try session.save(original).decode(SaveCaptureAcknowledgment.self).alreadySaved)

        var changedText = original
        changedText.text = "different words"
        var changedTimestamp = original
        changedTimestamp.createdAt = "2026-10-08T09:31:00Z"
        for conflicting in [changedText, changedTimestamp] {
            let event = try session.save(conflicting)
            let rejection = failure(of: event)
            XCTAssertEqual(rejection?.code, "capture_conflict")
            XCTAssertEqual(rejection?.errorClass, .permanent)
            XCTAssertThrowsError(try event.decode(SaveCaptureAcknowledgment.self),
                                 "a failed save is never a durable-success acknowledgment")
        }

        let readout = try session.read("capture-dup").decode(CaptureReadout.self)
        XCTAssertEqual(readout.capture, original, "the stored capture is untouched by the rejected saves")
    }

    func testStorageFailureAcknowledgesNothingAndStoresNothing() throws {
        let session = try Session(path: storePath)
        defer { session.close() }

        // Another connection holds the write lock past the store's busy timeout.
        var blocker: OpaquePointer?
        XCTAssertEqual(sqlite3_open(storePath, &blocker), SQLITE_OK)
        defer { sqlite3_close(blocker) }
        XCTAssertEqual(sqlite3_exec(blocker, "BEGIN IMMEDIATE", nil, nil, nil), SQLITE_OK)

        let capture = makeCapture(id: "capture-blocked", text: "must not be acknowledged")
        let event = try session.save(capture)
        XCTAssertEqual(failure(of: event)?.code, "store_busy")
        XCTAssertEqual(failure(of: event)?.errorClass, .transient)
        XCTAssertThrowsError(try event.decode(SaveCaptureAcknowledgment.self))
        XCTAssertEqual(sqlite3_exec(blocker, "ROLLBACK", nil, nil, nil), SQLITE_OK)

        XCTAssertEqual(failure(of: try session.read("capture-blocked"))?.code, "not_found")
        let retry = try session.save(capture).decode(SaveCaptureAcknowledgment.self)
        XCTAssertFalse(retry.alreadySaved)
    }

    func testUnknownRecordsAreNotFoundWithoutEchoingTheRequest() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        for event in [try session.read("synthetic-marker-capture-9"), try session.status("synthetic-marker-item-9")] {
            let rejection = failure(of: event)
            XCTAssertEqual(rejection?.code, "not_found")
            XCTAssertEqual(rejection?.errorClass, .permanent)
            XCTAssertFalse(rejection?.message.contains("synthetic-marker") ?? true)
        }
    }

    func testInvalidCapturesAreRefusedBeforeAnythingIsQueued() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        var withoutContent = makeCapture(id: "capture-invalid", text: nil)
        withoutContent.audioReference = nil
        var unknownScope = makeCapture(id: "capture-invalid")
        unknownScope.itemScope = "shared"
        var emptyRoute = makeCapture(id: "capture-invalid")
        emptyRoute.routeID = ""
        for invalid in [withoutContent, unknownScope, emptyRoute] {
            XCTAssertThrowsError(try session.core.startSaveCapture(operationID: 900, capture: invalid)) { error in
                XCTAssertEqual((error as? CoreFailure)?.code, "invalid_capture")
                XCTAssertEqual((error as? CoreFailure)?.errorClass, .permanent)
            }
        }
        XCTAssertThrowsError(try session.core.startGetCapture(operationID: 901, captureID: "")) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request")
        }
        XCTAssertThrowsError(try session.core.startItemStatus(operationID: 902, itemID: "")) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_request")
        }
        let oversizedIdentifier = String(repeating: "a", count: Int(OHAND_CORE_MAX_CAPTURE_REQUEST_BYTES) + 1)
        XCTAssertThrowsError(try session.core.startGetCapture(operationID: 903, captureID: oversizedIdentifier)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "request_too_large")
        }

        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        XCTAssertTrue(session.events.isEmpty, "refused requests produce no events")
        XCTAssertThrowsError(try session.read("capture-invalid").decode(CaptureReadout.self), "and store nothing")
    }

    func testCapturesAreRefusedAfterCancelAndClose() throws {
        let core = try CoreHandle(path: storePath)
        try core.cancel()
        XCTAssertThrowsError(try core.startSaveCapture(operationID: 1, capture: makeCapture(id: "capture-c"))) { error in
            XCTAssertEqual((error as? CoreFailure)?.errorClass, .cancelled)
        }
        XCTAssertThrowsError(try core.startGetCapture(operationID: 2, captureID: "capture-c")) { error in
            XCTAssertEqual((error as? CoreFailure)?.errorClass, .cancelled)
        }
        core.close()
        XCTAssertThrowsError(try core.startSaveCapture(operationID: 3, capture: makeCapture(id: "capture-c"))) { error in
            XCTAssertEqual(error as? CoreFailure, CoreFailure.handleClosed)
        }
        XCTAssertThrowsError(try core.startItemStatus(operationID: 4, itemID: "capture-c")) { error in
            XCTAssertEqual(error as? CoreFailure, CoreFailure.handleClosed)
        }
    }

    func testRawCaptureExportsRejectStaleHandlesWithoutCrashing() throws {
        let core = try CoreHandle()
        let closedHandle = core.handleIdentifier
        core.close()
        let request = Array("{}".utf8)
        let handles: [OhandCoreHandle] = [0, closedHandle, UInt64.max]
        for handle in handles {
            let results = [
                request.withUnsafeBufferPointer { ohand_core_start_save_capture(handle, 1, $0.baseAddress, $0.count) },
                request.withUnsafeBufferPointer { ohand_core_start_get_capture(handle, 2, $0.baseAddress, $0.count) },
                request.withUnsafeBufferPointer { ohand_core_start_item_status(handle, 3, $0.baseAddress, $0.count) },
            ]
            for result in results {
                XCTAssertThrowsError(try consumeCoreResult(result)) { error in
                    XCTAssertEqual((error as? CoreFailure)?.code, "invalid_handle")
                }
            }
        }
    }
}
