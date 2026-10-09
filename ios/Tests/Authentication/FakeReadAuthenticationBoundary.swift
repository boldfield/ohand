import XCTest
@testable import OhAndServices

/// Produces authentication outcomes on demand. Completes immediately with `nextOutcome`, or holds completions so a
/// test can relock or supersede an attempt while it is still showing.
final class FakeReadAuthenticationBoundary: ReadAuthenticationBoundary {
    var nextOutcome: ReadAuthenticationOutcome = .succeeded
    var defersCompletion = false
    private(set) var authenticateCallCount = 0
    private(set) var invalidateCallCount = 0
    private(set) var deferredCompletions: [(ReadAuthenticationOutcome) -> Void] = []

    func authenticate(reason: String, completion: @escaping (ReadAuthenticationOutcome) -> Void) {
        authenticateCallCount += 1
        if defersCompletion {
            deferredCompletions.append(completion)
        } else {
            completion(nextOutcome)
        }
    }

    func invalidatePendingAttempt() {
        invalidateCallCount += 1
    }

    func completeDeferredAttempt(at index: Int, with outcome: ReadAuthenticationOutcome) {
        deferredCompletions[index](outcome)
    }
}

extension XCTestCase {
    /// Runs one attempt against an immediately completing boundary and waits for the session's completion.
    func authenticateAndWait(_ session: ReadAuthenticationSession) -> ReadAuthenticationResult? {
        var result: ReadAuthenticationResult?
        let completed = expectation(description: "authentication completion")
        session.authenticate(reason: "test") {
            result = $0
            completed.fulfill()
        }
        wait(for: [completed], timeout: 5)
        return result
    }
}
