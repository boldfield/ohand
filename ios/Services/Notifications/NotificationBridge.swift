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
/// the caller has stopped waiting is undone again: the request is removed, or the request that was
/// pending before the attempt is restored, unless a newer schedule of the same identifier has since
/// started (which then owns the state). Undoing is itself bounded and tracked; an undo that cannot
/// finish is remembered and retried by `reconcile()`.
public final class NotificationBridge: Sendable {
    private let center: NotificationCenterProviding
    private let now: @Sendable () -> Date
    private let ingestor: NotificationEventIngestor
    private let removalPollInterval: TimeInterval
    private let maximumRemovalPolls: Int
    private let effectTimeout: TimeInterval
    private let reconciliation = AbandonedWorkLedger()

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
            let attempt = reconciliation.begin(identifier, kind: .schedule)
            let centerRequest = NotificationCenterRequest(
                identifier: identifier, dueInstant: request.dueInstant, content: content)
            let priorRequest = try await bounded { try await center.pendingRequests() }
                .first(where: { $0.identifier == identifier })
            let abandon: @Sendable () -> Void = { [self] in
                self.undoAbandonedSchedule(centerRequest, attempt: attempt, prior: priorRequest)
            }
            try await bounded(
                { try await center.add(centerRequest) },
                onAbandonedSuccess: { _ in abandon() }
            )

            let pending: [NotificationCenterPendingRequest]
            do {
                pending = try await bounded { try await center.pendingRequests() }
            } catch {
                abandon()
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
            reconciliation.begin(rawIdentifier, kind: .cancel)
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
        try? await reconcile()
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

    /// Retries every undo of abandoned work that could not finish earlier. Succeeds when nothing
    /// is left to undo.
    public func reconcile() async throws {
        do {
            for (identifier, undo) in reconciliation.unfinishedUndos() {
                try Task.checkCancellation()
                try await perform(undo, for: identifier)
                reconciliation.finish(identifier, undo: undo)
            }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Waits for the undo of every abandoned schedule started so far. Each is bounded, so this
    /// returns within the effect timeout of the slowest one.
    public func settleAbandonedWork() async {
        while true {
            let running = reconciliation.trackedUndos()
            if running.isEmpty { return }
            for (key, task) in running {
                await task.value
                reconciliation.untrack(key)
            }
        }
    }

    /// Identifiers whose abandoned work could not be undone yet.
    public var unreconciledIdentifiers: [String] {
        reconciliation.unfinishedUndos().map { $0.0 }.sorted()
    }

    private func undoAbandonedSchedule(
        _ attempted: NotificationCenterRequest,
        attempt: Int,
        prior: NotificationCenterPendingRequest?
    ) {
        let identifier = attempted.identifier
        guard let undo = reconciliation.undoForAbandonedSchedule(
            identifier, attempt: attempt, attempted: attempted, prior: prior)
        else { return }
        let key = UUID()
        let task = Task { [self] in
            do {
                try await perform(undo, for: identifier)
                reconciliation.finish(identifier, undo: undo)
            } catch {
                reconciliation.remember(identifier, undo: undo, attempt: attempt)
            }
            reconciliation.untrack(key)
        }
        reconciliation.track(key, task)
    }

    private func perform(_ undo: AbandonedUndo, for identifier: String) async throws {
        let center = self.center
        switch undo {
        case .remove:
            try await bounded { await center.removePending(identifiers: [identifier]) }
        case let .restore(request):
            try await bounded { try await center.add(request) }
        }
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

/// How an abandoned schedule is undone.
private enum AbandonedUndo: Equatable, Sendable {
    case remove
    case restore(NotificationCenterRequest)
}

private enum BridgeOperationKind: Sendable {
    case schedule
    case cancel
}

/// Per-identifier record of the newest operation, of undo work in flight and of undo work that
/// has not finished. Lets a late OS completion tell whether it is still the latest word on its
/// identifier before it removes or restores anything.
private final class AbandonedWorkLedger: @unchecked Sendable {
    private let lock = NSLock()
    private var counter = 0
    private var newest: [String: (attempt: Int, kind: BridgeOperationKind)] = [:]
    private var tracked: [UUID: Task<Void, Never>] = [:]
    private var unfinished: [String: AbandonedUndo] = [:]

    /// Registers a new operation on `identifier`; it now owns the identifier's state, so older
    /// unfinished undos are dropped.
    @discardableResult
    func begin(_ identifier: String, kind: BridgeOperationKind) -> Int {
        lock.lock()
        defer { lock.unlock() }
        counter += 1
        newest[identifier] = (counter, kind)
        unfinished[identifier] = nil
        return counter
    }

    /// The undo an abandoned schedule owes, or nil when a newer schedule owns the identifier or
    /// the pending request is already equivalent to what was there before.
    func undoForAbandonedSchedule(
        _ identifier: String,
        attempt: Int,
        attempted: NotificationCenterRequest,
        prior: NotificationCenterPendingRequest?
    ) -> AbandonedUndo? {
        lock.lock()
        let latest = newest[identifier]
        lock.unlock()
        guard let latest else { return nil }
        if latest.attempt != attempt {
            return latest.kind == .cancel ? .remove : nil
        }
        guard let prior else { return .remove }
        if prior.dueInstant == attempted.dueInstant && prior.userInfo == attempted.content.userInfo {
            return nil
        }
        guard let due = prior.dueInstant,
              prior.userInfo[NotificationContent.kindKey] == NotificationPayloadKind.generic.rawValue,
              let target = prior.userInfo[NotificationContent.targetKey].flatMap({ OpaqueIdentifier($0) })
        else { return nil }
        return .restore(NotificationCenterRequest(
            identifier: identifier, dueInstant: due, content: .generic(opaqueTargetID: target)))
    }

    func remember(_ identifier: String, undo: AbandonedUndo, attempt: Int) {
        lock.lock()
        defer { lock.unlock() }
        guard newest[identifier]?.attempt == attempt || newest[identifier]?.kind == .cancel else { return }
        unfinished[identifier] = undo
    }

    func finish(_ identifier: String, undo: AbandonedUndo) {
        lock.lock()
        defer { lock.unlock() }
        if unfinished[identifier] == undo { unfinished[identifier] = nil }
    }

    func unfinishedUndos() -> [(String, AbandonedUndo)] {
        lock.lock()
        defer { lock.unlock() }
        return unfinished.map { ($0.key, $0.value) }
    }

    func track(_ key: UUID, _ task: Task<Void, Never>) {
        lock.lock()
        defer { lock.unlock() }
        tracked[key] = task
    }

    func untrack(_ key: UUID) {
        lock.lock()
        defer { lock.unlock() }
        tracked[key] = nil
    }

    func trackedUndos() -> [(UUID, Task<Void, Never>)] {
        lock.lock()
        defer { lock.unlock() }
        return tracked.map { ($0.key, $0.value) }
    }
}
