import XCTest
@testable import OhAndCoreBridge

/// Tests for OhAndCoreHandle initialization, cancellation, and lifetime management.
class CoreHandleTests: XCTestCase {

    /// Test handle initialization creates a valid handle.
    func testHandleInitialization() {
        let handle = OhAndCoreHandle()
        XCTAssertNotNil(handle.isCancelled(), "Handle should be valid after initialization")
        XCTAssertFalse(handle.isCancelled() ?? true, "Handle should not be cancelled on creation")
    }

    /// Test handle cancellation works correctly.
    func testHandleCancellation() {
        let handle = OhAndCoreHandle()
        XCTAssertFalse(handle.isCancelled() ?? true, "Should not be cancelled initially")

        let result = handle.cancel()
        XCTAssertTrue(result, "Cancel should succeed")
        XCTAssertTrue(handle.isCancelled() ?? false, "Should be cancelled after cancel()")
    }

    /// Test repeated cancellation is safe.
    func testRepeatedCancellation() {
        let handle = OhAndCoreHandle()

        let result1 = handle.cancel()
        XCTAssertTrue(result1, "First cancel should succeed")

        let result2 = handle.cancel()
        XCTAssertTrue(result2, "Second cancel should also succeed")

        XCTAssertTrue(handle.isCancelled() ?? false, "Should remain cancelled")
    }

    /// Test handle destruction is safe.
    func testHandleDestruction() {
        let handle = OhAndCoreHandle()
        XCTAssertNotNil(handle.isCancelled(), "Handle should be valid before destroy")

        handle.destroy()

        XCTAssertNil(handle.isCancelled(), "Handle should be invalid after destroy")
    }

    /// Test repeated destruction is safe.
    func testRepeatedDestruction() {
        let handle = OhAndCoreHandle()

        handle.destroy()
        let result1 = handle.isCancelled()
        XCTAssertNil(result1, "Handle should be invalid after first destroy")

        handle.destroy()
        let result2 = handle.isCancelled()
        XCTAssertNil(result2, "Handle should remain invalid after second destroy")
    }

    /// Test destruction happens automatically on deinit.
    func testAutomaticDestructionOnDeinit() {
        autoreleasepool {
            let handle = OhAndCoreHandle()
            XCTAssertNotNil(handle.isCancelled(), "Handle should be valid")
        }

        XCTAssertTrue(true, "Should not crash when handle goes out of scope")
    }

    /// Test cancellation prevents further operations.
    func testCancellationPreventsOperations() {
        let handle = OhAndCoreHandle()

        handle.cancel()
        XCTAssertTrue(handle.isCancelled() ?? false, "Should be cancelled")

        let cancelled_again = handle.cancel()
        XCTAssertTrue(cancelled_again, "Further cancels should succeed")
    }

    /// Test concurrent access to handle is thread-safe.
    func testConcurrentHandleAccess() {
        let handle = OhAndCoreHandle()
        let expectation = XCTestExpectation(description: "Concurrent operations complete")
        var errors: [Error] = []

        let queue = DispatchQueue(label: "concurrent", attributes: .concurrent)

        for _ in 0..<10 {
            queue.async {
                let _ = handle.isCancelled()
            }
        }

        queue.async(flags: .barrier) {
            handle.cancel()
        }

        for _ in 0..<10 {
            queue.async {
                let _ = handle.isCancelled()
            }
        }

        queue.async(flags: .barrier) {
            expectation.fulfill()
        }

        wait(for: [expectation], timeout: 2.0)
        XCTAssertEqual(errors.count, 0, "Should have no errors during concurrent access")
    }

    /// Test handle state consistency across lifecycle.
    func testHandleStateConsistency() {
        let handle = OhAndCoreHandle()

        XCTAssertFalse(handle.isCancelled() ?? true)

        handle.cancel()
        XCTAssertTrue(handle.isCancelled() ?? false)

        handle.cancel()
        XCTAssertTrue(handle.isCancelled() ?? false)

        handle.destroy()
        XCTAssertNil(handle.isCancelled())
    }
}

/// Tests for callback delivery and threading behavior.
class CoreCallbackTests: XCTestCase {

