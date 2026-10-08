import Foundation
import XCTest
import OhandCoreC
@testable import OhAndCoreBridge

/// Exercises the real Rust core through the Swift wrapper on the simulator. Events arrive on
/// the Rust worker thread and are hopped to the main queue, which is where XCTest runs.
final class CoreHandleTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUp() {
        super.setUp()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDown() {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: Lifecycle

    func testRepeatedOpenAndCloseReturnsEveryCounterToItsBaseline() throws {
        for _ in 0..<25 {
            let core = try CoreHandle()
            XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles + 1)
            core.close()
            XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)
        }
    }

    func testCloseIsIdempotentAndLaterCallsAreRejected() throws {
        let core = try CoreHandle()
        core.close()
        core.close()
        XCTAssertThrowsError(try core.startStoreCheck(operationID: 1)) { error in
            XCTAssertEqual(error as? CoreFailure, CoreFailure.handleClosed)
        }
        XCTAssertThrowsError(try core.cancel())
        XCTAssertThrowsError(try core.setEventHandler { _ in })
    }

    func testDroppingTheHandleClosesTheCore() throws {
        autoreleasepool {
            let core = try? CoreHandle()
            XCTAssertNotNil(core)
            XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles + 1)
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)
    }

    func testFileBackedStoreOpensAndReopens() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-core-handle-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let storePath = directory.appendingPathComponent("core.sqlite").path
        for _ in 0..<2 {
            let core = try CoreHandle(path: storePath)
            let received = expectation(description: "store check")
            try core.setEventHandler { event in
                XCTAssertEqual((try? event.decode(StoreCheckReport.self))?.captureCount, 0)
                received.fulfill()
            }
            try core.startStoreCheck(operationID: 7)
            wait(for: [received], timeout: 20)
            core.close()
        }
    }

    // MARK: Invalid handles and input

    func testInvalidHandlesAreRejectedByEveryExportWithoutCrashing() throws {
        let core = try CoreHandle()
        let closedHandle = core.handleIdentifier
        core.close()
        let staleAndForeign: [OhandCoreHandle] = [0, closedHandle, closedHandle + 1_000_000, UInt64.max]
        for handle in staleAndForeign {
            for result in [
                ohand_core_cancel(handle),
                ohand_core_close(handle),
                ohand_core_close(handle),
                ohand_core_start_store_check(handle, 1),
                ohand_core_set_event_callback(handle, nil, nil),
            ] {
                XCTAssertThrowsError(try consumeCoreResult(result)) { error in
                    XCTAssertEqual((error as? CoreFailure)?.code, "invalid_handle")
                    XCTAssertEqual((error as? CoreFailure)?.errorClass, .permanent)
                }
            }
        }
    }

    func testInvalidOpenInputIsRejected() throws {
        XCTAssertThrowsError(try CoreHandle(path: "")) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_path")
        }
        let oversizedPath = String(repeating: "a", count: Int(OHAND_CORE_MAX_PATH_BYTES) + 1)
        XCTAssertThrowsError(try CoreHandle(path: oversizedPath)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "request_too_large")
        }

        var handle: OhandCoreHandle = 99
        let invalidUTF8: [UInt8] = [0xFF, 0xFE, 0xFD]
        let rejectedBytes = invalidUTF8.withUnsafeBufferPointer {
            ohand_core_open($0.baseAddress, $0.count, &handle)
        }
        XCTAssertThrowsError(try consumeCoreResult(rejectedBytes)) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "invalid_utf8")
        }
        XCTAssertEqual(handle, 0, "a failed open clears the output handle")

        XCTAssertThrowsError(try consumeCoreResult(ohand_core_open(nil, 3, &handle))) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "null_argument")
        }
        XCTAssertThrowsError(try consumeCoreResult(ohand_core_open(nil, 0, nil))) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "null_argument")
        }

        let unwritableDirectoryPath = "/nonexistent-ohand-directory/core.sqlite"
        XCTAssertThrowsError(try CoreHandle(path: unwritableDirectoryPath)) { error in
            XCTAssertEqual((error as? CoreFailure)?.errorClass, .permanent)
        }
    }

    func testFreeingAResultTwiceOrNullIsHarmless() throws {
        var result = ohand_core_close(0)
        XCTAssertNotEqual(result.status, 0)
        XCTAssertNotNil(result.data)
        ohand_core_result_free(&result)
        XCTAssertNil(result.data)
        ohand_core_result_free(&result)
        ohand_core_result_free(nil)
    }

    func testFailuresCarryFixedContentFreeTextAndNeverTheirInput() throws {
        let secretLookingPath = "/nonexistent-ohand-directory/synthetic-marker-4242.sqlite"
        XCTAssertThrowsError(try CoreHandle(path: secretLookingPath)) { error in
            let failure = error as? CoreFailure
            XCTAssertFalse(failure?.message.contains("synthetic-marker-4242") ?? true)
        }
    }

    // MARK: Background callbacks and threading

    func testStoreCheckRunsInTheBackgroundAndIsDeliveredOnTheMainThread() throws {
        let core = try CoreHandle()
        defer { core.close() }
        let received = expectation(description: "store check delivered")
        var delivered: [CoreEvent] = []
        try core.setEventHandler { event in
            XCTAssertTrue(Thread.isMainThread, "UI-facing delivery is on the main thread")
            delivered.append(event)
            received.fulfill()
        }
        try core.startStoreCheck(operationID: 42)
        wait(for: [received], timeout: 20)

        XCTAssertEqual(delivered.count, 1)
        let report = try delivered[0].decode(StoreCheckReport.self)
        XCTAssertEqual(delivered[0].operationID, 42)
        XCTAssertEqual(report.operationID, 42)
        XCTAssertEqual(report.captureCount, 0)
        XCTAssertGreaterThan(report.schemaVersion, 0)
    }

    func testEventsKeepSubmissionOrder() throws {
        let core = try CoreHandle()
        defer { core.close() }
        let operationCount = 40
        let received = expectation(description: "all events delivered")
        received.expectedFulfillmentCount = operationCount
        var delivered: [UInt64] = []
        try core.setEventHandler { event in
            delivered.append(event.operationID)
            received.fulfill()
        }
        for operationID in 1...UInt64(operationCount) {
            try core.startStoreCheck(operationID: operationID)
        }
        wait(for: [received], timeout: 30)
        XCTAssertEqual(delivered, Array(1...UInt64(operationCount)))
    }

    func testTheRawCallbackRunsOnTheCoreWorkerThreadNeverTheMainThread() throws {
        let core = try CoreHandle()
        defer { core.close() }
        let recorder = RawThreadRecorder()
        let context = Unmanaged.passUnretained(recorder).toOpaque()
        let registered = ohand_core_set_event_callback(core.handleIdentifier, rawThreadRecorderCallback, context)
        XCTAssertNoThrow(try consumeCoreResult(registered))
        try core.startStoreCheck(operationID: 5)
        wait(for: [recorder.finished], timeout: 20)
        XCTAssertEqual(recorder.threadName, "ohand-core-worker")
        XCTAssertFalse(recorder.ranOnMainThread)
        XCTAssertEqual(recorder.status, UInt32(OHAND_CORE_STATUS_OK))
    }

    func testEventsWithoutAHandlerAreDiscardedAndClearingTheHandlerStopsDelivery() throws {
        let core = try CoreHandle()
        defer { core.close() }
        var deliveredCount = 0
        try core.setEventHandler { _ in deliveredCount += 1 }
        try core.setEventHandler(nil)
        try core.startStoreCheck(operationID: 1)
        pumpMainQueue(seconds: 0.5)
        XCTAssertEqual(deliveredCount, 0)
    }

    // MARK: Cancellation and teardown

    func testNoHandlerRunsAfterCancelReturnsOnTheMainQueue() throws {
        let core = try CoreHandle()
        defer { core.close() }
        var cancelReturned = false
        var deliveredBeforeCancel = 0
        var deliveredAfterCancel = 0
        try core.setEventHandler { _ in
            if cancelReturned { deliveredAfterCancel += 1 } else { deliveredBeforeCancel += 1 }
        }
        for operationID in 1...60 {
            try core.startStoreCheck(operationID: UInt64(operationID))
        }
        try core.cancel()
        cancelReturned = true
        pumpMainQueue(seconds: 1.0)
        XCTAssertEqual(deliveredAfterCancel, 0, "events already queued for the main queue are dropped by cancel")
        XCTAssertLessThanOrEqual(deliveredBeforeCancel, 60)

        XCTAssertThrowsError(try core.startStoreCheck(operationID: 99)) { error in
            XCTAssertEqual((error as? CoreFailure)?.errorClass, .cancelled)
        }
        XCTAssertNoThrow(try core.cancel(), "cancel is idempotent")
    }

    func testNoHandlerStartsAfterCancelReturnsOnABackgroundThread() throws {
        let core = try CoreHandle()
        defer { core.close() }
        let flagLock = NSLock()
        var settled = false
        var deliveredAfterSettled = 0
        try core.setEventHandler { _ in
            flagLock.lock()
            if settled { deliveredAfterSettled += 1 }
            flagLock.unlock()
        }
        for operationID in 1...60 {
            try core.startStoreCheck(operationID: UInt64(operationID))
        }
        let cancelled = expectation(description: "cancelled off the main thread")
        DispatchQueue.global().async {
            do {
                try core.cancel()
            } catch {
                XCTFail("cancel failed: \(error)")
            }
            cancelled.fulfill()
        }
        wait(for: [cancelled], timeout: 20)
        // Handlers run on the main queue, so any that began before cancel returned has finished
        // by the time this test code runs on it again.
        flagLock.lock()
        settled = true
        flagLock.unlock()
        pumpMainQueue(seconds: 1.0)
        XCTAssertEqual(deliveredAfterSettled, 0)
    }

    func testReplacingTheHandlerNeverDeliversToTheOldHandlerAfterwards() throws {
        let core = try CoreHandle()
        defer { core.close() }
        var oldHandlerCalls = 0
        var newHandlerCalls = 0
        var replaced = false
        try core.setEventHandler { _ in
            XCTAssertFalse(replaced, "the replaced handler was invoked after the replacement returned")
            oldHandlerCalls += 1
        }
        for operationID in 1...20 {
            try core.startStoreCheck(operationID: UInt64(operationID))
        }
        let newHandlerSawEvent = expectation(description: "new handler delivery")
        newHandlerSawEvent.assertForOverFulfill = false
        try core.setEventHandler { _ in
            newHandlerCalls += 1
            newHandlerSawEvent.fulfill()
        }
        replaced = true
        try core.startStoreCheck(operationID: 100)
        wait(for: [newHandlerSawEvent], timeout: 20)
        pumpMainQueue(seconds: 0.3)
        XCTAssertGreaterThanOrEqual(newHandlerCalls, 1)
        XCTAssertLessThanOrEqual(oldHandlerCalls, 20)
    }

    func testCloseStopsDeliveryAndReleasesTheHandlerContext() throws {
        weak var weakCapture: HandlerCapture?
        do {
            let core = try CoreHandle()
            let capture = HandlerCapture()
            weakCapture = capture
            var deliveredAfterClose = 0
            var closed = false
            try core.setEventHandler { _ in
                _ = capture
                if closed { deliveredAfterClose += 1 }
            }
            for operationID in 1...30 {
                try core.startStoreCheck(operationID: UInt64(operationID))
            }
            core.close()
            closed = true
            pumpMainQueue(seconds: 0.7)
            XCTAssertEqual(deliveredAfterClose, 0)
        }
        pumpMainQueue(seconds: 0.2)
        XCTAssertNil(weakCapture, "the handler and everything it captured are released after close")
    }

    func testAHandlerMayCancelItsOwnCore() throws {
        let core = try CoreHandle()
        defer { core.close() }
        var deliveredCount = 0
        var cancelOutcome: Result<Void, Error>?
        let firstEvent = expectation(description: "first event")
        try core.setEventHandler { _ in
            deliveredCount += 1
            if deliveredCount == 1 {
                cancelOutcome = Result { try core.cancel() }
                firstEvent.fulfill()
            }
        }
        for operationID in 1...30 {
            try core.startStoreCheck(operationID: UInt64(operationID))
        }
        wait(for: [firstEvent], timeout: 20)
        pumpMainQueue(seconds: 0.7)
        XCTAssertNoThrow(try cancelOutcome?.get())
        XCTAssertEqual(deliveredCount, 1, "cancel from the handler suppresses every later event")
    }

    func testAHandlerMayCloseItsOwnCore() throws {
        let core = try CoreHandle()
        var deliveredCount = 0
        let firstEvent = expectation(description: "first event")
        try core.setEventHandler { _ in
            deliveredCount += 1
            core.close()
            firstEvent.fulfill()
        }
        for operationID in 1...10 {
            try core.startStoreCheck(operationID: UInt64(operationID))
        }
        wait(for: [firstEvent], timeout: 20)
        pumpMainQueue(seconds: 0.5)
        XCTAssertEqual(deliveredCount, 1)
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles)
    }

    func testRawCloseAndHandlerReplacementFromInsideARawCallbackAreRejected() throws {
        let core = try CoreHandle()
        defer { core.close() }
        let probe = ReentrancyProbe(handle: core.handleIdentifier)
        let context = Unmanaged.passUnretained(probe).toOpaque()
        XCTAssertNoThrow(try consumeCoreResult(
            ohand_core_set_event_callback(core.handleIdentifier, reentrancyProbeCallback, context)
        ))
        try core.startStoreCheck(operationID: 1)
        wait(for: [probe.finished], timeout: 20)
        XCTAssertEqual(probe.closeCode, "reentrant_call")
        XCTAssertEqual(probe.replaceCode, "reentrant_call")
        XCTAssertNil(probe.cancelCode, "cancel from inside a callback is allowed")
    }

    func testConcurrentOpenSubmitCancelAndCloseNeverCrashOrLeak() throws {
        let group = DispatchGroup()
        let workerCount = 4
        for workerIndex in 0..<workerCount {
            group.enter()
            DispatchQueue.global().async {
                defer { group.leave() }
                for iteration in 0..<6 {
                    guard let core = try? CoreHandle() else {
                        XCTFail("open failed")
                        return
                    }
                    try? core.setEventHandler { _ in }
                    for operationID in 0..<8 {
                        try? core.startStoreCheck(operationID: UInt64(operationID))
                    }
                    if (workerIndex + iteration) % 2 == 0 { try? core.cancel() }
                    core.close()
                    _ = try? core.startStoreCheck(operationID: 1)
                }
            }
        }
        let finished = expectation(description: "stress finished")
        group.notify(queue: .main) { finished.fulfill() }
        wait(for: [finished], timeout: 120)
        pumpMainQueue(seconds: 0.3)
    }

    private func pumpMainQueue(seconds: TimeInterval) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.02))
        }
    }
}

