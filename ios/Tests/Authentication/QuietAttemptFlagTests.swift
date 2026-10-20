import XCTest
@testable import OhAndServices

final class QuietAttemptFlagTests: XCTestCase {
    private var boundary: FakeReadAuthenticationBoundary!
    private var session: ReadAuthenticationSession!
    private var observedFlags: [Bool] = []

    override func setUp() {
        super.setUp()
        boundary = FakeReadAuthenticationBoundary()
        session = ReadAuthenticationSession(boundary: boundary, completionQueue: DispatchQueue(label: "quiet-flag-tests"))
        observedFlags = []
        session.observeQuietAttempt { [unowned self] in observedFlags.append(session.isQuietAttemptInFlight) }
    }

    func testTheFlagRisesAndFallsAroundAQuietAttempt() {
        boundary.defersCompletion = true
        XCTAssertFalse(session.isQuietAttemptInFlight)
        XCTAssertTrue(session.authenticateQuietly(reason: "quiet"))
        XCTAssertTrue(session.isQuietAttemptInFlight)
        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        XCTAssertFalse(session.isQuietAttemptInFlight)
        XCTAssertEqual(observedFlags, [true, false])
    }

    func testAFailedQuietAttemptLowersTheFlagAndLeavesTheSessionCaptureOnly() {
        boundary.defersCompletion = true
        session.authenticateQuietly(reason: "quiet")
        boundary.completeDeferredAttempt(at: 0, with: .unavailable)
        XCTAssertEqual(observedFlags, [true, false])
        XCTAssertEqual(session.scope, .captureOnly)
    }

    func testAnExplicitAttemptSupersedingAQuietOneLowersTheFlag() {
        boundary.defersCompletion = true
        session.authenticateQuietly(reason: "quiet")
        session.authenticate(reason: "explicit") { _ in }
        XCTAssertFalse(session.isQuietAttemptInFlight)
        XCTAssertEqual(observedFlags, [true, false])
    }

    func testAnExplicitAttemptAloneNeverRaisesTheFlag() {
        boundary.defersCompletion = true
        session.authenticate(reason: "explicit") { _ in }
        XCTAssertFalse(session.isQuietAttemptInFlight)
        XCTAssertEqual(observedFlags, [])
    }

    func testARelockDuringAQuietAttemptLowersTheFlag() {
        boundary.defersCompletion = true
        session.authenticateQuietly(reason: "quiet")
        session.relock(.enteredBackground)
        XCTAssertEqual(observedFlags, [true, false])
    }

    func testObservingTheFlagChangesNoScope() {
        _ = session.isQuietAttemptInFlight
        XCTAssertEqual(session.scope, .captureOnly)
        boundary.defersCompletion = true
        session.authenticateQuietly(reason: "quiet")
        _ = session.isQuietAttemptInFlight
        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertNil(session.currentAuthorization())
        XCTAssertEqual(boundary.quietCallCount, 1)
    }
}
