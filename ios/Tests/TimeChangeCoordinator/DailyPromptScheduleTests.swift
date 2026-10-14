import Foundation
import XCTest
@testable import OhAndServices

private func instant(_ text: String) -> Date {
    ISO8601DateFormatter().date(from: text)!
}

private func prompt(at hour: Int, _ minute: Int) -> DailyPromptTime {
    DailyPromptTime(hour: hour, minute: minute)!
}

private let newYork = TimeZone(identifier: "America/New_York")!
private let losAngeles = TimeZone(identifier: "America/Los_Angeles")!

/// The local-time semantics of the daily prompt: one occurrence per local day, DST gaps and folds, and what a clock or
/// zone change may and may not do to the plan.
final class DailyPromptPlannerTests: XCTestCase {
    func testAnOrdinaryDayFiresAtTheChosenWallClockTime() {
        let planned = DailyPromptPlanner.plan(
            at: prompt(at: 9, 0), after: instant("2026-10-14T12:00:00Z"), in: newYork)

        XCTAssertEqual(
            planned.map(\.fireAt),
            [instant("2026-10-14T13:00:00Z"), instant("2026-10-15T13:00:00Z"), instant("2026-10-16T13:00:00Z")])
        XCTAssertEqual(planned.map(\.notificationID), ["prompt:2026-10-14", "prompt:2026-10-15", "prompt:2026-10-16"])
    }

    func testASkippedLocalTimeFiresAtTheFirstValidMinuteAfterTheGap() {
        let planned = DailyPromptPlanner.plan(
            at: prompt(at: 2, 30), after: instant("2026-03-07T20:00:00Z"), in: newYork)

        XCTAssertEqual(
            planned.map(\.fireAt),
            [
                instant("2026-03-08T07:00:00Z"),  // 02:30 does not exist on 8 March: 03:00 EDT
                instant("2026-03-09T06:30:00Z"),
                instant("2026-03-10T06:30:00Z"),
            ])
        XCTAssertEqual(planned.map(\.localDate.text), ["2026-03-08", "2026-03-09", "2026-03-10"])
    }

    func testAHalfHourGapFiresAtTheFirstValidMinute() throws {
        let lordHowe = try XCTUnwrap(TimeZone(identifier: "Australia/Lord_Howe"))
        let date = LocalCalendarDate(year: 2026, month: 10, day: 4)

        let fireAt = DailyPromptPlanner.fireInstant(on: date, at: prompt(at: 2, 15), in: lordHowe)

        XCTAssertEqual(fireAt, instant("2026-10-03T15:30:00Z"), "02:15 is skipped; 02:30 LHDT (UTC+11) is the first valid minute")
    }

    func testARepeatedLocalTimeFiresAtItsFirstOccurrence() {
        let planned = DailyPromptPlanner.plan(
            at: prompt(at: 1, 30), after: instant("2026-10-31T12:00:00Z"), in: newYork)

        XCTAssertEqual(
            planned.map(\.fireAt),
            [
                instant("2026-11-01T05:30:00Z"),  // 01:30 EDT, before the clocks go back
                instant("2026-11-02T06:30:00Z"),
                instant("2026-11-03T06:30:00Z"),
            ])
    }

    func testTheHorizonIsBoundedAndEveryOccurrenceIsAheadOfTheFloor() {
        let floor = instant("2026-10-14T13:00:00Z")
        let planned = DailyPromptPlanner.plan(at: prompt(at: 9, 0), after: floor, in: newYork)

        XCTAssertEqual(planned.count, DailyPromptPlanner.horizonDays)
        XCTAssertTrue(planned.allSatisfy { $0.fireAt > floor }, "an occurrence at the floor itself has already happened")
        XCTAssertEqual(planned.first?.localDate.text, "2026-10-15")
    }

    func testSpentDaysAreNotPlannedAgain() {
        let spent: Set<LocalCalendarDate> = [LocalCalendarDate(year: 2026, month: 10, day: 15)]
        let planned = DailyPromptPlanner.plan(
            at: prompt(at: 9, 0), after: instant("2026-10-14T12:00:00Z"), in: newYork, excluding: spent)

        XCTAssertEqual(planned.map(\.localDate.text), ["2026-10-14", "2026-10-16", "2026-10-17"])
    }

    func testInvalidTimesAreRefused() {
        XCTAssertNil(DailyPromptTime(hour: 24, minute: 0))
        XCTAssertNil(DailyPromptTime(hour: 9, minute: 60))
        XCTAssertNil(DailyPromptTime(hour: -1, minute: 0))
    }
}

