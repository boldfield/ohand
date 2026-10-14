import Foundation
import UIKit
import XCTest
@testable import OhAndServices

// MARK: - Test doubles

private func instant(_ text: String) -> Date {
    ISO8601DateFormatter().date(from: text)!
}

/// Clocks the test moves by hand, and a timer that holds every sleeper until the test releases them.
final class ManualTime: @unchecked Sendable {
    private let lock = NSLock()
    private var wall: Date
    private var monotonic: TimeInterval = 1_000
    private var zone: TimeZone
    private var sleepers: [CheckedContinuation<Void, Never>] = []
    private var delays: [TimeInterval] = []

    init(wall: Date, zone: TimeZone = TimeZone(identifier: "UTC")!) {
        self.wall = wall
        self.zone = zone
    }

    var sources: TimeSources {
        TimeSources(
            wallClock: { [self] in locked { wall } },
            monotonicSeconds: { [self] in locked { monotonic } },
            timeZone: { [self] in locked { zone } },
            sleep: { [self] seconds in
                await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                    lock.lock()
                    delays.append(seconds)
                    sleepers.append(continuation)
                    lock.unlock()
                }
            })
    }

    var now: Date { locked { wall } }
    var sleeperCount: Int { locked { sleepers.count } }
    var requestedDelays: [TimeInterval] { locked { delays } }

    /// Real time passes: both clocks move together.
    func pass(_ seconds: TimeInterval) {
        locked {
            wall = wall.addingTimeInterval(seconds)
            monotonic += seconds
        }
    }

    /// Someone sets the wall clock: it moves and the monotonic clock does not.
    func jumpWallClock(by seconds: TimeInterval) {
        locked { wall = wall.addingTimeInterval(seconds) }
    }

    func travel(to newZone: TimeZone) {
        locked { zone = newZone }
    }

    /// Lets every held timer fire, including those a newer callback has since superseded.
    func releaseTimers() {
        let waiting: [CheckedContinuation<Void, Never>] = locked {
            let taken = sleepers
            sleepers = []
            return taken
        }
        for continuation in waiting {
            continuation.resume()
        }
    }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
}

private final class EventLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [TimeChangeEvent] = []

    var events: [TimeChangeEvent] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(_ event: TimeChangeEvent) {
        lock.lock()
        recorded.append(event)
        lock.unlock()
    }
}

func timeChangeWaitUntil(timeout: TimeInterval = 10, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}

private let newYork = TimeZone(identifier: "America/New_York")!
private let losAngeles = TimeZone(identifier: "America/Los_Angeles")!

// MARK: - Coordinator

final class TimeChangeCoordinatorTests: XCTestCase {
    private let center = NotificationCenter()

    private func makeCoordinator(
        _ time: ManualTime,
        log: EventLog,
        configuration: TimeChangeConfiguration = TimeChangeConfiguration()
    ) -> TimeChangeCoordinator {
        TimeChangeCoordinator(
            notificationCenter: center, sources: time.sources, configuration: configuration,
            onChange: { log.record($0) })
    }

    private func postSignals(zone: Int = 0, clock: Int = 0, significant: Int = 0) {
        for _ in 0..<zone { center.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        for _ in 0..<clock { center.post(name: .NSSystemClockDidChange, object: nil) }
        for _ in 0..<significant { center.post(name: UIApplication.significantTimeChangeNotification, object: nil) }
    }

    /// Waits until `count` timers are held, then lets them all fire.
    private func settle(_ time: ManualTime, signals count: Int) async {
        let held = await timeChangeWaitUntil { time.sleeperCount == count }
        XCTAssertTrue(held, "every callback started a timer")
        time.releaseTimers()
    }

    func testARepeatedBurstOfCallbacksForOneZoneChangeRequestsReconciliationOnce() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.travel(to: losAngeles)
        postSignals(zone: 3, clock: 2, significant: 1)
        await settle(time, signals: 6)

        let reported = await timeChangeWaitUntil { coordinator.reportedChangeCount == 1 }
        XCTAssertTrue(reported)
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(log.events.count, 1, "six callbacks, one request")
        XCTAssertTrue(log.events[0].reasons.contains(.timeZoneChanged))
        XCTAssertEqual(log.events[0].foldedSignals, 6)
        XCTAssertEqual(coordinator.evaluationsPerformed, 1, "only the last callback's timer evaluated")
    }

    func testCallbacksThatFindNothingChangedRequestNothing() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.pass(30)
        postSignals(clock: 1, significant: 1)
        await settle(time, signals: 2)

