import Foundation
@testable import OhAndServices

/// An in-memory stand-in for the OS that reproduces the behavior the notification probe observed:
/// the same identifier replaces the earlier request, a denied or full center accepts `add`
/// without retaining it, and removal takes effect only after some polls.
final class FakeNotificationCenter: NotificationCenterProviding, @unchecked Sendable {
    private let lock = NSLock()
    private var pending: [String: NotificationCenterRequest] = [:]
    private var removalCountdown: [String: Int] = [:]

    var authorizationStatus: NotificationAuthorization = .authorized
    var addError: Error?
    var pendingReadError: Error?
    var retainsAddedRequests = true
    /// Polls of the pending list a removed request stays visible for.
    var removalDelayPolls = 0
    var delivered: [NotificationCenterDeliveredNotification] = []

    private(set) var addedRequests: [NotificationCenterRequest] = []
    private(set) var removedIdentifiers: [String] = []

    func authorization() async -> NotificationAuthorization {
        lock.lock()
        defer { lock.unlock() }
        return authorizationStatus
    }

    func add(_ request: NotificationCenterRequest) async throws {
        lock.lock()
        defer { lock.unlock() }
        if let addError { throw addError }
        addedRequests.append(request)
        if retainsAddedRequests {
            pending[request.identifier] = request
            removalCountdown[request.identifier] = nil
        }
    }

    func pendingRequests() async throws -> [NotificationCenterPendingRequest] {
        lock.lock()
        defer { lock.unlock() }
        if let pendingReadError { throw pendingReadError }
        for (identifier, remaining) in removalCountdown {
            if remaining <= 0 {
                pending[identifier] = nil
                removalCountdown[identifier] = nil
            } else {
                removalCountdown[identifier] = remaining - 1
            }
        }
        return pending.values.map { request in
            NotificationCenterPendingRequest(
                identifier: request.identifier,
                dueInstant: request.dueInstant,
                userInfo: request.content.userInfo
            )
        }
    }

    func removePending(identifiers: [String]) async {
        lock.lock()
        defer { lock.unlock() }
        removedIdentifiers.append(contentsOf: identifiers)
        for identifier in identifiers where pending[identifier] != nil {
            if removalDelayPolls == 0 {
                pending[identifier] = nil
            } else {
                removalCountdown[identifier] = removalDelayPolls
            }
        }
    }

    func deliveredNotifications() async -> [NotificationCenterDeliveredNotification] {
        lock.lock()
        defer { lock.unlock() }
        return delivered
    }

    func installForeignRequest(identifier: String, dueInstant: Date) {
        lock.lock()
        defer { lock.unlock() }
        pending[identifier] = NotificationCenterRequest(
            identifier: identifier,
            dueInstant: dueInstant,
            content: NotificationContent(title: "Other", body: "Other", userInfo: [:])
        )
    }

    var pendingIdentifiers: [String] {
        lock.lock()
        defer { lock.unlock() }
        return pending.keys.sorted()
    }
}

/// Collects events handed to the bridge's ingestor.
final class EventRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [NotificationEvent] = []

    var events: [NotificationEvent] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(_ event: NotificationEvent) {
        lock.lock()
        defer { lock.unlock() }
        recorded.append(event)
    }
}

/// A latch that suspends callers without honoring task cancellation, like a provider that ignores
/// it. Releasing lets every suspended caller continue.
final class SuspensionGate: @unchecked Sendable {
    private let lock = NSLock()
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private var arrivalCount = 0

    var arrivals: Int {
        lock.lock()
        defer { lock.unlock() }
        return arrivalCount
    }

    func wait() async {
        await withCheckedContinuation { (waiter: CheckedContinuation<Void, Never>) in
            lock.lock()
            arrivalCount += 1
            if isOpen {
                lock.unlock()
                waiter.resume()
            } else {
                waiters.append(waiter)
                lock.unlock()
            }
        }
    }

    func release() {
        lock.lock()
        isOpen = true
        let suspended = waiters
        waiters = []
        lock.unlock()
        suspended.forEach { $0.resume() }
    }
}

/// Wraps a center so chosen calls suspend on a gate before reaching it. The wrapped call still
/// runs after release, which is how a late OS completion is reproduced.
final class SuspendingNotificationCenter: NotificationCenterProviding, @unchecked Sendable {
    let inner: FakeNotificationCenter
    private let gateLock = NSLock()
    private var pendingCallCount = 0
    var authorizationGate: SuspensionGate?
    var addGate: SuspensionGate?
    var pendingGate: SuspensionGate?
    /// Pending-list calls that pass the gate before it starts suspending callers. Scheduling
    /// reads the list once before `add` and once after it.
    var pendingCallsBeforeGate = 0
    var removeGate: SuspensionGate?
    var deliveredGate: SuspensionGate?

    init(inner: FakeNotificationCenter) {
        self.inner = inner
    }

    func authorization() async -> NotificationAuthorization {
        await authorizationGate?.wait()
        return await inner.authorization()
    }

    func add(_ request: NotificationCenterRequest) async throws {
        await addGate?.wait()
        try await inner.add(request)
    }

    func pendingRequests() async throws -> [NotificationCenterPendingRequest] {
        gateLock.lock()
        pendingCallCount += 1
        let isGated = pendingCallCount > pendingCallsBeforeGate
        gateLock.unlock()
        if isGated { await pendingGate?.wait() }
        return try await inner.pendingRequests()
    }

    func removePending(identifiers: [String]) async {
        await removeGate?.wait()
        await inner.removePending(identifiers: identifiers)
    }

    func deliveredNotifications() async -> [NotificationCenterDeliveredNotification] {
        await deliveredGate?.wait()
        return await inner.deliveredNotifications()
    }
}
