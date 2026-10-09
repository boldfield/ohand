import UIKit
import XCTest
@testable import OhAndServices

private final class RecordingRedactionPresenter: RedactionPresenter {
    private(set) var calls: [String] = []
    func showRedaction() { calls.append("show") }
    func hideRedaction() { calls.append("hide") }
}

private final class LoggingRedactionPresenter: RedactionPresenter {
    private let log: (String) -> Void
    init(log: @escaping (String) -> Void) { self.log = log }
    func showRedaction() { log("show") }
    func hideRedaction() { log("hide") }
}

final class SessionLifecycleCoordinatorTests: XCTestCase {
    private var center: NotificationCenter!
    private var boundary: FakeReadAuthenticationBoundary!
    private var session: ReadAuthenticationSession!
    private var presenter: RecordingRedactionPresenter!
    private var coordinator: SessionLifecycleCoordinator!

    override func setUp() {
        super.setUp()
        center = NotificationCenter()
        boundary = FakeReadAuthenticationBoundary()
        session = ReadAuthenticationSession(boundary: boundary)
        presenter = RecordingRedactionPresenter()
        coordinator = SessionLifecycleCoordinator(session: session, presenter: presenter, notificationCenter: center)
        coordinator.start()
    }

    func testResigningActiveCoversTheScreenWithoutRelocking() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)

        center.post(name: UIApplication.willResignActiveNotification, object: nil)

        XCTAssertEqual(presenter.calls, ["show"])
        XCTAssertEqual(session.scope, .authenticated, "the authentication sheet resigns active; that must not relock")
    }

    func testEnteringTheBackgroundRelocksAndKeepsTheScreenCovered() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)

        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertEqual(session.lastRelockReason, .enteredBackground)
        XCTAssertEqual(presenter.calls, ["show"])
    }

    func testReturningToTheForegroundRelocksEvenIfTheBackgroundNotificationWasMissed() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)

        center.post(name: UIApplication.willEnterForegroundNotification, object: nil)

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertEqual(session.lastRelockReason, .willEnterForeground)
    }

    func testProtectedDataBecomingUnavailableRelocks() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)

        center.post(name: UIApplication.protectedDataWillBecomeUnavailableNotification, object: nil)

        XCTAssertEqual(session.scope, .captureOnly)
        XCTAssertEqual(session.lastRelockReason, .protectedDataUnavailable)
    }

    func testBecomingActiveUncoversTheScreenButDoesNotRestoreAccess() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        center.post(name: UIApplication.willResignActiveNotification, object: nil)
        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        center.post(name: UIApplication.willEnterForegroundNotification, object: nil)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)

        XCTAssertEqual(presenter.calls.last, "hide")
        XCTAssertEqual(session.scope, .captureOnly)
    }

    func testRelockIsPublishedBeforeTheCoverComesOffOnReturnFromBackground() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        var log: [String] = []
        session.observeScope { transition in
            if case .relocked = transition.cause { log.append("relock-published") }
        }
        let loggingPresenter = LoggingRedactionPresenter(log: { log.append($0) })
        let loggingCoordinator = SessionLifecycleCoordinator(
            session: session, presenter: loggingPresenter, notificationCenter: center)
        coordinator.stop()
        loggingCoordinator.start()

        center.post(name: UIApplication.willResignActiveNotification, object: nil)
        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        center.post(name: UIApplication.willEnterForegroundNotification, object: nil)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)

        guard let hideIndex = log.firstIndex(of: "hide"), let relockIndex = log.firstIndex(of: "relock-published") else {
            return XCTFail("expected a published relock and a hide, got \(log)")
        }
        XCTAssertLessThan(relockIndex, hideIndex, "observers learn of the relock before the cover comes off: \(log)")
        XCTAssertEqual(session.scope, .captureOnly)
        loggingCoordinator.stop()
    }

    func testProtectedDataRelockWhileActiveIsPublishedToObservers() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        var transitions: [SessionScopeTransition] = []
        session.observeScope { transitions.append($0) }

        center.post(name: UIApplication.protectedDataWillBecomeUnavailableNotification, object: nil)

        XCTAssertEqual(transitions, [SessionScopeTransition(scope: .captureOnly, cause: .relocked(.protectedDataUnavailable))])
        XCTAssertEqual(presenter.calls, [], "no cover was needed, but displayed content can still be dropped")
    }

    func testStoppedCoordinatorIgnoresLifecycleNotifications() {
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        coordinator.stop()

        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)

        XCTAssertEqual(presenter.calls, [])
        XCTAssertEqual(session.scope, .authenticated)
    }

    func testWindowPresenterCoversEverySceneAndUncoversThem() throws {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        try XCTSkipIf(scenes.isEmpty, "the test host has no window scene to cover")
        let windowPresenter = WindowRedactionPresenter()

        windowPresenter.showRedaction()
        XCTAssertEqual(windowPresenter.coverWindowCount, scenes.count)
        windowPresenter.showRedaction()
        XCTAssertEqual(windowPresenter.coverWindowCount, scenes.count, "showing twice does not stack covers")

        windowPresenter.hideRedaction()
        XCTAssertEqual(windowPresenter.coverWindowCount, 0)
    }
}
