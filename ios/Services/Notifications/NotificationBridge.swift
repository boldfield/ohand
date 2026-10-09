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
/// yields a normalized `cancelled` or `timed_out` result.
///
/// A call the bridge stopped waiting for can still complete later. To keep such late work from
/// disturbing newer state, every mutation of one identifier runs exclusively (behind a bounded
/// per-identifier lock), and the bridge remembers the state the newest operation wants for that
/// identifier: installed for a schedule, absent for a cancel, or what was pending before an
/// operation that failed or was abandoned. An operation records its wanted state before its first
/// OS write, so that an older abandoned call completing late while it runs is never mistaken for
/// current. Whenever an abandoned mutation completes late, the bridge re-applies the desired state
/// under the same lock, so a late write is overwritten rather than trusted, and a check made
/// earlier is never acted on after a newer operation has started. Re-applying is itself bounded
/// and tracked, and counts as done only once the OS confirms the desired state; one that cannot
/// finish or confirm is remembered and retried by `reconcile()`.
///
/// The state an operation puts back when it fails is the state the bridge last recorded for the
/// identifier, not what the OS listed when the operation began: the list can still show an
/// abandoned write whose undo has not landed, and a failed operation must not adopt that write as
/// the earlier state. The OS list decides only for an identifier the bridge has no record of. A
/// recorded request whose due instant has passed is never installed again; re-applying it only
/// removes whatever else was written under its identifier.
public final class NotificationBridge: Sendable {
    private let center: NotificationCenterProviding
    private let now: @Sendable () -> Date
    private let ingestor: NotificationEventIngestor
    private let removalPollInterval: TimeInterval
    private let maximumRemovalPolls: Int
    private let effectTimeout: TimeInterval
    private let ledger = DesiredStateLedger()
    private let identifierLocks = IdentifierLocks()

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
    /// what the OS reports pending afterwards. The install is confirmed only when the OS lists
    /// this request, with its due instant and payload, under the identifier; an earlier request
    /// still listed there is not confirmation.
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
            return try await exclusively(identifier) {
                let priorRequest = try await self.bounded { try await center.pendingRequests() }
                    .first(where: { $0.identifier == identifier })
                let stateBeforeAttempt = self.restorePoint(for: identifier, observed: priorRequest)
                // Claim the identifier for this attempt before writing. An older abandoned call
                // (such as an undo removal that timed out) may complete while this add or its
                // confirming read is in flight; because the desired state has already moved on,
                // that late completion re-applies this install instead of being taken as current.
                self.ledger.setDesired(identifier, .installed(centerRequest))
                do {
                    try await self.mutate(identifier, toward: nil) { try await center.add(centerRequest) }
                } catch {
                    // A definite failure changed nothing. An abandoned add may still land, and its
                    // late completion then re-applies the earlier state recorded here.
                    self.ledger.setDesired(identifier, stateBeforeAttempt)
                    throw error
                }

                let installed: NotificationCenterPendingRequest
                do {
                    installed = try await self.confirmInstalled(centerRequest)
                } catch {
                    // The read failed, or the OS accepted the add without keeping it. The caller
                    // is told this schedule failed, so the earlier state is wanted again.
                    self.ledger.setDesired(identifier, stateBeforeAttempt)
                    self.enforceInBackground(identifier)
                    throw error
                }
                self.ledger.markSettled(identifier)
                return InstalledNotification(
                    identifier: request.identifier,
                    dueInstant: installed.dueInstant ?? request.dueInstant
                )
            }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Removes the pending request and waits until the OS no longer lists it, because removal is
    /// asynchronous. Idempotent: cancelling an identifier that is not pending succeeds. A cancel
    /// that does not succeed puts back the request that was pending before it.
    public func cancel(_ identifier: NotificationIdentifier) async throws {
        do {
            let center = self.center
            let rawIdentifier = identifier.rawValue
            try await exclusively(rawIdentifier) {
                let priorRequest = try await self.bounded { try await center.pendingRequests() }
                    .first(where: { $0.identifier == rawIdentifier })
                let stateBeforeAttempt = self.restorePoint(for: rawIdentifier, observed: priorRequest)
                let generation = self.ledger.setDesired(rawIdentifier, .absent)
                do {
                    try await self.mutate(rawIdentifier, toward: generation) {
                        await center.removePending(identifiers: [rawIdentifier])
                    }
                } catch let error where Self.isUncertain(error) {
                    // The removal may still land. If it lands after this point, its late completion
                    // re-applies the earlier state; if it landed before, its completion still saw
                    // `.absent` as current and did nothing, so an earlier request is put back here
                    // too. Re-applying runs under the lock and leaves a still-pending request alone.
                    self.ledger.setDesired(rawIdentifier, stateBeforeAttempt)
                    if case .installed = stateBeforeAttempt {
                        self.enforceInBackground(rawIdentifier)
                    }
                    throw error
                }

                do {
                    try await self.confirmAbsent(rawIdentifier)
                    self.ledger.markSettled(rawIdentifier)
                } catch {
                    // The caller is told this cancel failed, so the earlier request is put back.
                    self.ledger.setDesired(rawIdentifier, stateBeforeAttempt)
                    self.enforceInBackground(rawIdentifier)
                    throw error
                }
            }
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

    /// Re-applies the desired state of every identifier whose late work could not be settled
    /// earlier. Succeeds when nothing is left unsettled.
    public func reconcile() async throws {
        do {
            for identifier in ledger.unsettledIdentifiers() {
                try Task.checkCancellation()
                try await enforce(identifier)
            }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// Waits for every re-application started so far, including ones that late completions start
    /// while waiting. Each is bounded, so this returns once the provider stops completing late.
    public func settleAbandonedWork() async {
        while true {
            let running = ledger.trackedWork()
            if running.isEmpty { return }
            for (key, task) in running {
                await task.value
                ledger.untrack(key)
            }
        }
    }

    /// Identifiers whose desired state could not be re-applied yet.
    public var unreconciledIdentifiers: [String] {
        ledger.unsettledIdentifiers().sorted()
    }

    /// Runs `body` while holding `identifier`'s lock. Waiting for the lock is bounded too; it is
    /// held by at most a few bounded calls at a time.
    private func exclusively<Value: Sendable>(
        _ identifier: String,
        _ body: @Sendable () async throws -> Value
    ) async throws -> Value {
        let locks = identifierLocks
        try await bounded(
            { await locks.acquire(identifier) },
            timeout: effectTimeout * 4,
            onAbandonedSuccess: { _ in locks.release(identifier) }
        )
        defer { locks.release(identifier) }
        return try await body()
    }

    /// Runs one mutating OS call. If the bridge stops waiting and the call completes later, the
    /// desired state is re-applied unless the late call was itself applying the still-current
    /// `generation` of it.
    private func mutate(
        _ identifier: String,
        toward generation: Int?,
        _ operation: @escaping @Sendable () async throws -> Void
    ) async throws {
        try await bounded(operation, onAbandonedSuccess: { [self] _ in
            if let generation, self.ledger.generation(of: identifier) == generation { return }
            self.enforceInBackground(identifier)
        })
    }

    /// Starts a tracked re-application of `identifier`'s desired state; a failure leaves the
    /// identifier for `reconcile()`.
    private func enforceInBackground(_ identifier: String) {
        let key = UUID()
        let task = Task { [self] in
            do {
                try await enforce(identifier)
            } catch {
                ledger.markUnsettled(identifier)
            }
            ledger.untrack(key)
        }
        ledger.track(key, task)
    }

    /// Makes the OS match the desired state of `identifier`, under its lock so no other operation
    /// on it can interleave. Leaves an identifier with no known desired state alone.
    ///
    /// The identifier is settled only once the OS confirms the desired state, by the same
    /// read-back `schedule` and `cancel` require: the OS accepts an add it then does not keep,
    /// and carries out a removal asynchronously. A write it did not confirm leaves the identifier
    /// unsettled for `reconcile()`, so an abandoned operation is never called undone on the
    /// strength of a write alone.
    private func enforce(_ identifier: String) async throws {
        let center = self.center
        do {
            try await exclusively(identifier) {
                let (desired, generation) = self.ledger.desired(of: identifier)
                switch desired {
                case nil:
                    break
                case .absent:
                    try await self.mutate(identifier, toward: generation) {
                        await center.removePending(identifiers: [identifier])
                    }
                    try await self.confirmAbsent(identifier)
                case let .installed(request):
                    let current = try await self.bounded { try await center.pendingRequests() }
                        .first(where: { $0.identifier == identifier })
                    if let current, Self.matches(current, request) { break }
                    if self.hasPassed(request) {
                        // The request is not installed again once its due instant has passed: the
                        // OS has delivered or dropped it, and a late copy would be a stale reminder.
                        // A different request written under its identifier still goes.
                        guard current != nil else { break }
                        try await self.mutate(identifier, toward: generation) {
                            await center.removePending(identifiers: [identifier])
                        }
                        try await self.confirmAbsent(identifier)
                        break
                    }
                    try await self.mutate(identifier, toward: generation) { try await center.add(request) }
                    _ = try await self.confirmInstalled(request)
                }
                self.ledger.markSettled(identifier)
            }
        } catch {
            ledger.markUnsettled(identifier)
            throw error
        }
    }

    /// Reads the pending list back and returns the entry that is `request` itself. Only the
    /// request that was asked for counts as installed: a pending request that merely shares the
    /// identifier is an earlier one the OS kept when it accepted the add without applying it, so
    /// taking it as confirmation would report a replacement that never happened and leave the OS
    /// firing at the earlier instant.
    private func confirmInstalled(_ request: NotificationCenterRequest) async throws -> NotificationCenterPendingRequest {
        let center = self.center
        let pending = try await bounded { try await center.pendingRequests() }
        guard let installed = pending.first(where: { $0.identifier == request.identifier }),
              Self.matches(installed, request)
        else {
            throw NotificationBridgeError.installNotConfirmed
        }
        return installed
    }

    /// Polls the pending list until the OS no longer lists `identifier`, because removal is
    /// asynchronous. Fails with `cancelNotConfirmed` when it is still listed after the last poll.
    private func confirmAbsent(_ identifier: String) async throws {
        let center = self.center
        for attempt in 0..<maximumRemovalPolls {
            let pending = try await bounded { try await center.pendingRequests() }
            if !pending.contains(where: { $0.identifier == identifier }) { return }
            if attempt + 1 < maximumRemovalPolls {
                try await Task.sleep(nanoseconds: UInt64(removalPollInterval * 1_000_000_000))
            }
        }
        throw NotificationBridgeError.cancelNotConfirmed
    }

    /// Whether a bounded call's failure leaves its effect unknown: the bridge stopped waiting.
    private static func isUncertain(_ error: Error) -> Bool {
        error is CancellationError || (error as? NotificationBridgeError) == .timedOut
    }

    /// Whether the OS lists `request` itself under its identifier: the same payload, due at the
    /// same instant. Instants compare at whole seconds, because the calendar trigger the real
    /// center installs carries no finer precision and reports the instant it kept.
    private static func matches(_ pending: NotificationCenterPendingRequest, _ request: NotificationCenterRequest) -> Bool {
        guard let pendingDue = pending.dueInstant else { return false }
        return wholeSecond(pendingDue) == wholeSecond(request.dueInstant)
            && pending.userInfo == request.content.userInfo
    }

    private static func wholeSecond(_ instant: Date) -> Date {
        Date(timeIntervalSince1970: instant.timeIntervalSince1970.rounded(.down))
    }

    /// What a schedule or cancel puts back if it fails: the state the bridge last recorded for the
    /// identifier, when it has one, because the OS list may still show an abandoned write whose
    /// undo is pending. Only an identifier without a record is read from the list.
    private func restorePoint(
        for identifier: String,
        observed prior: NotificationCenterPendingRequest?
    ) -> DesiredNotificationState? {
        let (recorded, generation) = ledger.desired(of: identifier)
        if generation != nil { return recorded }
        return Self.restorableState(of: prior, identifier: identifier)
    }

    /// Whether `request` could no longer be scheduled: its due instant is not ahead of the clock
    /// by the margin `schedule` requires.
    private func hasPassed(_ request: NotificationCenterRequest) -> Bool {
        request.dueInstant.timeIntervalSince(now()) < 1
    }

    /// What to put back for an identifier the bridge has no record of: nothing when nothing was
    /// pending, the earlier generic request when one was, or nil (leave alone) when the earlier
    /// request cannot be rebuilt from fixed wording.
    private static func restorableState(
        of prior: NotificationCenterPendingRequest?,
        identifier: String
    ) -> DesiredNotificationState? {
        guard let prior else { return .absent }
        guard let due = prior.dueInstant,
              prior.userInfo[NotificationContent.kindKey] == NotificationPayloadKind.generic.rawValue,
              let target = prior.userInfo[NotificationContent.targetKey].flatMap({ OpaqueIdentifier($0) })
        else { return nil }
        return .installed(NotificationCenterRequest(
            identifier: identifier, dueInstant: due, content: .generic(opaqueTargetID: target)))
    }

    /// Runs one OS call as its own task and returns whichever happens first: the call's result, its
    /// failure, the caller's cancellation, or the deadline. The losing call is cancelled but may
    /// still finish; `onAbandonedSuccess` receives such a late result so its effect can be undone.
    private func bounded<Value: Sendable>(
        _ operation: @escaping @Sendable () async throws -> Value,
        timeout: TimeInterval? = nil,
        onAbandonedSuccess: (@Sendable (Value) async -> Void)? = nil
    ) async throws -> Value {
        try Task.checkCancellation()
        let race = EffectRace<Value>()
        let deadlineNanoseconds = UInt64((timeout ?? effectTimeout) * 1_000_000_000)
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

/// The state an identifier should be in, as decided by the newest operation on it.
private enum DesiredNotificationState: Equatable, Sendable {
    case absent
    case installed(NotificationCenterRequest)
}

/// Per-identifier desired state with a generation that changes on every decision, the set of
/// identifiers whose desired state could not be re-applied, and the re-applications in flight.
private final class DesiredStateLedger: @unchecked Sendable {
    private let lock = NSLock()
    private var counter = 0
    private var desired: [String: (state: DesiredNotificationState?, generation: Int)] = [:]
    private var unsettled: Set<String> = []
    private var tracked: [UUID: Task<Void, Never>] = [:]

    /// Records a decision; nil means the state is unknown and is left alone. Returns its generation.
    @discardableResult
    func setDesired(_ identifier: String, _ state: DesiredNotificationState?) -> Int {
        lock.lock()
        defer { lock.unlock() }
        counter += 1
        desired[identifier] = (state, counter)
        return counter
    }

    func desired(of identifier: String) -> (DesiredNotificationState?, Int?) {
        lock.lock()
        defer { lock.unlock() }
        let entry = desired[identifier]
        return (entry?.state, entry?.generation)
    }

    func generation(of identifier: String) -> Int? {
        lock.lock()
        defer { lock.unlock() }
        return desired[identifier]?.generation
    }

    func markUnsettled(_ identifier: String) {
        lock.lock()
        defer { lock.unlock() }
        unsettled.insert(identifier)
    }

    func markSettled(_ identifier: String) {
        lock.lock()
        defer { lock.unlock() }
        unsettled.remove(identifier)
    }

    func unsettledIdentifiers() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return Array(unsettled)
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

    func trackedWork() -> [(UUID, Task<Void, Never>)] {
        lock.lock()
        defer { lock.unlock() }
        return tracked.map { ($0.key, $0.value) }
    }
}

/// First-come, first-served exclusive ownership of an identifier. Acquiring does not honor
/// cancellation itself; callers bound it, and an abandoned acquirer releases as soon as it is
/// granted ownership.
private final class IdentifierLocks: @unchecked Sendable {
    private let lock = NSLock()
    private var held: Set<String> = []
    private var waiting: [String: [CheckedContinuation<Void, Never>]] = [:]

    func acquire(_ identifier: String) async {
        await withCheckedContinuation { (waiter: CheckedContinuation<Void, Never>) in
            lock.lock()
            if held.insert(identifier).inserted {
                lock.unlock()
                waiter.resume()
            } else {
                waiting[identifier, default: []].append(waiter)
                lock.unlock()
            }
        }
    }

    /// Hands ownership to the next waiter, if any, without letting a newcomer cut in.
    func release(_ identifier: String) {
        lock.lock()
        if var queue = waiting[identifier], !queue.isEmpty {
            let next = queue.removeFirst()
            waiting[identifier] = queue.isEmpty ? nil : queue
            lock.unlock()
            next.resume()
        } else {
            held.remove(identifier)
            lock.unlock()
        }
    }
}
