import Foundation
import UIKit

// MARK: - Vocabulary

/// What a settled reading found different from the one before it.
public enum TimeChangeReason: Hashable, Sendable {
    case timeZoneChanged
    /// The offset from UTC differs, for instance across a daylight-saving transition in the same zone.
    case utcOffsetChanged
    case clockJumpedForward
    case clockJumpedBackward
}

/// One settled time change. Callbacks that arrived together are folded into one event.
public struct TimeChangeEvent: Equatable, Sendable {
    public let reasons: Set<TimeChangeReason>
    /// How many system callbacks this event folds in. Zero for a `checkNow()` made with none pending.
    public let foldedSignals: Int
    public let observedAt: Date
}

/// The readings change detection compares: wall clock, a monotonic clock that keeps counting through sleep, and the zone.
struct TimeReading: Equatable {
    let wall: Date
    let monotonicSeconds: TimeInterval
    let zoneIdentifier: String
    let utcOffsetSeconds: Int
}

public struct TimeChangeConfiguration: Equatable, Sendable {
    /// A change is evaluated this long after the last callback of a burst.
    public var quietWindowSeconds: TimeInterval
    /// A burst that never goes quiet is still evaluated this long after its first callback.
    public var maximumDeferralSeconds: TimeInterval
    /// Wall-clock movement beyond the monotonic clock's by more than this counts as a jump. Smaller movement is
    /// ordinary synchronization.
    public var clockJumpToleranceSeconds: TimeInterval

    public init(
        quietWindowSeconds: TimeInterval = 1,
        maximumDeferralSeconds: TimeInterval = 5,
        clockJumpToleranceSeconds: TimeInterval = 5
    ) {
        self.quietWindowSeconds = quietWindowSeconds
        self.maximumDeferralSeconds = maximumDeferralSeconds
        self.clockJumpToleranceSeconds = clockJumpToleranceSeconds
    }
}

/// The clocks and timer the coordinator reads, injectable so tests move time without waiting for it.
public struct TimeSources: Sendable {
    public var wallClock: @Sendable () -> Date
    /// Seconds on a clock that never jumps and keeps counting while the device sleeps.
    public var monotonicSeconds: @Sendable () -> TimeInterval
    public var timeZone: @Sendable () -> TimeZone
    public var sleep: @Sendable (TimeInterval) async -> Void

    public init(
        wallClock: @escaping @Sendable () -> Date,
        monotonicSeconds: @escaping @Sendable () -> TimeInterval,
        timeZone: @escaping @Sendable () -> TimeZone,
        sleep: @escaping @Sendable (TimeInterval) async -> Void
    ) {
        self.wallClock = wallClock
        self.monotonicSeconds = monotonicSeconds
        self.timeZone = timeZone
        self.sleep = sleep
    }

    public static let system = TimeSources(
        wallClock: { Date() },
        monotonicSeconds: {
            var specification = timespec()
            clock_gettime(CLOCK_MONOTONIC, &specification)
            return TimeInterval(specification.tv_sec) + TimeInterval(specification.tv_nsec) / 1_000_000_000
        },
        timeZone: { TimeZone.current },
        sleep: { seconds in
            try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
        })
}

// MARK: - Coordinator

/// Turns the system's clock and time-zone callbacks into one reconciliation request per real change.
///
/// The system posts several callbacks for one event (a zone change brings a zone callback and a significant-time
/// callback), repeats them, and posts them for changes that do not matter (midnight, a small clock synchronization).
/// This type makes that noise harmless:
///
/// * **Coalescing.** Callbacks arriving within `quietWindowSeconds` of each other settle together into one
///   evaluation; a burst that never quiets is still evaluated `maximumDeferralSeconds` after its first callback.
/// * **Real changes only.** The evaluation compares a reading with the one before it: the zone identifier, the UTC
///   offset, and wall-clock movement against a monotonic clock that counts through sleep. A callback that finds none
///   of them changed (a repeat, a midnight, a change undone within the window) requests nothing.
/// * **One request.** A real change calls `onChange` once. It requests reconciliation; it does not install, cancel or
///   deliver anything itself, and never decides whether a reminder is due. The reminder pass does, so a reminder
///   keeps the absolute instant the core resolved (the bridge installs it as a UTC trigger no zone change moves), an
///   overdue one is expired rather than replayed, and a repeated request converges on the same state.
///
/// Wire it to the lifecycle with `init(lifecycle:)`; the lifecycle runs only the reminder and prompt steps for it.
public final class TimeChangeCoordinator: @unchecked Sendable {
    /// The callbacks the coordinator listens for.
    public static let observedNotifications: [Notification.Name] = [
        .NSSystemClockDidChange, .NSSystemTimeZoneDidChange, UIApplication.significantTimeChangeNotification,
    ]

    private let notificationCenter: NotificationCenter
    private let sources: TimeSources
    private let configuration: TimeChangeConfiguration
    private let onChange: @Sendable (TimeChangeEvent) -> Void

    private let lock = NSLock()
    private var observers: [NSObjectProtocol] = []
    private var isStarted = false
    private var isStopped = false
    private var baseline: TimeReading?
    private var pendingSignals = 0
    private var firstSignalAt: TimeInterval?
    private var generation = 0
    private var deliveredEvents = 0
    private var evaluationCount = 0

    public init(
        notificationCenter: NotificationCenter = .default,
        sources: TimeSources = .system,
        configuration: TimeChangeConfiguration = TimeChangeConfiguration(),
        onChange: @escaping @Sendable (TimeChangeEvent) -> Void
    ) {
        self.notificationCenter = notificationCenter
        self.sources = sources
        self.configuration = configuration
        self.onChange = onChange
    }

