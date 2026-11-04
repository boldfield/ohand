import Foundation
import XCTest
import OhAndCoreBridge

/// The process-wide live core handle and result-buffer counts a test starts from, and the judgement of a leak at its end.
/// A core read issued by a test (a lifecycle step, a health or status read) completes on its own queue, so its result
/// buffer is freed a moment after the test body ends; a read started by the previous test can likewise still hold its
/// buffer when the next `setUp` runs. Both sides therefore wait, bounded, instead of reading the counts once. Test files
/// use this type and never read the counts themselves (`ios/scripts/check_test_leak_baseline.py` enforces it).
struct CoreLeakBaseline {
    let handles: Int
    let buffers: Int

    /// The value a test class stores before `setUp` has run.
    static let empty = CoreLeakBaseline(handles: 0, buffers: 0)

    /// Captures the counts once no result buffer is live, or once both counts have stopped changing (two equal reads
    /// a moment apart), waiting at most `timeout` seconds. A buffer or handle still live after that stays in the
    /// baseline, so it is not blamed on the test that follows.
    static func capture(timeout: TimeInterval = 10) -> CoreLeakBaseline {
        var previousHandles = -1
        var previousBuffers = -1
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            let handles = CoreHandle.liveHandleCount
            let buffers = CoreHandle.liveResultBufferCount
            if buffers == 0 || (handles == previousHandles && buffers == previousBuffers) { break }
            previousHandles = handles
            previousBuffers = buffers
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return CoreLeakBaseline(handles: CoreHandle.liveHandleCount, buffers: CoreHandle.liveResultBufferCount)
    }

    /// Asserts, without waiting, that the live handle count is back at the baseline, for a test that has just closed
    /// every core it opened and says so mid-body.
    func assertHandlesReturned(_ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(CoreHandle.liveHandleCount, handles, message, file: file, line: line)
    }

    /// Waits, at most `timeout` seconds, for both counts to return to the baseline, then asserts they are equal to it.
    /// Equality, not "at most": a buffer or handle still live after the wait fails the test at the caller's line.
    func check(timeout: TimeInterval = 10, file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(timeout)
        while CoreHandle.liveHandleCount != handles || CoreHandle.liveResultBufferCount != buffers, Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, handles, "every core handle is closed", file: file, line: line)
        XCTAssertEqual(CoreHandle.liveResultBufferCount, buffers, "no result buffer leaks", file: file, line: line)
    }
}
