import XCTest

/// `CoreLeakBaseline` with injected counts and a virtual clock: the schedule says what the counters read at each
/// virtual second, and `pause` advances the clock, so nothing sleeps and no real core is opened.
final class CoreLeakBaselineTests: XCTestCase {
    typealias Counts = CoreLeakBaseline.Counts

    private final class VirtualCounters {
        var time: TimeInterval = 0
        let schedule: (TimeInterval) -> Counts
        init(_ schedule: @escaping (TimeInterval) -> Counts) { self.schedule = schedule }

        var source: CoreLeakBaseline.Source {
            CoreLeakBaseline.Source(
                read: { self.schedule(self.time) },
                now: { self.time },
                pause: { self.time += $0 }
            )
        }
    }

    private let clean = Counts(handles: 0, buffers: 0)

    func testABufferThatAppearsJustAfterTheCountsMatchAndIsFreedWithinTheSettleIntervalPasses() {
        let counters = VirtualCounters { time in
            (0.05..<0.08).contains(time) ? Counts(handles: 0, buffers: 1) : Counts(handles: 0, buffers: 0)
        }
        let settled = CoreLeakBaseline.settledCounts(matching: clean, timeout: 10, source: counters.source)
        XCTAssertEqual(settled, clean)
        XCTAssertGreaterThanOrEqual(counters.time, 0.08 + CoreLeakBaseline.settleInterval - 0.001, "the match was held after the buffer was freed")
        CoreLeakBaseline.empty.check(source: VirtualCounters { time in
            (0.05..<0.08).contains(time) ? Counts(handles: 0, buffers: 1) : Counts(handles: 0, buffers: 0)
        }.source)
    }

    func testAMatchIsNotBelievedBeforeItHasHeldForTheSettleInterval() {
        let counters = VirtualCounters { _ in Counts(handles: 0, buffers: 0) }
        _ = CoreLeakBaseline.settledCounts(matching: clean, timeout: 10, source: counters.source)
        XCTAssertGreaterThanOrEqual(counters.time, CoreLeakBaseline.settleInterval)
        XCTAssertLessThan(counters.time, CoreLeakBaseline.settleInterval + 0.05)
    }

    func testABufferThatIsNeverFreedStaysDifferentFromTheBaselineAndTheWaitIsBounded() {
        let counters = VirtualCounters { _ in Counts(handles: 0, buffers: 1) }
        let settled = CoreLeakBaseline.settledCounts(matching: clean, timeout: 10, source: counters.source)
        XCTAssertEqual(settled, Counts(handles: 0, buffers: 1))
        XCTAssertLessThanOrEqual(counters.time, 10 + CoreLeakBaseline.pollInterval + 0.001)
    }

    func testAHandleThatIsNeverClosedStaysDifferentFromTheBaselineAndTheWaitIsBounded() {
        let counters = VirtualCounters { _ in Counts(handles: 1, buffers: 0) }
        let settled = CoreLeakBaseline.settledCounts(matching: clean, timeout: 10, source: counters.source)
        XCTAssertEqual(settled, Counts(handles: 1, buffers: 0))
        XCTAssertLessThanOrEqual(counters.time, 10 + CoreLeakBaseline.pollInterval + 0.001)
    }

    func testAPassingWaitEndsNoLaterThanTheTimeoutPlusTheSettleInterval() {
        // The counts match only 50 ms before the deadline and then keep matching.
        let counters = VirtualCounters { time in time < 9.95 ? Counts(handles: 0, buffers: 1) : Counts(handles: 0, buffers: 0) }
        let settled = CoreLeakBaseline.settledCounts(matching: clean, timeout: 10, source: counters.source)
        XCTAssertEqual(settled, clean)
        XCTAssertLessThanOrEqual(counters.time, 10 + CoreLeakBaseline.settleInterval + 0.001)
    }

    func testCaptureDoesNotRecordABufferInFlightThatALaterReadShowsFreed() {
        let counters = VirtualCounters { time in time < 0.05 ? Counts(handles: 0, buffers: 1) : Counts(handles: 0, buffers: 0) }
        let baseline = CoreLeakBaseline.capture(source: counters.source)
        XCTAssertEqual(baseline.buffers, 0)
        XCTAssertEqual(baseline.handles, 0)
    }