final class DailyPromptScheduleTests: XCTestCase {
    private let start = instant("2026-10-14T12:00:00Z")  // 08:00 in New York

    func testPromptingIsOffUntilATimeIsChosen() {
        let schedule = DailyPromptSchedule()
        XCTAssertNil(schedule.time)
        XCTAssertTrue(schedule.candidates().isEmpty)

        schedule.appOpened(now: start, zone: newYork)
        XCTAssertTrue(schedule.candidates().isEmpty, "opening the app does not turn prompting on")
    }

    func testChoosingATimePlansTheHorizonAndClearingItEmptiesThePlan() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        XCTAssertEqual(schedule.candidates().count, DailyPromptPlanner.horizonDays)
        XCTAssertEqual(schedule.candidates().first?.notificationID, "prompt:2026-10-14")

        schedule.configure(nil, now: start, zone: newYork)
        XCTAssertTrue(schedule.candidates().isEmpty)
    }

    func testReentryAndTimeChangesDropPassedOccurrencesAndNeverAddOne() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)

        schedule.appReentered(now: start.addingTimeInterval(86_400), zone: newYork)
        XCTAssertEqual(schedule.planned.map(\.localDate.text), ["2026-10-15", "2026-10-16"], "the first day passed; nothing was added")

        schedule.timeChanged(now: start.addingTimeInterval(10 * 86_400), zone: newYork)
        XCTAssertTrue(schedule.planned.isEmpty, "after days of nonuse the plan is empty: no backlog and no catch-up")
    }

    func testAppOpenRefillsFromTheNextFutureOccurrenceWithoutReplayingMissedDays() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        schedule.timeChanged(now: start.addingTimeInterval(10 * 86_400), zone: newYork)

        schedule.appOpened(now: start.addingTimeInterval(10 * 86_400), zone: newYork)

        XCTAssertEqual(schedule.planned.map(\.localDate.text), ["2026-10-24", "2026-10-25", "2026-10-26"])
    }

    func testZoneTravelKeepsPlannedInstantsUntilTheAppIsOpenedThenReplansTheLocalTime() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        let before = schedule.planned

        schedule.timeChanged(now: start.addingTimeInterval(60), zone: losAngeles)
        XCTAssertEqual(schedule.planned, before, "a zone change alone moves nothing and adds nothing")

        schedule.appOpened(now: start.addingTimeInterval(120), zone: losAngeles)
        XCTAssertEqual(
            schedule.planned.map(\.fireAt),
            [instant("2026-10-14T16:00:00Z"), instant("2026-10-15T16:00:00Z"), instant("2026-10-16T16:00:00Z")],
            "09:00 in Los Angeles")
    }

    func testAClockMovedBackNeverReplaysAnOccurrenceTheClockAlreadyReached() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        let afterFirstDay = start.addingTimeInterval(86_400)
        schedule.timeChanged(now: afterFirstDay, zone: newYork)
        XCTAssertTrue(schedule.isSpent(LocalCalendarDate(year: 2026, month: 10, day: 14)))

        schedule.timeChanged(now: start, zone: newYork)
        schedule.appOpened(now: start, zone: newYork)

        XCTAssertEqual(
            schedule.planned.map(\.localDate.text), ["2026-10-15", "2026-10-16", "2026-10-17"],
            "14 October is not replayed: the high-water mark keeps every instant the clock reached out of the plan")
    }

    func testADaySpentByItsOccurrenceIsNotPlannedAgainWhenTheTimeMovesLater() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        let afterNine = instant("2026-10-14T14:00:00Z")  // 10:00 in New York

        schedule.configure(prompt(at: 18, 0), now: afterNine, zone: newYork)

        XCTAssertEqual(schedule.planned.first?.localDate.text, "2026-10-15", "14 October had its prompt")
    }

    func testTheSpentLedgerIsPrunedByAge() {
        let schedule = DailyPromptSchedule()
        schedule.configure(prompt(at: 9, 0), now: start, zone: newYork)
        schedule.timeChanged(now: start.addingTimeInterval(86_400), zone: newYork)
        XCTAssertTrue(schedule.isSpent(LocalCalendarDate(year: 2026, month: 10, day: 14)))

        schedule.timeChanged(now: start.addingTimeInterval(40 * 86_400), zone: newYork)

        XCTAssertFalse(schedule.isSpent(LocalCalendarDate(year: 2026, month: 10, day: 14)))
    }
}
