import Foundation
import XCTest
import OhandCoreC
@testable import OhAndCoreBridge

/// Saves and reads real captures and statuses through `CoreHandle` against the real Rust core
/// on the simulator. Outcomes are events delivered on the main queue, where XCTest runs.
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

    func testSavedCaptureIsReadBackWithIndependentInitialStatuses() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        let capture = makeCapture(id: "capture-1")

        let acknowledgment = try session.save(capture).decode(SaveCaptureAcknowledgment.self)
        XCTAssertFalse(acknowledgment.alreadySaved)
        XCTAssertEqual(acknowledgment.capture, capture)
        XCTAssertEqual(acknowledgment.itemID, "capture-1")

        let readout = try session.read("capture-1").decode(CaptureReadout.self)
        XCTAssertEqual(readout.capture, capture)

        let status = try session.status(acknowledgment.itemID).decode(ItemStatusReport.self)
        XCTAssertEqual(status.saveState, "saved_local")
        XCTAssertEqual(status.syncState, "not_configured")
        XCTAssertEqual(status.processingState, "unprocessed")
        XCTAssertEqual(status.transcriptionState, "not_applicable")
        XCTAssertNil(status.processingJobStatus)
        XCTAssertNil(status.reminderRequestState, "no reminder exists, so no reminder status is invented")
        XCTAssertNil(status.reminderScheduleState)
    }

    func testAudioOnlyCaptureReportsAudioPendingTranscription() throws {
        let session = try Session(path: storePath)
        defer { session.close() }
        var capture = makeCapture(id: "capture-audio", text: nil)
        capture.audioReference = "staging/capture-audio.m4a"
        _ = try session.save(capture).decode(SaveCaptureAcknowledgment.self)
        let status = try session.status("capture-audio").decode(ItemStatusReport.self)
        XCTAssertEqual(status.transcriptionState, "audio_pending")
        XCTAssertEqual(status.saveState, "saved_local")
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

    func testCaptureAndStatusSurviveClosingAndReopeningTheStore() throws {
        let capture = makeCapture(id: "capture-restart", text: "still here after restart")
        let first = try Session(path: storePath)
        let acknowledgment = try first.save(capture).decode(SaveCaptureAcknowledgment.self)
        first.close()
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)

        let second = try Session(path: storePath)
        defer { second.close() }
        XCTAssertEqual(try second.read("capture-restart").decode(CaptureReadout.self).capture, capture)
        let status = try second.status(acknowledgment.itemID).decode(ItemStatusReport.self)
        XCTAssertEqual(status.saveState, "saved_local")

        let retry = try second.save(capture).decode(SaveCaptureAcknowledgment.self)
        XCTAssertTrue(retry.alreadySaved, "re-delivery after a restart converges on the stored capture")
        XCTAssertEqual(retry.itemID, acknowledgment.itemID)
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