        let evaluated = await timeChangeWaitUntil { coordinator.evaluationsPerformed == 1 }
        XCTAssertTrue(evaluated)
        XCTAssertTrue(log.events.isEmpty, "a midnight or synchronization callback with the same zone and a continuous clock changes nothing")
    }

    func testAZoneChangeUndoneWithinTheQuietWindowRequestsNothing() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"), zone: newYork)
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.travel(to: losAngeles)
        postSignals(zone: 1)
        time.travel(to: newYork)
        postSignals(zone: 1)
        await settle(time, signals: 2)

        let evaluated = await timeChangeWaitUntil { coordinator.evaluationsPerformed == 1 }
        XCTAssertTrue(evaluated)
        XCTAssertTrue(log.events.isEmpty)
    }

    func testAClockJumpForwardIsReportedOnce() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.jumpWallClock(by: 3 * 86_400)
        postSignals(clock: 1, significant: 1)
        await settle(time, signals: 2)

        let reported = await timeChangeWaitUntil { log.events.count == 1 }
        XCTAssertTrue(reported)
        XCTAssertEqual(log.events[0].reasons, [.clockJumpedForward])
        XCTAssertEqual(log.events[0].observedAt, instant("2026-10-17T12:00:00Z"))
    }

    func testAClockJumpBackwardIsReportedOnce() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.pass(600)
        time.jumpWallClock(by: -7_200)
        postSignals(clock: 1)
        await settle(time, signals: 1)

        let reported = await timeChangeWaitUntil { log.events.count == 1 }
        XCTAssertTrue(reported)
        XCTAssertEqual(log.events[0].reasons, [.clockJumpedBackward])
    }

    func testSmallSynchronizationAdjustmentsAreIgnored() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.jumpWallClock(by: 2)
        postSignals(clock: 1)
        await settle(time, signals: 1)

        let evaluated = await timeChangeWaitUntil { coordinator.evaluationsPerformed == 1 }
        XCTAssertTrue(evaluated)
        XCTAssertTrue(log.events.isEmpty, "two seconds is inside the tolerance")
    }

    func testRealElapsedTimeThroughSleepIsNotAClockJump() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.pass(8 * 3_600)  // the device slept overnight: both clocks advanced
        postSignals(significant: 1)
        await settle(time, signals: 1)

        let evaluated = await timeChangeWaitUntil { coordinator.evaluationsPerformed == 1 }
        XCTAssertTrue(evaluated)
        XCTAssertTrue(log.events.isEmpty)
    }

    func testADaylightSavingTransitionInTheSameZoneIsAnOffsetChange() async {
        let time = ManualTime(wall: instant("2026-03-08T06:30:00Z"), zone: newYork)  // 01:30 EST
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.pass(3_600)  // 03:30 EDT: one real hour later
        postSignals(significant: 1)
        await settle(time, signals: 1)

        let reported = await timeChangeWaitUntil { log.events.count == 1 }
        XCTAssertTrue(reported)
        XCTAssertEqual(log.events[0].reasons, [.utcOffsetChanged])
    }

    func testABurstThatNeverGoesQuietIsStillEvaluatedWithinTheMaximumDeferral() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(
            time, log: log,
            configuration: TimeChangeConfiguration(quietWindowSeconds: 1, maximumDeferralSeconds: 5))
        coordinator.start()
        defer { coordinator.stop() }

        var expectedTimers = 0
        for secondsBeforeSignal in [0, 2, 2.5, 2] as [TimeInterval] {
            time.pass(secondsBeforeSignal)
            postSignals(clock: 1)
            expectedTimers += 1
            let held = await timeChangeWaitUntil { time.sleeperCount == expectedTimers }
            XCTAssertTrue(held, "timers are started in callback order")
        }
        XCTAssertEqual(time.requestedDelays, [1, 1, 0.5, 0], "the wait shrinks to the remaining deferral and then to none")
        time.releaseTimers()
    }

    func testCheckNowReportsAtOnceAndSupersedesAWaitingEvaluation() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        defer { coordinator.stop() }

        time.jumpWallClock(by: 86_400)
        postSignals(clock: 1)
        let held = await timeChangeWaitUntil { time.sleeperCount == 1 }
        XCTAssertTrue(held)

        XCTAssertTrue(coordinator.checkNow())
        time.releaseTimers()
        try? await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertEqual(log.events.count, 1, "the superseded timer found nothing left to report")
        XCTAssertFalse(coordinator.checkNow(), "nothing has changed since")
    }

    func testStoppingDropsPendingEvaluationsAndIgnoresLaterCallbacks() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()

        time.travel(to: losAngeles)
        postSignals(zone: 1)
        let held = await timeChangeWaitUntil { time.sleeperCount == 1 }
        XCTAssertTrue(held)
        coordinator.stop()
        time.releaseTimers()
        postSignals(zone: 2)
        try? await Task.sleep(nanoseconds: 200_000_000)

        XCTAssertEqual(time.sleeperCount, 0, "callbacks after stop start no timer")
        XCTAssertTrue(log.events.isEmpty)
        XCTAssertFalse(coordinator.checkNow())
    }

    func testStartingTwiceRegistersOneSetOfObservers() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"))
        let log = EventLog()
        let coordinator = makeCoordinator(time, log: log)
        coordinator.start()
        coordinator.start()
        defer { coordinator.stop() }

        postSignals(clock: 1)

        let held = await timeChangeWaitUntil { time.sleeperCount >= 1 }
        XCTAssertTrue(held)
        XCTAssertEqual(time.sleeperCount, 1)
        time.releaseTimers()
    }
}