    /// Forwards each real change to `lifecycle`. The lifecycle's reminder and prompt steps then act: the prompt step
    /// refreshes the core's durable prompt schedule for the device's current zone, so this type keeps no prompt state.
    public convenience init(
        lifecycle: LifecycleCoordinator,
        notificationCenter: NotificationCenter = .default,
        sources: TimeSources = .system,
        configuration: TimeChangeConfiguration = TimeChangeConfiguration()
    ) {
        let requestReconciliation = lifecycle.timeChangeTrigger
        self.init(
            notificationCenter: notificationCenter, sources: sources, configuration: configuration,
            onChange: { _ in
                requestReconciliation()
            })
    }

    deinit {
        stop()
    }

    // MARK: Observation

    /// How many changes have been reported to `onChange`.
    public var reportedChangeCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return deliveredEvents
    }

    /// How many times a settled reading was compared, whether or not it found a change.
    public var evaluationsPerformed: Int {
        lock.lock()
        defer { lock.unlock() }
        return evaluationCount
    }

    // MARK: Lifecycle

    /// Takes the first reading and starts listening. Idempotent; does nothing after `stop()`.
    public func start() {
        lock.lock()
        guard !isStarted, !isStopped else {
            lock.unlock()
            return
        }
        isStarted = true
        baseline = currentReading()
        let center = notificationCenter
        observers = Self.observedNotifications.map { name in
            center.addObserver(forName: name, object: nil, queue: nil) { [weak self] _ in
                self?.receiveSignal()
            }
        }
        lock.unlock()
    }

    /// Stops listening and drops any evaluation still waiting. Idempotent.
    public func stop() {
        lock.lock()
        isStopped = true
        isStarted = false
        generation += 1
        pendingSignals = 0
        firstSignalAt = nil
        let registered = observers
        observers = []
        lock.unlock()
        for observer in registered {
            notificationCenter.removeObserver(observer)
        }
    }

    /// Compares now against the last reading at once, without waiting out the quiet window, and reports a change if
    /// there is one. For a caller that wants to check after a stretch in which callbacks may have been missed, such as
    /// the app returning from the background. Returns whether a change was reported.
    @discardableResult
    public func checkNow() -> Bool {
        lock.lock()
        guard isStarted, !isStopped else {
            lock.unlock()
            return false
        }
        generation += 1
        let event = evaluateLocked()
        lock.unlock()
        guard let event else { return false }
        onChange(event)
        return true
    }

    // MARK: Settling

    private func receiveSignal() {
        let now = sources.monotonicSeconds()
        lock.lock()
        guard isStarted, !isStopped else {
            lock.unlock()
            return
        }
        pendingSignals += 1
        let burstStart = firstSignalAt ?? now
        firstSignalAt = burstStart
        generation += 1
        let waitingFor = generation
        let remainingDeferral = configuration.maximumDeferralSeconds - (now - burstStart)
        let delay = max(0, min(configuration.quietWindowSeconds, remainingDeferral))
        lock.unlock()

        let sleep = sources.sleep
        Task(priority: .utility) { [weak self] in
            await sleep(delay)
            self?.settle(generation: waitingFor)
        }
    }

    private func settle(generation settledGeneration: Int) {
        lock.lock()
        guard isStarted, !isStopped, generation == settledGeneration else {
            lock.unlock()
            return
        }
        let event = evaluateLocked()
        lock.unlock()
        if let event {
            onChange(event)
        }
    }

    /// Must hold the lock. Takes a reading, advances the baseline to it, and returns the event if it differs.
    private func evaluateLocked() -> TimeChangeEvent? {
        let signals = pendingSignals
        pendingSignals = 0
        firstSignalAt = nil
        evaluationCount += 1
        let current = currentReading()
        let previous = baseline
        baseline = current
        guard let previous else { return nil }
        let reasons = Self.reasons(from: previous, to: current, tolerance: configuration.clockJumpToleranceSeconds)
        guard !reasons.isEmpty else { return nil }
        deliveredEvents += 1
        return TimeChangeEvent(reasons: reasons, foldedSignals: signals, observedAt: current.wall)
    }

    private func currentReading() -> TimeReading {
        let wall = sources.wallClock()
        let zone = sources.timeZone()
        return TimeReading(
            wall: wall,
            monotonicSeconds: sources.monotonicSeconds(),
            zoneIdentifier: zone.identifier,
            utcOffsetSeconds: zone.secondsFromGMT(for: wall))
    }

    static func reasons(from previous: TimeReading, to current: TimeReading, tolerance: TimeInterval)
        -> Set<TimeChangeReason>
    {
        var reasons = Set<TimeChangeReason>()
        if previous.zoneIdentifier != current.zoneIdentifier {
            reasons.insert(.timeZoneChanged)
        }
        if previous.utcOffsetSeconds != current.utcOffsetSeconds {
            reasons.insert(.utcOffsetChanged)
        }
        let wallElapsed = current.wall.timeIntervalSince(previous.wall)
        let monotonicElapsed = current.monotonicSeconds - previous.monotonicSeconds
        let drift = wallElapsed - monotonicElapsed
        if drift > tolerance {
            reasons.insert(.clockJumpedForward)
        } else if drift < -tolerance {
            reasons.insert(.clockJumpedBackward)
        }
        return reasons
    }
}