private final class HandlerCapture {}

private final class RawThreadRecorder {
    let finished = XCTestExpectation(description: "raw callback ran")
    var threadName = ""
    var ranOnMainThread = true
    var status: UInt32 = UInt32.max
}

private func rawThreadRecorderCallback(
    _ context: UnsafeMutableRawPointer?,
    _ operationID: UInt64,
    _ status: UInt32,
    _ data: UnsafePointer<UInt8>?,
    _ length: Int
) {
    guard let context else { return }
    let recorder = Unmanaged<RawThreadRecorder>.fromOpaque(context).takeUnretainedValue()
    recorder.threadName = Thread.current.name ?? ""
    recorder.ranOnMainThread = Thread.isMainThread
    recorder.status = status
    recorder.finished.fulfill()
}

private final class ReentrancyProbe {
    let handle: OhandCoreHandle
    let finished = XCTestExpectation(description: "reentrancy probe ran")
    var closeCode: String?
    var replaceCode: String?
    var cancelCode: String?

    init(handle: OhandCoreHandle) {
        self.handle = handle
    }
}

private func failureCode(of result: OhandCoreResult) -> String? {
    do {
        _ = try consumeCoreResult(result)
        return nil
    } catch {
        return (error as? CoreFailure)?.code
    }
}

private func reentrancyProbeCallback(
    _ context: UnsafeMutableRawPointer?,
    _ operationID: UInt64,
    _ status: UInt32,
    _ data: UnsafePointer<UInt8>?,
    _ length: Int
) {
    guard let context else { return }
    let probe = Unmanaged<ReentrancyProbe>.fromOpaque(context).takeUnretainedValue()
    probe.closeCode = failureCode(of: ohand_core_close(probe.handle))
    probe.replaceCode = failureCode(of: ohand_core_set_event_callback(probe.handle, nil, nil))
    probe.cancelCode = failureCode(of: ohand_core_cancel(probe.handle))
    probe.finished.fulfill()
}