// MARK: - Into the lifecycle

final class TimeChangeLifecycleTests: XCTestCase {
    private let center = NotificationCenter()

    private final class PassLog: @unchecked Sendable {
        private let lock = NSLock()
        private var steps: [String] = []
        private var plannedCountsSeenByPromptStep: [Int] = []

        var order: [String] {
            lock.lock()
            defer { lock.unlock() }
            return steps
        }

        var promptStepCounts: [Int] {
            lock.lock()
            defer { lock.unlock() }
            return plannedCountsSeenByPromptStep
        }

        func record(_ step: String, plannedCount: Int? = nil) {
            lock.lock()
            steps.append(step)
            if let plannedCount { plannedCountsSeenByPromptStep.append(plannedCount) }
            lock.unlock()
        }
    }

    func testARealChangeRunsOneReminderAndPromptPassAndRepeatsRunNone() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"), zone: newYork)
        let log = PassLog()
        let lifecycle = LifecycleCoordinator(stepTimeoutSeconds: 10)
        for step in LifecycleStep.allCases {
            lifecycle.setAction({ _ in log.record(step.rawValue); return .completed }, for: step)
        }
        let coordinator = TimeChangeCoordinator(
            lifecycle: lifecycle, notificationCenter: center, sources: time.sources)
        coordinator.start()
        defer { coordinator.stop() }

        time.travel(to: losAngeles)
        for _ in 0..<4 { center.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        let held = await timeChangeWaitUntil { time.sleeperCount == 4 }
        XCTAssertTrue(held)
        time.releaseTimers()

        let passed = await timeChangeWaitUntil { lifecycle.finishedPassCount == 1 }
        XCTAssertTrue(passed)
        XCTAssertEqual(lifecycle.lastReport?.triggers, [.systemTimeChange])
        XCTAssertEqual(log.order, ["reminders", "promptEligibility"], "only the steps that depend on local time ran")

        for _ in 0..<3 { center.post(name: .NSSystemClockDidChange, object: nil) }
        let heldAgain = await timeChangeWaitUntil { time.sleeperCount == 3 }
        XCTAssertTrue(heldAgain)
        time.releaseTimers()
        try? await Task.sleep(nanoseconds: 300_000_000)

        XCTAssertEqual(lifecycle.finishedPassCount, 1, "repeat callbacks with nothing changed ran no pass")
        XCTAssertEqual(log.order.count, 2)
    }

    func testThePromptScheduleDropsPassedOccurrencesBeforeThePromptStepAndAddsNone() async {
        let time = ManualTime(wall: instant("2026-10-14T12:00:00Z"), zone: newYork)
        let schedule = DailyPromptSchedule()
        schedule.configure(DailyPromptTime(hour: 9, minute: 0), now: time.now, zone: newYork)
        XCTAssertEqual(schedule.planned.count, 3)

        let log = PassLog()
        let lifecycle = LifecycleCoordinator(stepTimeoutSeconds: 10)
        lifecycle.setAction({ _ in .completed }, for: .reminders)
        lifecycle.setAction(
            { _ in log.record("promptEligibility", plannedCount: schedule.planned.count); return .completed },
            for: .promptEligibility)
        let coordinator = TimeChangeCoordinator(
            lifecycle: lifecycle, promptSchedule: schedule, notificationCenter: center, sources: time.sources)
        coordinator.start()
        defer { coordinator.stop() }

        time.jumpWallClock(by: 2 * 86_400)  // the clock was set forward two days
        center.post(name: .NSSystemClockDidChange, object: nil)
        let held = await timeChangeWaitUntil { time.sleeperCount == 1 }
        XCTAssertTrue(held)
        time.releaseTimers()

        let passed = await timeChangeWaitUntil { lifecycle.finishedPassCount == 1 }
        XCTAssertTrue(passed)
        XCTAssertEqual(log.promptStepCounts, [1], "14 and 15 October passed and were dropped, not replayed; none was added")
        XCTAssertEqual(schedule.planned.map(\.localDate.text), ["2026-10-16"])
    }
}
