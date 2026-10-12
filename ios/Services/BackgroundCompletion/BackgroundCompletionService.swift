import Foundation

/// Asks for finite background time when a capture ends and uses it to let the job drain finish, then lets go.
///
/// Call `completeAfterCapture()` while the app is still in the foreground, right after the capture is durably saved.
/// The service holds one `BackgroundExecutionGrant` and one drain completion window, and ends both on every path:
///
/// * **Completed.** The drain went idle and the core reports nothing queued: the pending alert is removed.
/// * **Left pending.** The drain went idle with jobs queued (backoff, offline, a missing capability): nothing more is
///   attempted here, the alert stays armed, and the jobs wait for a later foreground or reachability drain.
/// * **Expired.** The system took the time back, the service's own cap elapsed, or the owner shut down: the window is
///   closed, which stops the running drain at a checkpoint (its job returns to the queue with no retry budget spent),
///   and the grant is ended at once. Nothing is retried in the background and nothing is waited for beyond a short,
///   bounded settle.
/// * **Refused.** The system granted no time: no window is opened and no alert is armed.
///
/// An unprocessed transcript or interpretation stays saved and queued. This type never schedules, and never claims
/// to have scheduled, the reminder a capture asked for. The optional alert is the only thing it can leave behind,
/// and it is a generic, single, one-shot "not processed yet" notice that is never worded as the reminder.
@MainActor
public final class BackgroundCompletionService {
    private let drain: BackgroundCompletionDrain
    private let grant: BackgroundExecutionGrant
    private let pending: PendingProcessingReading
    private let alert: PendingProcessingAlerting?
    private let maximumWindow: TimeInterval
    private let settleTimeout: TimeInterval
    private let pollInterval: TimeInterval
    private var activeLease: Lease?
    private var activeRun: Task<BackgroundCompletionOutcome, Never>?
    private var armTask: Task<Void, Never>?
    private var isShutDown = false

    public init(
        drain: BackgroundCompletionDrain,
        grant: BackgroundExecutionGrant,
        pending: PendingProcessingReading,
        alert: PendingProcessingAlerting? = nil,
        maximumWindow: TimeInterval = BackgroundCompletionConstraints.defaultMaximumWindow,
        settleTimeout: TimeInterval = BackgroundCompletionConstraints.defaultSettleTimeout,
        pollInterval: TimeInterval = 0.05
    ) {
        self.drain = drain
        self.grant = grant
        self.pending = pending
        self.alert = alert
        self.maximumWindow = maximumWindow
        self.settleTimeout = settleTimeout
        self.pollInterval = pollInterval
    }

    /// Runs one completion lease and reports how it ended. A call made while a lease is open asks the drain to run
    /// once more and returns that lease's outcome; it does not extend the window.
    @discardableResult
    public func completeAfterCapture() async -> BackgroundCompletionOutcome {
        if let activeRun {
            drain.requestDrain()
            return await activeRun.value
        }
        guard !isShutDown else { return .expired(.shutDown) }
        let run = Task { @MainActor [self] in await runLease() }
        activeRun = run
        return await run.value
    }

    /// The app became active: whatever alert is armed is no longer needed, because the app can show status itself.
    public func appBecameActive() async {
        await settleArming()
        await alert?.disarm()
    }

    /// Ends any open lease as expired and refuses further ones. Idempotent.
    public func shutDown() {
        isShutDown = true
        guard let lease = activeLease else { return }
        endLease(lease, cause: .shutDown)
    }

    // MARK: lease

    private func runLease() async -> BackgroundCompletionOutcome {
        defer { activeRun = nil }
        let lease = Lease()
        let drain = self.drain
        let grant = self.grant
        let token = grant.begin { [weak grant] in
            // Runs on the main thread when the system takes the time back, or at once when it cannot grant any.
            let held = lease.expire(.systemExpiration)
            drain.endCompletionWindow()
            if let held {
                grant?.end(held)
            }
        }
        guard token != Self.invalidToken else {
            return .refused
        }
        guard lease.attach(token) else {
            grant.end(token)
            drain.endCompletionWindow()
            return .expired(.systemExpiration)
        }
        activeLease = lease
        defer { activeLease = nil }

        drain.beginCompletionWindow()
        if let alert {
            armTask = Task { _ = await alert.arm() }
        }

        let deadline = Date().addingTimeInterval(maximumWindow)
        while true {
            if let cause = lease.cause {
                return await finishExpired(lease, cause: cause)
            }
            if !drain.isDraining { break }
            if Date() >= deadline {
                return await finishExpired(lease, cause: .windowElapsed)
            }
            try? await Task.sleep(nanoseconds: Self.nanoseconds(pollInterval))
        }

        let status = await pending.read()
        drain.endCompletionWindow()
        if status == .none {
            await settleArming()
            await alert?.disarm()
        }
        releaseGrant(lease)
        switch status {
        case .none:
            return .completed
        case .pending(let jobs):
            return .pendingRemains(pendingJobs: jobs)
        case .unknown:
            return .pendingRemains(pendingJobs: nil)
        }
    }

    private func finishExpired(_ lease: Lease, cause: BackgroundCompletionExpiryCause) async -> BackgroundCompletionOutcome {
        endLease(lease, cause: cause)
        let settleDeadline = Date().addingTimeInterval(settleTimeout)
        while drain.isDraining, Date() < settleDeadline {
            try? await Task.sleep(nanoseconds: Self.nanoseconds(pollInterval))
        }
        return .expired(lease.cause ?? cause)
    }

    private func endLease(_ lease: Lease, cause: BackgroundCompletionExpiryCause) {
        drain.endCompletionWindow()
        if let held = lease.expire(cause) {
            grant.end(held)
        }
    }

    private func releaseGrant(_ lease: Lease) {
        if let held = lease.release() {
            grant.end(held)
        }
    }

    private func settleArming() async {
        await armTask?.value
        armTask = nil
    }

    private static let invalidToken = 0

    private static func nanoseconds(_ seconds: TimeInterval) -> UInt64 {
        UInt64(max(0.001, seconds) * 1_000_000_000)
    }
}

/// The grant token for one lease, handed to whichever of the expiration handler and the lease's own end runs first,
/// so the background task is ended exactly once.
private final class Lease: @unchecked Sendable {
    private let lock = NSLock()
    private var token: Int?
    private var isFinished = false
    private var recordedCause: BackgroundCompletionExpiryCause?

    var cause: BackgroundCompletionExpiryCause? {
        lock.lock()
        defer { lock.unlock() }
        return recordedCause
    }

    /// `false` when the lease already ended (the handler ran before `begin` returned): the caller ends `token`.
    func attach(_ value: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !isFinished else { return false }
        token = value
        return true
    }

    /// Records the first expiry cause and returns the token to end, if it was not already taken.
    func expire(_ cause: BackgroundCompletionExpiryCause) -> Int? {
        lock.lock()
        defer { lock.unlock() }
        if recordedCause == nil { recordedCause = cause }
        return takeLocked()
    }

    /// Returns the token to end without recording an expiry.
    func release() -> Int? {
        lock.lock()
        defer { lock.unlock() }
        return takeLocked()
    }

    private func takeLocked() -> Int? {
        isFinished = true
        let value = token
        token = nil
        return value
    }
}
