import Foundation
import LocalAuthentication

/// How one native authentication attempt ended.
enum ReadAuthenticationOutcome: Equatable {
    case succeeded
    case cancelled
    case denied
    /// No usable authentication method (for example no device passcode). Treated as a failure: the session stays
    /// capture-only.
    case unavailable
}

/// Narrow seam over the platform authentication prompt. Production uses `LocalAuthenticationBoundary`; tests inject a
/// fake to produce outcomes and orderings a simulator cannot be made to return.
protocol ReadAuthenticationBoundary: AnyObject {
    /// Starts one attempt. `completion` may run on any queue, exactly once.
    func authenticate(reason: String, completion: @escaping (ReadAuthenticationOutcome) -> Void)
    /// Dismisses an attempt that is still showing. The abandoned attempt's completion may still run; the session
    /// ignores it.
    func invalidatePendingAttempt()
}

/// Device-owner authentication: biometrics with the device passcode as the fallback, so a user without enrolled
/// biometrics can still read.
final class LocalAuthenticationBoundary: ReadAuthenticationBoundary {
    private let lock = NSLock()
    private var pendingContext: LAContext?

    func authenticate(reason: String, completion: @escaping (ReadAuthenticationOutcome) -> Void) {
        let context = LAContext()
        var policyError: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &policyError) else {
            completion(.unavailable)
            return
        }
        lock.lock()
        let supersededContext = pendingContext
        pendingContext = context
        lock.unlock()
        supersededContext?.invalidate()
        context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) { [weak self] success, error in
            self?.clear(context)
            completion(Self.outcome(success: success, error: error))
        }
    }

    func invalidatePendingAttempt() {
        lock.lock()
        let context = pendingContext
        pendingContext = nil
        lock.unlock()
        context?.invalidate()
    }

    private func clear(_ context: LAContext) {
        lock.lock()
        if pendingContext === context { pendingContext = nil }
        lock.unlock()
    }

    /// Fails closed: anything that is not an explicit success is a non-success outcome.
    static func outcome(success: Bool, error: Error?) -> ReadAuthenticationOutcome {
        if success { return .succeeded }
        guard let authenticationError = error as? LAError else { return .denied }
        switch authenticationError.code {
        case .userCancel, .systemCancel, .appCancel:
            return .cancelled
        case .passcodeNotSet, .biometryNotAvailable, .notInteractive:
            return .unavailable
        default:
            return .denied
        }
    }
}
