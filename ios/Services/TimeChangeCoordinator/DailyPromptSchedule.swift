import Foundation

// MARK: - Vocabulary

/// A chosen local wall-clock time of day, to the minute.
public struct DailyPromptTime: Equatable, Hashable, Sendable {
    public let hour: Int
    public let minute: Int

    public init?(hour: Int, minute: Int) {
        guard (0...23).contains(hour), (0...59).contains(minute) else { return nil }
        self.hour = hour
        self.minute = minute
    }
}

/// A calendar day as a person in some zone reads it, with no instant attached.
public struct LocalCalendarDate: Equatable, Hashable, Comparable, Sendable {
    public let year: Int
    public let month: Int
    public let day: Int

    public init(year: Int, month: Int, day: Int) {
        self.year = year
        self.month = month
        self.day = day
    }

    public var text: String {
        String(format: "%04d-%02d-%02d", year, month, day)
    }

    public static func < (lhs: LocalCalendarDate, rhs: LocalCalendarDate) -> Bool {
        (lhs.year, lhs.month, lhs.day) < (rhs.year, rhs.month, rhs.day)
    }
}

/// One planned occurrence of the daily prompt: its stable identifier, the local day it belongs to and the absolute
/// instant it fires at.
public struct PlannedDailyPrompt: Equatable, Sendable {
    public let notificationID: String
    public let localDate: LocalCalendarDate
    public let fireAt: Date
}

// MARK: - Planner

/// The local-time semantics of the optional daily prompt, the same ones the core's prompt schedule documents
/// (`core/src/suggestions/schedule/`): one prompt per local day at the chosen wall-clock time, the next
/// `horizonDays` occurrences ahead of a floor instant, identified as `prompt:<local date>`.
///
/// * A local time the zone skips (a DST gap) fires at the first valid minute after the gap.
/// * A local time the zone repeats (a DST fold) fires at its first occurrence.
/// * The instant is computed from the zone's offsets, not from calendar leniency, so it does not depend on how
///   `Calendar` resolves a nonexistent or ambiguous local time.
public enum DailyPromptPlanner {
    public static let horizonDays = 3
    public static let identifierPrefix = "prompt:"

    /// The longest DST gap searched for the first valid minute after a skipped time.
    static let maximumGapMinutes = 180
    static let scanLimitDays = 366

    public static func notificationID(for localDate: LocalCalendarDate) -> String {
        identifierPrefix + localDate.text
    }

    /// The next occurrences strictly after `floor`, skipping `spent` days, at most `horizonDays` of them.
    public static func plan(
        at time: DailyPromptTime,
        after floor: Date,
        in zone: TimeZone,
        excluding spent: Set<LocalCalendarDate> = []
    ) -> [PlannedDailyPrompt] {
        var planned: [PlannedDailyPrompt] = []
        var localDate = self.localDate(of: floor, in: zone)
        for _ in 0..<scanLimitDays {
            if planned.count == horizonDays { break }
            if !spent.contains(localDate), let fireAt = fireInstant(on: localDate, at: time, in: zone), fireAt > floor {
                planned.append(
                    PlannedDailyPrompt(notificationID: notificationID(for: localDate), localDate: localDate, fireAt: fireAt))
            }
            guard let next = day(after: localDate) else { break }
            localDate = next
        }
        return planned
    }

    /// The instant `time` falls on `localDate` in `zone`, or `nil` if no valid minute exists within the search bound.
    public static func fireInstant(on localDate: LocalCalendarDate, at time: DailyPromptTime, in zone: TimeZone) -> Date? {
        guard let wall = wallClockInstant(on: localDate, hour: time.hour, minute: time.minute) else { return nil }
        for shiftMinutes in 0...maximumGapMinutes {
            let shifted = wall.addingTimeInterval(TimeInterval(shiftMinutes * 60))
            if let instant = earliestInstant(forWallClock: shifted, in: zone) {
                return instant
            }
        }
        return nil
    }

    public static func localDate(of instant: Date, in zone: TimeZone) -> LocalCalendarDate {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let parts = calendar.dateComponents([.year, .month, .day], from: instant)
        return LocalCalendarDate(year: parts.year ?? 1970, month: parts.month ?? 1, day: parts.day ?? 1)
    }

    static func day(after localDate: LocalCalendarDate) -> LocalCalendarDate? {
        shifted(localDate, byDays: 1)
    }

    static func shifted(_ localDate: LocalCalendarDate, byDays days: Int) -> LocalCalendarDate? {
        let calendar = utcCalendar()
        guard
            let start = calendar.date(from: DateComponents(year: localDate.year, month: localDate.month, day: localDate.day)),
            let moved = calendar.date(byAdding: .day, value: days, to: start)
        else { return nil }
        let parts = calendar.dateComponents([.year, .month, .day], from: moved)
        guard let year = parts.year, let month = parts.month, let day = parts.day else { return nil }
        return LocalCalendarDate(year: year, month: month, day: day)
    }

