import Foundation

/// The Apple mechanism behind bounded background completion, and the constraints this module is built around.
///
/// The mechanism is `UIApplication.beginBackgroundTask(withName:expirationHandler:)` (Apple documentation, checked
/// 2026-10-12). What it does and does not promise:
///
/// * The time is finite and chosen by the system. The documentation gives no figure; `backgroundTimeRemaining`
///   reports it, and it is commonly about thirty seconds. `BackgroundCompletionService` therefore caps its own
///   window well below that and never assumes more.
/// * The system calls the expiration handler synchronously on the main thread shortly before the time reaches zero.
///   The task must be ended during or soon after it, or the system terminates the app. The handler here requests a
///   checkpoint and ends the task at once.
/// * The call may return `UIBackgroundTaskIdentifier.invalid` when background execution is not possible, and when
///   the system cannot grant the assertion it calls the expiration handler instead. Both mean "no time": nothing is
///   retried and no work is assumed to have finished.
/// * The request is asynchronous, so it is made while the app is still in the foreground, at the end of a capture,
///   not from a background-entry callback.
/// * Nothing here is a wake mechanism. `BGProcessingTask` and `BGAppRefreshTask` are discretionary, may never run,
///   and are not used; there is no background audio or location. If the app is suspended or terminated with work
///   pending, that work stays queued and runs on a later launch or foreground.
public enum BackgroundCompletionConstraints {
    /// The longest a completion window stays open, in seconds, whatever the system would still allow.
    public static let defaultMaximumWindow: TimeInterval = 20
    /// How long after asking for a checkpoint the service waits for the drain to stop before it lets go of the grant.
    public static let defaultSettleTimeout: TimeInterval = 3
}

/// Why a completion window ended with work possibly in flight.
public enum BackgroundCompletionExpiryCause: Equatable, Sendable {
    /// The system took the background time back (or could not grant it).
    case systemExpiration
    /// The service's own cap on the window elapsed first.
    case windowElapsed
    /// The owner shut the service down.
    case shutDown
}

/// How one completion lease ended. It says what was observed, never that something was scheduled or processed beyond
/// that: a saved capture whose processing did not finish stays saved and unprocessed, and a reminder in it stays
/// unscheduled until the core schedules it.
public enum BackgroundCompletionOutcome: Equatable, Sendable {
    /// The drain went idle and the core reported no pending or running job.
    case completed
    /// The drain went idle but jobs remain queued (backoff, offline, a missing capability or an unreadable count).
    /// `pendingJobs` is `nil` when the count could not be read.
    case pendingRemains(pendingJobs: Int?)
    /// The window ended while work could still be in flight. The running drain was stopped at a checkpoint; its job
    /// is back in the queue and nothing was lost.
    case expired(BackgroundCompletionExpiryCause)
    /// The system gave no background time. Nothing was waited for.
    case refused
}

/// What the core says is still queued.
public enum PendingProcessingStatus: Equatable, Sendable {
    case none
    case pending(jobs: Int)
    case unknown
}

/// Reads how much processing the core still has queued.
public protocol PendingProcessingReading: Sendable {
    func read() async -> PendingProcessingStatus
}

/// The seam over `JobRunnerService`: a completion window that lets the drain run while the app is in the background.
public protocol BackgroundCompletionDrain: AnyObject, Sendable {
    var isDraining: Bool { get }
    func beginCompletionWindow()
    func endCompletionWindow()
    func requestDrain()
}

extension JobRunnerService: BackgroundCompletionDrain {}
