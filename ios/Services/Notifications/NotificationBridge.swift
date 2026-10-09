import Foundation

/// Native side of the core's notification effect interface: schedule, cancel, list pending and
/// delivered-evidence ingestion.
///
/// Rust decides what should be installed; this type executes and reports. It transports only
/// validated opaque identifiers and fixed wording. Authenticated reads and user mutations (and
/// therefore any credential) are outside it. Scheduling reads the permission first and the
/// pending list afterwards, because the OS accepts an `add` that it then does not keep.
///
/// Every OS call is cancellable and time-bounded: it runs as its own task and races a deadline and
/// the caller's cancellation, so a provider that ignores cancellation or never answers still
/// yields a normalized `cancelled` or `timed_out` result. A schedule that the OS completes after
/// the caller has stopped waiting is removed again, so cancelled work leaves no pending request.
public final class NotificationBridge: Sendable {
    private let center: NotificationCenterProviding
    private let now: @Sendable () -> Date
    private let ingestor: NotificationEventIngestor
    private let removalPollInterval: TimeInterval
    private let maximumRemovalPolls: Int
    private let effectTimeout: TimeInterval

    public init(
        center: NotificationCenterProviding,
        ingestor: NotificationEventIngestor,
        now: @escaping @Sendable () -> Date = { Date() },
        removalPollInterval: TimeInterval = 0.05,
        maximumRemovalPolls: Int = 20,
        effectTimeout: TimeInterval = 10
    ) {
        self.center = center
        self.ingestor = ingestor
        self.now = now
        self.removalPollInterval = removalPollInterval
        self.maximumRemovalPolls = max(1, maximumRemovalPolls)
        self.effectTimeout = max(0, effectTimeout)
    }