    private static func utcCalendar() -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .current
        return calendar
    }

    /// The wall-clock reading as if it were UTC: the number every candidate offset is subtracted from.
    private static func wallClockInstant(on localDate: LocalCalendarDate, hour: Int, minute: Int) -> Date? {
        utcCalendar().date(
            from: DateComponents(
                year: localDate.year, month: localDate.month, day: localDate.day, hour: hour, minute: minute))
    }

    /// Every instant whose local reading in `zone` is `wall` (read as UTC) is `wall` minus an offset the zone is on
    /// at that instant. Candidate offsets are those in force a day either side, which brackets any transition. None
    /// valid means a gap; two mean a fold, and the earlier one is the first occurrence.
    private static func earliestInstant(forWallClock wall: Date, in zone: TimeZone) -> Date? {
        let day: TimeInterval = 86_400
        let candidateOffsets = Set([
            zone.secondsFromGMT(for: wall.addingTimeInterval(-day)),
            zone.secondsFromGMT(for: wall.addingTimeInterval(day)),
        ])
        let valid = candidateOffsets.compactMap { offset -> Date? in
            let instant = wall.addingTimeInterval(-TimeInterval(offset))
            return zone.secondsFromGMT(for: instant) == offset ? instant : nil
        }
        return valid.min()
    }
}

// MARK: - Schedule

/// The in-process plan of the daily prompt, kept by the code that offers prompt candidates to the reminder pass.
///
/// The rules mirror the core's schedule so native and core cannot disagree about what a clock or zone change means:
///
/// * The horizon is filled only by `configure` (the user changing the setting in the open app) and `appOpened`.
///   `appReentered` and `timeChanged` only drop occurrences that have passed; they never add one, so a clock or zone
///   change cannot create a backlog or a catch-up burst.
/// * Occurrences keep their absolute instant when the zone changes. Zone travel takes effect when the app is next
///   opened, which re-plans the same local time in the new zone.
/// * A high-water mark records the latest clock reading seen. No occurrence at or before it is ever planned, so a
///   clock moved backward cannot replay a local day the user has already lived through. A local day whose occurrence
///   the mark reached is spent and is not planned again, even if the user moves the time later.
public final class DailyPromptSchedule: @unchecked Sendable {
    /// How long a spent day is remembered, measured from the high-water day.
    public static let spentRetentionDays = 14

    private let lock = NSLock()
    private var chosenTime: DailyPromptTime?
    private var plan: [PlannedDailyPrompt] = []
    private var highWater: Date?
    private var spentDates: Set<LocalCalendarDate> = []

    public init() {}

    /// The chosen time, or `nil` while prompting is off (the default).
    public var time: DailyPromptTime? {
        lock.lock()
        defer { lock.unlock() }
        return chosenTime
    }

    public var planned: [PlannedDailyPrompt] {
        lock.lock()
        defer { lock.unlock() }
        return plan
    }

    public func isSpent(_ localDate: LocalCalendarDate) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return spentDates.contains(localDate)
    }

    /// The candidates to offer the core's capacity pass. Empty while prompting is off.
    public func candidates() -> [OptionalPromptCandidate] {
        planned.map { OptionalPromptCandidate(notificationID: $0.notificationID, fireAt: $0.fireAt) }
    }

    /// Applies the user's choice and rebuilds the horizon ahead of everything the clock has shown. `nil` turns
    /// prompting off and empties the plan.
    public func configure(_ time: DailyPromptTime?, now: Date, zone: TimeZone) {
        lock.lock()
        defer { lock.unlock() }
        observe(now: now, zone: zone)
        chosenTime = time
        rebuild(zone: zone)
    }

    /// The app was opened. The only event besides `configure` that refills the horizon.
    public func appOpened(now: Date, zone: TimeZone) {
        lock.lock()
        defer { lock.unlock() }
        observe(now: now, zone: zone)
        rebuild(zone: zone)
    }

    /// The app returned to the foreground: occurrences that have passed are dropped and none are added.
    public func appReentered(now: Date, zone: TimeZone) {
        lock.lock()
        defer { lock.unlock() }
        observe(now: now, zone: zone)
    }

    /// The system clock or zone changed: occurrences that have passed are dropped and none are added.
    public func timeChanged(now: Date, zone: TimeZone) {
        lock.lock()
        defer { lock.unlock() }
        observe(now: now, zone: zone)
    }

    private func observe(now: Date, zone: TimeZone) {
        let reached = highWater.map { max($0, now) } ?? now
        highWater = reached
        for occurrence in plan where occurrence.fireAt <= reached {
            spentDates.insert(occurrence.localDate)
        }
        plan.removeAll { $0.fireAt <= reached }
        let today = DailyPromptPlanner.localDate(of: reached, in: zone)
        if let oldestKept = DailyPromptPlanner.shifted(today, byDays: -Self.spentRetentionDays) {
            spentDates = spentDates.filter { $0 >= oldestKept }
        }
    }

    private func rebuild(zone: TimeZone) {
        guard let chosenTime, let floor = highWater else {
            plan = []
            return
        }
        plan = DailyPromptPlanner.plan(at: chosenTime, after: floor, in: zone, excluding: spentDates)
    }
}
