import XCTest
@testable import OhAndServices

final class QuietAuthenticationTests: XCTestCase {
    private var boundary: FakeReadAuthenticationBoundary!
    private var session: ReadAuthenticationSession!

    override func setUp() {
        super.setUp()
        boundary = FakeReadAuthenticationBoundary()
        session = ReadAuthenticationSession(boundary: boundary)
    }

    private func quietAndWait() -> ReadAuthenticationResult? {
        var result: ReadAuthenticationResult?
        let completed = expectation(description: "quiet completion")
        XCTAssertTrue(session.authenticateQuietly(reason: "test") {
            result = $0
            completed.fulfill()
        })
        wait(for: [completed], timeout: 5)
        return result
    }

    func testQuietSuccessAuthenticatesTheSession() {
        XCTAssertEqual(quietAndWait(), .authenticated)
        XCTAssertEqual(session.scope, .authenticated)
        XCTAssertEqual(boundary.quietCallCount, 1)
        XCTAssertEqual(boundary.authenticateCallCount, 0)
    }

    func testQuietFailuresLeaveTheSessionCaptureOnly() {
        let expectations: [(ReadAuthenticationOutcome, ReadAuthenticationFailure)] = [
            (.cancelled, .cancelled), (.denied, .denied), (.unavailable, .unavailable),
        ]
        for (outcome, failure) in expectations {
            boundary.nextOutcome = outcome
            XCTAssertEqual(quietAndWait(), .notAuthenticated(failure), "\(outcome)")
            XCTAssertEqual(session.scope, .captureOnly, "\(outcome)")
        }
    }

    func testForegroundDuringAnAttemptDoesNotPromptAgain() {
        boundary.defersCompletion = true
        XCTAssertTrue(session.authenticateQuietly(reason: "first"))
        XCTAssertFalse(session.authenticateQuietly(reason: "second"))
        XCTAssertEqual(boundary.quietCallCount, 1)
    }

    func testAuthenticatedSessionMakesNoQuietAttempt() {
        XCTAssertEqual(quietAndWait(), .authenticated)
        XCTAssertFalse(session.authenticateQuietly(reason: "again"))
        XCTAssertEqual(boundary.quietCallCount, 1)
    }

    func testExplicitUnlockSupersedesAQuietAttempt() {
        boundary.defersCompletion = true
        var quietResult: ReadAuthenticationResult?
        let quietDone = expectation(description: "quiet")
        session.authenticateQuietly(reason: "quiet") {
            quietResult = $0
            quietDone.fulfill()
        }
        var explicitResult: ReadAuthenticationResult?
        let explicitDone = expectation(description: "explicit")
        session.authenticate(reason: "explicit") {
            explicitResult = $0
            explicitDone.fulfill()
        }
        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        boundary.completeDeferredAttempt(at: 1, with: .succeeded)
        wait(for: [quietDone, explicitDone], timeout: 5)
        XCTAssertEqual(quietResult, .notAuthenticated(.interrupted))
        XCTAssertEqual(explicitResult, .authenticated)
        XCTAssertEqual(session.scope, .authenticated)
    }

    func testSavingStaysAvailableWhileAQuietAttemptIsInFlight() {
        boundary.defersCompletion = true
        XCTAssertTrue(session.authenticateQuietly(reason: "quiet"))
        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertNil(session.currentAuthorization(), "capture path needs no read authorization")
    }
}
