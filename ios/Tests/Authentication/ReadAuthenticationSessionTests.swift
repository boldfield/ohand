import LocalAuthentication
import XCTest
@testable import OhAndServices

final class ReadAuthenticationSessionTests: XCTestCase {
    private var boundary: FakeReadAuthenticationBoundary!
    private var session: ReadAuthenticationSession!

    override func setUp() {
        super.setUp()
        boundary = FakeReadAuthenticationBoundary()
        session = ReadAuthenticationSession(boundary: boundary)
    }

    func testSessionStartsCaptureOnlyAndGrantsNoAuthorization() {
        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertNil(session.currentAuthorization())
        XCTAssertNil(session.lastRelockReason)
    }

    func testSuccessfulAttemptAuthenticatesTheSession() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        XCTAssertEqual(session.scope, .authenticated)
        XCTAssertNotNil(session.currentAuthorization())
    }

    func testCancelledDeniedAndUnavailableAttemptsLeaveTheSessionCaptureOnly() {
        let expectations: [(ReadAuthenticationOutcome, ReadAuthenticationFailure)] = [
            (.cancelled, .cancelled), (.denied, .denied), (.unavailable, .unavailable),
        ]
        for (outcome, failure) in expectations {
            boundary.nextOutcome = outcome
            XCTAssertEqual(authenticateAndWait(session), .notAuthenticated(failure), "\(outcome)")
            XCTAssertEqual(session.scope, .captureOnly, "\(outcome)")
            XCTAssertNil(session.currentAuthorization(), "\(outcome)")
        }
    }

    func testFailedReauthenticationRevokesAnAuthenticatedSession() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        let authorization = session.currentAuthorization()
        XCTAssertNotNil(authorization)

        boundary.nextOutcome = .cancelled
        XCTAssertEqual(authenticateAndWait(session), .notAuthenticated(.cancelled))

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertFalse(session.isCurrent(authorization!), "an authorization from before the failed attempt is stale")
    }

    func testRelockReturnsToCaptureOnlyAndStalesEarlierAuthorizations() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        let authorization = session.currentAuthorization()!
        XCTAssertTrue(session.isCurrent(authorization))

        session.relock(.enteredBackground)

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertEqual(session.lastRelockReason, .enteredBackground)
        XCTAssertFalse(session.isCurrent(authorization))
        XCTAssertNil(session.currentAuthorization())
    }

    func testAuthorizationFromBeforeRelockStaysStaleAfterAuthenticatingAgain() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        let before = session.currentAuthorization()!
        session.relock(.explicit)
        XCTAssertEqual(authenticateAndWait(session), .authenticated)

        XCTAssertFalse(session.isCurrent(before))
        XCTAssertTrue(session.isCurrent(session.currentAuthorization()!))
    }

    func testRelockDuringAnAttemptInvalidatesItAndIgnoresALateSuccess() {
        boundary.defersCompletion = true
        var result: ReadAuthenticationResult?
        let completed = expectation(description: "interrupted attempt completes")
        session.authenticate(reason: "test") {
            result = $0
            completed.fulfill()
        }

        session.relock(.enteredBackground)
        XCTAssertEqual(boundary.invalidateCallCount, 1, "the showing prompt was dismissed")
        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        wait(for: [completed], timeout: 5)

        XCTAssertEqual(result, .notAuthenticated(.interrupted))
        XCTAssertEqual(session.scope, .captureOnly, "a success that arrives after a relock grants nothing")
    }

    func testRelockWithoutAnAttemptDoesNotTouchThePrompt() {
        session.relock(.willEnterForeground)
        XCTAssertEqual(boundary.invalidateCallCount, 0)
    }

    func testNewerAttemptSupersedesOneStillInFlight() {
        boundary.defersCompletion = true
        var firstResult: ReadAuthenticationResult?
        var secondResult: ReadAuthenticationResult?
        let firstCompleted = expectation(description: "first completes")
        let secondCompleted = expectation(description: "second completes")
        session.authenticate(reason: "first") {
            firstResult = $0
            firstCompleted.fulfill()
        }
        session.authenticate(reason: "second") {
            secondResult = $0
            secondCompleted.fulfill()
        }

        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        boundary.completeDeferredAttempt(at: 1, with: .succeeded)
        wait(for: [firstCompleted, secondCompleted], timeout: 5)

        XCTAssertEqual(firstResult, .notAuthenticated(.interrupted))
        XCTAssertEqual(secondResult, .authenticated)
        XCTAssertEqual(session.scope, .authenticated)
    }

    func testObserverIsToldOfSuccessRelockAndFailedOrCancelledReauthentication() {
        var transitions: [SessionScopeTransition] = []
        session.observeScope { transitions.append($0) }

        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        session.relock(.enteredBackground)
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        boundary.nextOutcome = .cancelled
        XCTAssertEqual(authenticateAndWait(session), .notAuthenticated(.cancelled))
        boundary.nextOutcome = .denied
        XCTAssertEqual(authenticateAndWait(session), .notAuthenticated(.denied))
        boundary.nextOutcome = .unavailable
        XCTAssertEqual(authenticateAndWait(session), .notAuthenticated(.unavailable))

        XCTAssertEqual(transitions, [
            SessionScopeTransition(scope: .authenticated, cause: .authenticated),
            SessionScopeTransition(scope: .captureOnly, cause: .relocked(.enteredBackground)),
            SessionScopeTransition(scope: .authenticated, cause: .authenticated),
            SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.cancelled)),
            SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.denied)),
            SessionScopeTransition(scope: .captureOnly, cause: .authenticationFailed(.unavailable)),
        ])
    }

    func testObserverRunsAfterTheScopeChangedAndBeforeRelockReturns() {
        var scopeSeenByObserver: SessionReadScope?
        var observerHasRun = false
        session.observeScope { _ in
            scopeSeenByObserver = self.session.scope
            observerHasRun = true
        }
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        observerHasRun = false

        session.relock(.protectedDataUnavailable)

        XCTAssertTrue(observerHasRun, "the relock was published before relock returned")
        XCTAssertEqual(scopeSeenByObserver, .captureOnly)
    }

    func testRelockWhileAlreadyCaptureOnlyIsStillPublished() {
        var transitions: [SessionScopeTransition] = []
        session.observeScope { transitions.append($0) }

        session.relock(.willEnterForeground)

        XCTAssertEqual(transitions, [SessionScopeTransition(scope: .captureOnly, cause: .relocked(.willEnterForeground))])
    }

    func testSupersededAttemptIsNotPublishedAsAScopeChange() {
        boundary.defersCompletion = true
        var transitions: [SessionScopeTransition] = []
        session.observeScope { transitions.append($0) }
        let secondCompleted = expectation(description: "second completes")
        session.authenticate(reason: "first") { _ in }
        session.authenticate(reason: "second") { _ in secondCompleted.fulfill() }

        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        XCTAssertEqual(transitions, [])
        boundary.completeDeferredAttempt(at: 1, with: .succeeded)
        wait(for: [secondCompleted], timeout: 5)

        XCTAssertEqual(transitions, [SessionScopeTransition(scope: .authenticated, cause: .authenticated)])
    }

    func testRemovedObserverIsNotToldAndOthersStillAre() {
        var removedCalls = 0
        var keptCalls = 0
        let removed = session.observeScope { _ in removedCalls += 1 }
        session.observeScope { _ in keptCalls += 1 }
        session.removeScopeObserver(removed)

        session.relock(.explicit)

        XCTAssertEqual(removedCalls, 0)
        XCTAssertEqual(keptCalls, 1)
    }

    func testObserversAreToldInRegistrationOrder() {
        var order: [Int] = []
        session.observeScope { _ in order.append(1) }
        session.observeScope { _ in order.append(2) }

        session.relock(.explicit)

        XCTAssertEqual(order, [1, 2])
    }

    func testPlatformErrorsMapToNonSuccessOutcomesAndNeverToSuccess() {
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: true, error: nil), .succeeded)
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: false, error: LAError(.userCancel)), .cancelled)
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: false, error: LAError(.systemCancel)), .cancelled)
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: false, error: LAError(.appCancel)), .cancelled)
        XCTAssertEqual(
            LocalAuthenticationBoundary.outcome(success: false, error: LAError(.authenticationFailed)), .denied)
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: false, error: LAError(.passcodeNotSet)), .unavailable)
        XCTAssertEqual(LocalAuthenticationBoundary.outcome(success: false, error: nil), .denied)
        XCTAssertEqual(
            LocalAuthenticationBoundary.outcome(success: false, error: NSError(domain: "synthetic", code: 1)), .denied)
    }
}
