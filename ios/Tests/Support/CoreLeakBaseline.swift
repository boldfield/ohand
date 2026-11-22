import Foundation
import XCTest
import OhAndCoreBridge

/// The process-wide live core handle and result-buffer counts a test starts from, and the judgement of a leak at its end.
/// A core read issued by a test (a lifecycle step, a health or status read) completes on its own queue, so its result
/// buffer is freed a moment after the test body ends; a read started by the previous test can likewise still hold its
/// buffer when the next `setUp` runs. Both sides therefore wait, bounded, instead of reading the counts once, and a
/// reading only counts once it has held for a settle interval, so a read that starts just after a match is waited for. Test files
/// use this type and never read the counts themselves (`ios/scripts/check_test_leak_baseline.py` enforces it).
struct CoreLeakBaseline {
    /// A reading of the two process-wide counts.
    struct Counts: Equatable {
        let handles: Int
        let buffers: Int
    }

    /// Where the counts, the clock and the pause between reads come from. The default reads the real counters and runs
    /// the run loop; `CoreLeakBaselineTests` injects counts and a virtual clock so no test sleeps.
    struct Source {
        var read: () -> Counts
        var now: () -> TimeInterval
        var pause: (TimeInterval) -> Void

        static let live = Source(
            read: { Counts(handles: CoreHandle.liveHandleCount, buffers: CoreHandle.liveResultBufferCount) },
            now: { ProcessInfo.processInfo.systemUptime },
            pause: { RunLoop.current.run(mode: .default, before: Date().addingTimeInterval($0)) }
        )
    }

    /// Seconds between two reads.
    static let pollInterval: TimeInterval = 0.01
    /// Seconds a reading must hold unchanged before it is believed.
    static let settleInterval: TimeInterval = 0.1

    let handles: Int
    let buffers: Int

    /// The value a test class stores before `setUp` has run.
    static let empty = CoreLeakBaseline(handles: 0, buffers: 0)

    /// Reads until a reading has held for `settleInterval` (and, when `matching` is given, equals it, and, when `preferring` is given, satisfies it), or until
    /// `timeout` seconds have passed, and returns the last reading. A reading that began before the deadline may finish
    /// settling after it, so a wait ends at most `timeout + settleInterval` seconds after it started: a passing wait
    /// at most that, a wait that never settles `timeout` plus one poll.
    static func settledCounts(
        matching target: Counts? = nil, preferring preferred: ((Counts) -> Bool)? = nil,
        timeout: TimeInterval, source: Source = .live
    ) -> Counts {
        let deadline = source.now() + timeout
        var heldCounts: Counts?
        var heldSince = source.now()
        while true {
            let counts = source.read()
            let now = source.now()
            if let target, counts != target {
                heldCounts = nil
            } else if let preferred, !preferred(counts) {
                heldCounts = nil
            } else if counts != heldCounts {
                heldCounts = counts
                heldSince = now
            }
            if heldCounts != nil, now - heldSince >= settleInterval { return counts }
            if now >= deadline, heldCounts == nil { return counts }
            if now >= deadline + settleInterval { return counts }
            source.pause(pollInterval)
        }
    }

    /// Captures the counts once they have stopped changing for `settleInterval`, waiting at most `timeout` seconds.
    /// A buffer in flight that a later read shows freed is not recorded: the wait prefers a reading with no result buffer
    /// live, however long a read has held a buffer, and only a buffer live through the whole wait is kept. A buffer or
    /// handle still live after the wait stays in the baseline, so it is not blamed on the test that follows.
    static func capture(timeout: TimeInterval = 10, source: Source = .live) -> CoreLeakBaseline {
        let counts = settledCounts(preferring: { $0.buffers == 0 }, timeout: timeout, source: source)
        return CoreLeakBaseline(handles: counts.handles, buffers: counts.buffers)
    }

    /// Asserts, without waiting, that the live handle count is back at the baseline, for a test that has just closed
    /// every core it opened and says so mid-body.
    func assertHandlesReturned(_ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(CoreHandle.liveHandleCount, handles, message, file: file, line: line)
    }

    /// Waits, at most `timeout` seconds (a passing check at most `timeout` plus the settle interval), for both counts to
    /// return to the baseline and hold there for the settle interval, then asserts on the counts it waited for.
    /// Equality, not "at most": a buffer or handle still live after the wait fails the test at the caller's line.
    func check(timeout: TimeInterval = 10, source: Source = .live, file: StaticString = #filePath, line: UInt = #line) {
        let counts = CoreLeakBaseline.settledCounts(
            matching: Counts(handles: handles, buffers: buffers), timeout: timeout, source: source)
        XCTAssertEqual(counts.handles, handles, "every core handle is closed", file: file, line: line)
        XCTAssertEqual(counts.buffers, buffers, "no result buffer leaks", file: file, line: line)
    }
}