    func testCaptureKeepsABufferThatStaysLiveThroughItsWaitInTheBaseline() {
        let counters = VirtualCounters { _ in Counts(handles: 2, buffers: 1) }
        let baseline = CoreLeakBaseline.capture(timeout: 1, source: counters.source)
        XCTAssertEqual(baseline.buffers, 1)
        XCTAssertEqual(baseline.handles, 2)
        XCTAssertLessThanOrEqual(counters.time, 1 + CoreLeakBaseline.settleInterval + 0.001)
    }

    func testCaptureOfAStableCleanStateTakesOnlyTheSettleInterval() {
        let counters = VirtualCounters { _ in Counts(handles: 0, buffers: 0) }
        _ = CoreLeakBaseline.capture(source: counters.source)
        XCTAssertLessThan(counters.time, CoreLeakBaseline.settleInterval + 0.05)
    }

    func testCaptureDoesNotRecordABufferThatIsInFlightLongerThanTheSettleIntervalButFreedWithinTheTimeout() {
        let counters = VirtualCounters { time in time < 3 ? Counts(handles: 0, buffers: 1) : Counts(handles: 0, buffers: 0) }
        let baseline = CoreLeakBaseline.capture(source: counters.source)
        XCTAssertEqual(baseline.buffers, 0)
    }

    // MARK: check() itself

    /// Runs `body`, which must record failures, and returns the file, line and message of each.
    private func recordedFailures(_ body: () -> Void) -> [(file: String, line: Int, message: String)] {
        var failures: [(file: String, line: Int, message: String)] = []
        let options = XCTExpectedFailure.Options()
        options.issueMatcher = { issue in
            failures.append((
                issue.sourceCodeContext.location?.fileURL.path ?? "",
                issue.sourceCodeContext.location?.lineNumber ?? 0,
                issue.compactDescription))
            return true
        }
        XCTExpectFailure("check() is expected to fail", options: options, failingBlock: body)
        return failures
    }

    func testCheckFailsAtTheCallersLineForABufferThatIsNeverFreed() {
        let counters = VirtualCounters { _ in Counts(handles: 0, buffers: 1) }
        let expectedLine = #line + 2
        let failures = recordedFailures {
            CoreLeakBaseline.empty.check(source: counters.source, file: #filePath, line: UInt(expectedLine))
        }
        XCTAssertEqual(failures.count, 1)
        XCTAssertEqual(failures.first?.line, expectedLine)
        XCTAssertEqual(failures.first.map { URL(fileURLWithPath: $0.file).lastPathComponent }, "CoreLeakBaselineTests.swift")
        XCTAssertTrue(failures.first?.message.contains("no result buffer leaks") ?? false)
        XCTAssertLessThanOrEqual(counters.time, 10 + CoreLeakBaseline.pollInterval + 0.001)
    }

    func testCheckFailsForAHandleThatIsNeverClosed() {
        let counters = VirtualCounters { _ in Counts(handles: 1, buffers: 0) }
        let failures = recordedFailures { CoreLeakBaseline.empty.check(source: counters.source) }
        XCTAssertEqual(failures.count, 1)
        XCTAssertTrue(failures.first?.message.contains("every core handle is closed") ?? false)
    }

    func testCheckFailsWhenACountStaysBelowTheBaselineNotOnlyAbove() {
        let counters = VirtualCounters { _ in Counts(handles: 0, buffers: 0) }
        let baseline = CoreLeakBaseline(handles: 1, buffers: 1)
        let failures = recordedFailures { baseline.check(source: counters.source) }
        XCTAssertEqual(failures.count, 2, "equality: a handle and a buffer below the baseline both fail")
    }

    func testCheckPassesWhenTheCountsEqualANonZeroBaseline() {
        let counters = VirtualCounters { _ in Counts(handles: 2, buffers: 1) }
        CoreLeakBaseline(handles: 2, buffers: 1).check(source: counters.source)
    }
}