    /// Test that multiple handles can be created and destroyed independently.
    func testMultipleHandles() {
        let handle1 = OhAndCoreHandle()
        let handle2 = OhAndCoreHandle()

        XCTAssertNotNil(handle1.isCancelled(), "Handle 1 should be valid")
        XCTAssertNotNil(handle2.isCancelled(), "Handle 2 should be valid")

        handle1.cancel()
        XCTAssertTrue(handle1.isCancelled() ?? false, "Handle 1 should be cancelled")
        XCTAssertFalse(handle2.isCancelled() ?? true, "Handle 2 should not be cancelled")
    }

    /// Test handle operations under memory pressure.
    func testHandleUnderMemoryPressure() {
        var handles: [OhAndCoreHandle] = []

        for _ in 0..<100 {
            let handle = OhAndCoreHandle()
            handles.append(handle)
        }

        for (i, handle) in handles.enumerated() {
            if i % 2 == 0 {
                handle.cancel()
            }
        }

        for (i, handle) in handles.enumerated() {
            if i % 2 == 0 {
                XCTAssertTrue(handle.isCancelled() ?? false)
            } else {
                XCTAssertFalse(handle.isCancelled() ?? true)
            }
        }

        handles.removeAll()
    }

    /// Test callback registration and invocation on main thread.
    func testCallbackRegistration() {
        let handle = OhAndCoreHandle()
        let expectation = XCTestExpectation(description: "Callback invoked")
        var callbackExecuted = false

        let callback: CoreCallback = { _ in
            callbackExecuted = true
            expectation.fulfill()
        }

        let registered = handle.registerCallback(callback)
        XCTAssertTrue(registered, "Callback registration should succeed")

        let invokeResult = handle.invokeCallback()
        XCTAssertEqual(invokeResult, 0, "Callback should be invoked (result 0)")

        wait(for: [expectation], timeout: 1.0)
        XCTAssertTrue(callbackExecuted, "Callback should have been executed")
    }

    /// Test that callbacks are not invoked after cancellation.
    func testCallbackPreventedAfterCancellation() {
        let handle = OhAndCoreHandle()
        var callbackExecuted = false

        let callback: CoreCallback = { _ in
            callbackExecuted = true
        }

        let registered = handle.registerCallback(callback)
        XCTAssertTrue(registered, "Callback registration should succeed")

        handle.cancel()
        XCTAssertTrue(handle.isCancelled() ?? false, "Handle should be cancelled")

        let invokeResult = handle.invokeCallback()
        XCTAssertEqual(invokeResult, 1, "Callback should not be invoked when cancelled (result 1)")
        XCTAssertFalse(callbackExecuted, "Callback should not be executed after cancellation")
    }

    /// Test error classification.
    func testErrorClassification() {
        XCTAssertEqual(OhAndCoreHandle.classifyError("unauthorized access"), .unauthorized)
        XCTAssertEqual(OhAndCoreHandle.classifyError("permission denied"), .unauthorized)
        XCTAssertEqual(OhAndCoreHandle.classifyError("operation cancelled"), .cancelled)
        XCTAssertEqual(OhAndCoreHandle.classifyError("network timeout"), .transient)
        XCTAssertEqual(OhAndCoreHandle.classifyError("temporary failure"), .transient)
        XCTAssertEqual(OhAndCoreHandle.classifyError("unsupported feature"), .unsupported)
        XCTAssertEqual(OhAndCoreHandle.classifyError("database error"), .permanent)
        XCTAssertEqual(OhAndCoreHandle.classifyError("unknown error"), .permanent)
    }

    /// Test callback delivery on background thread still delivers to core.
    func testCallbackDeliveryThreadSafety() {
        let handle = OhAndCoreHandle()
        let expectation = XCTestExpectation(description: "Background thread callback")

        let callback: CoreCallback = { _ in
            expectation.fulfill()
        }

        let registered = handle.registerCallback(callback)
        XCTAssertTrue(registered, "Callback registration should succeed")

        DispatchQueue.global().async {
            let invokeResult = handle.invokeCallback()
            XCTAssertEqual(invokeResult, 0, "Callback should be invoked from background thread")
        }

        wait(for: [expectation], timeout: 1.0)
    }

    /// Test that invocation with no registered callback returns correct status.
    func testCallbackInvocationWithoutRegistration() {
        let handle = OhAndCoreHandle()

        let invokeResult = handle.invokeCallback()
        XCTAssertEqual(invokeResult, 2, "Should return 2 when no callback is registered")
    }
}
