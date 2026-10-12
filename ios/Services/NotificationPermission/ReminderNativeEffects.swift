import Foundation

/// The OS work the core's capacity pass asks for, with identifiers carried exactly as the OS holds them.
///
/// `NotificationBridge` lists and cancels only requests whose identifier is a reminder's
/// `<reminder-id>#<generation>`. The capacity pass must also see optional-prompt requests, whose identifiers
/// are independent raw strings, to count them against the OS budget and to cancel them when explicit
/// reminders need their slots. So the pending list and the cancel of a non-reminder identifier go to the
/// center directly, each call bounded by `effectTimeout` and confirmed like the bridge's own cancel. Reminder
/// schedules and reminder cancels still go through the bridge and its desired-state ledger.
struct ReminderNativeEffects: Sendable {
    /// The core rejects longer identifiers, so a request this long cannot be one the core created.
    static let maximumIdentifierBytes = 128

    let bridge: NotificationBridge
    let center: NotificationCenterProviding
    let effectTimeout: TimeInterval
    let removalPollInterval: TimeInterval
    let maximumRemovalPolls: Int

    func pending() async throws -> [NotificationCenterPendingRequest] {
        let center = self.center
        do {
            return try await BoundedNativeCall.run(timeout: effectTimeout) { try await center.pendingRequests() }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
    }

    /// The pending requests split into those the core can name and a count of the rest.
    struct CarriedPending {
        let named: [NotificationCenterPendingRequest]
        let unattributableCount: Int
    }

    /// Every pending request as the core can carry it.
    ///
    /// A request whose identifier is empty or longer than the core accepts cannot be one the core created, but it
    /// still occupies an OS slot, so it is counted instead of being left out. It is not renamed: any name native
    /// chose could equal a valid prompt identifier, so the count crosses the boundary on its own and the core
    /// charges each one under a placeholder that no prompt or reminder identifier can match.
    static func carried(_ requests: [NotificationCenterPendingRequest]) -> CarriedPending {
        let named = requests.filter { request in
            let size = request.identifier.utf8.count
            return size > 0 && size <= maximumIdentifierBytes
        }
        return CarriedPending(named: named, unattributableCount: requests.count - named.count)
    }

    func cancel(_ rawIdentifier: String) async throws {
        if let reminderIdentifier = NotificationIdentifier(rawValue: rawIdentifier) {
            try await bridge.cancel(reminderIdentifier)
            return
        }
        let center = self.center
        do {
            try await BoundedNativeCall.run(timeout: effectTimeout) {
                await center.removePending(identifiers: [rawIdentifier])
            }
            // Removal is asynchronous in the OS, so a cancel counts only once the request is no longer listed.
            for poll in 0..<max(1, maximumRemovalPolls) {
                let stillPending = try await pending().contains { $0.identifier == rawIdentifier }
                if !stillPending { return }
                if poll + 1 < max(1, maximumRemovalPolls) {
                    try await Task.sleep(nanoseconds: UInt64(max(0, removalPollInterval) * 1_000_000_000))
                }
            }
        } catch {
            throw NotificationBridgeError.normalized(error)
        }
        throw NotificationBridgeError.cancelNotConfirmed
    }
}

/// Runs one OS call as its own task and returns whichever happens first: its result, its failure, the caller's
/// cancellation or the deadline. A call that loses may still finish; nothing here depends on its late result.
enum BoundedNativeCall {
    static func run<Value: Sendable>(
        timeout: TimeInterval, _ operation: @escaping @Sendable () async throws -> Value
    ) async throws -> Value {
        try Task.checkCancellation()
        let race = Race<Value>()
        let deadlineNanoseconds = UInt64(max(0, timeout) * 1_000_000_000)
        return try await withTaskCancellationHandler {
            race.track(
                Task {
                    do {
                        let value = try await operation()
                        race.resolve(.success(value))
                    } catch {
                        race.resolve(.failure(error))
                    }
                })
            race.track(
                Task {
                    do { try await Task.sleep(nanoseconds: deadlineNanoseconds) } catch { return }
                    race.resolve(.failure(NotificationBridgeError.timedOut))
                })
            return try await race.outcome()
        } onCancel: {
            race.resolve(.failure(CancellationError()))
        }
    }

    private final class Race<Value: Sendable>: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Value, Error>?
        private var decided: Result<Value, Error>?
        private var isResolved = false
        private var tasks: [Task<Void, Never>] = []

        func resolve(_ result: Result<Value, Error>) {
            lock.lock()
            if isResolved {
                lock.unlock()
                return
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
}