    /// Installs the request, replacing any pending request with the same identifier, and returns
    /// what the OS reports pending afterwards.
    public func schedule(_ request: NotificationScheduleRequest) async throws -> InstalledNotification {
        do {
            let content = try Self.content(for: request)
            guard request.dueInstant.timeIntervalSince(now()) >= 1 else {
                throw NotificationBridgeError.dueTimeInPast
            }
            let center = self.center
            switch try await bounded({ await center.authorization() }) {
            case .authorized, .provisional, .ephemeral:
                break
            case .notDetermined, .denied:
                throw NotificationBridgeError.permissionDenied
            }

            let identifier = request.identifier.rawValue
            let centerRequest = NotificationCenterRequest(
                identifier: identifier, dueInstant: request.dueInstant, content: content)
            try await bounded(
                { try await center.add(centerRequest) },
                onAbandonedSuccess: { _ in await center.removePending(identifiers: [identifier]) }
            )

            let pending: [NotificationCenterPendingRequest]
            do {
                pending = try await bounded { try await center.pendingRequests() }
            } catch {
                Task.detached { await center.removePending(identifiers: [identifier]) }
                throw error
            }
            guard let installed = pending.first(where: { $0.identifier == identifier }) else {
                throw NotificationBridgeError.installNotConfirmed
            }
            return InstalledNotification(
                identifier: request.identifier,
                dueInstant: installed.dueInstant ?? request.dueInstant
            )
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Removes the pending request and waits until the OS no longer lists it, because removal is
    /// asynchronous. Idempotent: cancelling an identifier that is not pending succeeds.
    public func cancel(_ identifier: NotificationIdentifier) async throws {
        do {
            let center = self.center
            let rawIdentifier = identifier.rawValue
            try await bounded { await center.removePending(identifiers: [rawIdentifier]) }
            for attempt in 0..<maximumRemovalPolls {
                let pending = try await bounded { try await center.pendingRequests() }
                if !pending.contains(where: { $0.identifier == identifier.rawValue }) { return }
                if attempt + 1 < maximumRemovalPolls {
                    try await Task.sleep(nanoseconds: UInt64(removalPollInterval * 1_000_000_000))
                }
            }
            throw NotificationBridgeError.cancelNotConfirmed
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Pending requests that carry a core-derived identifier, soonest first. Requests with any
    /// other identifier are not ours to report.
    public func pendingNotifications() async throws -> [PendingNotification] {
        let pending: [NotificationCenterPendingRequest]
        do {
            let center = self.center
            pending = try await bounded { try await center.pendingRequests() }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
        return pending.compactMap { request -> PendingNotification? in
            guard let identifier = NotificationIdentifier(rawValue: request.identifier) else { return nil }
            return PendingNotification(
                identifier: identifier,
                dueInstant: request.dueInstant,
                opaqueTargetID: request.userInfo[NotificationContent.targetKey].flatMap { OpaqueIdentifier($0) }
            )
        }
        .sorted { left, right in
            switch (left.dueInstant, right.dueInstant) {
            case let (leftDue?, rightDue?) where leftDue != rightDue:
                return leftDue < rightDue
            case (nil, _?):
                return false
            case (_?, nil):
                return true
            default:
                return left.identifier.rawValue < right.identifier.rawValue
            }
        }
    }

    /// Reports every notification the OS still lists as delivered, for the launch-time check of
    /// requests that fired while the app was not running. Returns how many events were forwarded.
    @discardableResult
    public func ingestDeliveredNotifications() async throws -> Int {
        let deliveredNotifications: [NotificationCenterDeliveredNotification]
        do {
            let center = self.center
            deliveredNotifications = try await bounded { await center.deliveredNotifications() }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
        var forwarded = 0
        for delivered in deliveredNotifications {
            try Task.checkCancellation()
            let accepted = ingestor.ingestDelivered(
                requestIdentifier: delivered.identifier,
                userInfo: delivered.userInfo,
                deliveredAt: delivered.deliveredAt
            )
            if accepted { forwarded += 1 }
        }
        return forwarded
    }

    /// Runs one OS call as its own task and returns whichever happens first: the call's result, its
    /// failure, the caller's cancellation, or the deadline. The losing call is cancelled but may
    /// still finish; `onAbandonedSuccess` receives such a late result so its effect can be undone.
    private func bounded<Value: Sendable>(
        _ operation: @escaping @Sendable () async throws -> Value,
        onAbandonedSuccess: (@Sendable (Value) async -> Void)? = nil
    ) async throws -> Value {
        try Task.checkCancellation()
        let race = EffectRace<Value>()
        let deadlineNanoseconds = UInt64(effectTimeout * 1_000_000_000)
        return try await withTaskCancellationHandler {
            race.track(Task {
                do {
                    let value = try await operation()
                    if !race.resolve(.success(value)), let onAbandonedSuccess {
                        await onAbandonedSuccess(value)
                    }
                } catch {
                    race.resolve(.failure(error))
                }
            })
            race.track(Task {
                do { try await Task.sleep(nanoseconds: deadlineNanoseconds) } catch { return }
                race.resolve(.failure(NotificationBridgeError.timedOut))
            })
            return try await race.outcome()
        } onCancel: {
            race.resolve(.failure(CancellationError()))
        }
    }

    /// The only place a payload is built. Generic payloads get fixed wording plus the opaque
    /// target; a generic request that carries preview text or an approval is refused rather than
    /// silently stripped. Preview payloads are not enabled in this bridge.
    static func content(for request: NotificationScheduleRequest) throws -> NotificationContent {
        switch request.payloadKind {
        case .generic:
            guard request.previewText == nil, request.previewApproval == nil else {
                throw NotificationBridgeError.invalidPayload
            }
            return NotificationContent.generic(opaqueTargetID: request.opaqueTargetID)
        case .previewApproved:
            guard let text = request.previewText, !text.isEmpty,
                  let approval = request.previewApproval, approval.policyVersion > 0
            else {
                throw NotificationBridgeError.previewApprovalRequired
            }
            throw NotificationBridgeError.previewNotEnabled
        }
    }
}

/// First-writer-wins hand-off between an OS call, its deadline and the caller's cancellation.
private final class EffectRace<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Value, Error>?
    private var decided: Result<Value, Error>?
    private var isResolved = false
    private var tasks: [Task<Void, Never>] = []

    /// Returns whether this call decided the outcome. Deciding cancels the competing tasks.
    @discardableResult
    func resolve(_ result: Result<Value, Error>) -> Bool {
        lock.lock()
        if isResolved {
            lock.unlock()
            return false
        }
        isResolved = true
        let waiting = continuation
        continuation = nil
        if waiting == nil { decided = result }
        let competing = tasks
        tasks = []
        lock.unlock()
        competing.forEach { $0.cancel() }
        waiting?.resume(with: result)
        return true
    }

    func track(_ task: Task<Void, Never>) {
        lock.lock()
        if isResolved {
            lock.unlock()
            task.cancel()
            return
        }
        tasks.append(task)
        lock.unlock()
    }

    func outcome() async throws -> Value {
        try await withCheckedThrowingContinuation { (waiting: CheckedContinuation<Value, Error>) in
            lock.lock()
            if let decided = self.decided {
                lock.unlock()
                waiting.resume(with: decided)
            } else {
                continuation = waiting
                lock.unlock()
            }
        }
    }
}
