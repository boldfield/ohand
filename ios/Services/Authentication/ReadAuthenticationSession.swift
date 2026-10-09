import Foundation

/// The runtime read-authorization state from the contract's "Session read scope".
enum SessionReadScope: String, Equatable {
    /// Locked, cancelled, failed or relocked. New captures can be saved; no stored history, counts, snippets or
    /// errors derived from stored content may be returned.
    case captureOnly = "capture_only"
    case authenticated
}

enum ReadAuthenticationFailure: Equatable {
    case cancelled
    case denied
    case unavailable
    /// A relock or a newer attempt ended this attempt before it could grant access.
    case interrupted
}

enum ReadAuthenticationResult: Equatable {
    case authenticated
    case notAuthenticated(ReadAuthenticationFailure)
}

enum RelockReason: String, Equatable {
    case enteredBackground
    case willEnterForeground
    case protectedDataUnavailable
    case explicit
}

/// One change of the session read scope, published to observers after the session has already applied it.
struct SessionScopeTransition: Equatable {
    enum Cause: Equatable {
        case authenticated
        /// A lifecycle event or explicit request returned the session to capture-only.
        case relocked(RelockReason)
        /// An attempt did not grant access, which also revokes access held from an earlier success.
        case authenticationFailed(ReadAuthenticationFailure)
    }

    let scope: SessionReadScope
    let cause: Cause
}

/// Identifies one observer registration so it can be removed.
struct SessionScopeObserverToken: Hashable {
    fileprivate let identifier: UInt64
}

/// Proof that a read was admitted in a specific access epoch. It stops being current at the next relock or failed
/// attempt, so work admitted earlier can be recognized and dropped when its result arrives late.
struct ReadAuthorization: Equatable {
    fileprivate let epoch: UInt64
}

/// Holds the session read scope for the running app. The scope starts at `captureOnly` on every launch, becomes
/// `authenticated` only on an explicit successful attempt, and returns to `captureOnly` on relock or any
/// non-successful attempt, including a failed re-authentication of an already authenticated session.
///
/// The session owns no stored content and never touches storage: revoking access cannot delete, move or rewrite
/// source data. Safe to call from any thread.
///
/// Scope changes are published to observers (`observeScope`) so that content already shown can be dropped. Each
/// change is delivered synchronously, in order, on the thread that caused it, before `relock` or the attempt's
/// completion returns to its caller. Observers must not block waiting on another thread that is calling the session.
final class ReadAuthenticationSession {
    private let boundary: ReadAuthenticationBoundary
    private let completionQueue: DispatchQueue
    private let lock = NSLock()
    private var currentScope: SessionReadScope = .captureOnly
    private var accessEpoch: UInt64 = 0
    private var attemptSequence: UInt64 = 0
    private var attemptInFlight: UInt64?
    private var recordedRelockReason: RelockReason?
    private let deliveryLock = NSRecursiveLock()
    private var scopeObservers: [UInt64: (SessionScopeTransition) -> Void] = [:]
    private var nextObserverIdentifier: UInt64 = 0

    init(boundary: ReadAuthenticationBoundary, completionQueue: DispatchQueue = .main) {
        self.boundary = boundary
        self.completionQueue = completionQueue
    }

    var scope: SessionReadScope {
        lock.lock()
        defer { lock.unlock() }
        return currentScope
    }

    /// The reason for the most recent relock, nil if the session never relocked.
    var lastRelockReason: RelockReason? {
        lock.lock()
        defer { lock.unlock() }
        return recordedRelockReason
    }

    /// Returns the authorization for a read made right now, or nil in the capture-only scope.
    func currentAuthorization() -> ReadAuthorization? {
        lock.lock()
        defer { lock.unlock() }
        guard currentScope == .authenticated else { return nil }
        return ReadAuthorization(epoch: accessEpoch)
    }

    func isCurrent(_ authorization: ReadAuthorization) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return currentScope == .authenticated && authorization.epoch == accessEpoch
    }

    /// Registers an observer told of every scope change: each successful attempt, each relock (even when already
    /// capture-only) and each failed or cancelled attempt. A newer attempt superseding an older one is not a change.
    /// Not called for the current scope at registration; read `scope` for that.
    @discardableResult
    func observeScope(_ observer: @escaping (SessionScopeTransition) -> Void) -> SessionScopeObserverToken {
        lock.lock()
        defer { lock.unlock() }
        nextObserverIdentifier += 1
        scopeObservers[nextObserverIdentifier] = observer
        return SessionScopeObserverToken(identifier: nextObserverIdentifier)
    }

    func removeScopeObserver(_ token: SessionScopeObserverToken) {
        lock.lock()
        scopeObservers.removeValue(forKey: token.identifier)
        lock.unlock()
    }

    /// Starts an authentication attempt. The scope changes before `completion` runs (on the completion queue). A newer
    /// attempt supersedes one still in flight, which then reports `interrupted`.
    func authenticate(reason: String, completion: @escaping (ReadAuthenticationResult) -> Void) {
        lock.lock()
        attemptSequence += 1
        let attempt = attemptSequence
        attemptInFlight = attempt
        lock.unlock()

        boundary.authenticate(reason: reason) { [weak self] outcome in
            guard let self else { return }
            let result = self.finish(attempt: attempt, outcome: outcome)
            self.completionQueue.async { completion(result) }
        }
    }

    /// Returns to the capture-only scope and invalidates every authorization and attempt issued so far.
    func relock(_ reason: RelockReason) {
        deliveryLock.lock()
        defer { deliveryLock.unlock() }
        lock.lock()
        let attemptWasInFlight = attemptInFlight != nil
        recordedRelockReason = reason
        revokeLocked()
        let observers = observersInRegistrationOrderLocked()
        lock.unlock()
        if attemptWasInFlight {
            boundary.invalidatePendingAttempt()
        }
        publish(SessionScopeTransition(scope: .captureOnly, cause: .relocked(reason)), to: observers)
    }

    private func finish(attempt: UInt64, outcome: ReadAuthenticationOutcome) -> ReadAuthenticationResult {
        deliveryLock.lock()
        defer { deliveryLock.unlock() }
        lock.lock()
        guard attemptInFlight == attempt else {
            lock.unlock()
            return .notAuthenticated(.interrupted)
        }
        attemptInFlight = nil
        let result: ReadAuthenticationResult
        let transition: SessionScopeTransition
        switch outcome {
        case .succeeded:
            currentScope = .authenticated
            result = .authenticated
            transition = SessionScopeTransition(scope: .authenticated, cause: .authenticated)
        case .cancelled:
            revokeLocked()
            result = .notAuthenticated(.cancelled)
            transition = SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.cancelled))
        case .denied:
            revokeLocked()
            result = .notAuthenticated(.denied)
            transition = SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.denied))
        case .unavailable:
            revokeLocked()
            result = .notAuthenticated(.unavailable)
            transition = SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.unavailable))
        }
        let observers = observersInRegistrationOrderLocked()
        lock.unlock()
        publish(transition, to: observers)
        return result
    }

    private func observersInRegistrationOrderLocked() -> [(SessionScopeTransition) -> Void] {
        scopeObservers.sorted { $0.key < $1.key }.map { $0.value }
    }

    /// Called with `deliveryLock` held and `lock` released, so transitions reach observers in the order they were
    /// applied, and an observer may read the session.
    private func publish(_ transition: SessionScopeTransition, to observers: [(SessionScopeTransition) -> Void]) {
        for observer in observers { observer(transition) }
    }

    private func revokeLocked() {
        currentScope = .captureOnly
        accessEpoch += 1
        attemptInFlight = nil
    }
}
