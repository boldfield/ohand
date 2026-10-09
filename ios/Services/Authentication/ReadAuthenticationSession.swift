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
final class ReadAuthenticationSession {
    private let boundary: ReadAuthenticationBoundary
    private let completionQueue: DispatchQueue
    private let lock = NSLock()
    private var currentScope: SessionReadScope = .captureOnly
    private var accessEpoch: UInt64 = 0
    private var attemptSequence: UInt64 = 0
    private var attemptInFlight: UInt64?
    private var recordedRelockReason: RelockReason?

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
        lock.lock()
        let attemptWasInFlight = attemptInFlight != nil
        recordedRelockReason = reason
        revokeLocked()
        lock.unlock()
        if attemptWasInFlight {
            boundary.invalidatePendingAttempt()
        }
    }

    private func finish(attempt: UInt64, outcome: ReadAuthenticationOutcome) -> ReadAuthenticationResult {
        lock.lock()
        defer { lock.unlock() }
        guard attemptInFlight == attempt else {
            return .notAuthenticated(.interrupted)
        }
        attemptInFlight = nil
        switch outcome {
        case .succeeded:
            currentScope = .authenticated
            return .authenticated
        case .cancelled:
            revokeLocked()
            return .notAuthenticated(.cancelled)
        case .denied:
            revokeLocked()
            return .notAuthenticated(.denied)
        case .unavailable:
            revokeLocked()
            return .notAuthenticated(.unavailable)
        }
    }

    private func revokeLocked() {
        currentScope = .captureOnly
        accessEpoch += 1
        attemptInFlight = nil
    }
}
