import UIKit
import XCTest
@testable import OhAnd
@testable import OhAndServices

final class RecordingRedactionPresenter: RedactionPresenter {
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

final class FakeReadLock: ReadLockState {
    var readsAreUnlocked: Bool
    private var handlers: [() -> Void] = []

    init(unlocked: Bool) { readsAreUnlocked = unlocked }

    func observeReadScopeChanges(_ handler: @escaping () -> Void) -> AnyObject {
        handlers.append(handler)
        return NSObject()
    }

    func setUnlocked(_ unlocked: Bool) {
        readsAreUnlocked = unlocked
        handlers.forEach { $0() }
    }
}

final class SceneCoverControllerTests: XCTestCase {
    private var presenter: RecordingRedactionPresenter!
    private var readLock: FakeReadLock!
    private var cover: SceneCoverController!

    override func setUp() {
        super.setUp()
        presenter = RecordingRedactionPresenter()
        readLock = FakeReadLock(unlocked: true)
        cover = SceneCoverController(presenter: presenter, readLock: readLock)
    }

    func testResigningActiveShowsTheCover() {
        cover.sceneWillResignActive()
        XCTAssertEqual(presenter.calls, ["show"])
        XCTAssertTrue(cover.isCovered)
    }

    func testBecomingActiveRemovesTheCover() {
        cover.sceneWillResignActive()
        cover.sceneDidBecomeActive()
        XCTAssertEqual(presenter.calls, ["show", "hide"])
        XCTAssertFalse(cover.isCovered)
    }

    func testBecomingActiveWithoutResigningNeverShowsTheCover() {
        cover.sceneDidBecomeActive()
        XCTAssertEqual(presenter.calls, [])
    }

    func testResigningTwiceCoversOnce() {
        cover.sceneWillResignActive()
        cover.sceneWillResignActive()
        XCTAssertEqual(presenter.calls, ["show"])
    }

    func testResigningWhileLockedShowsNoCover() {
        readLock.setUnlocked(false)
        cover.sceneWillResignActive()
        cover.sceneDidBecomeActive()
        XCTAssertEqual(presenter.calls, [])
    }

    func testUnlockingWhileInactiveShowsTheCoverUntilActive() {
        readLock.setUnlocked(false)
        cover.sceneWillResignActive()
        readLock.setUnlocked(true)
        XCTAssertEqual(presenter.calls, ["show"])
        cover.sceneDidBecomeActive()
        XCTAssertEqual(presenter.calls, ["show", "hide"])
    }

    func testRelockingWhileCoveredKeepsTheCoverUntilActive() {
        cover.sceneDidEnterBackground()
        readLock.setUnlocked(false)
        XCTAssertEqual(presenter.calls, ["show"], "the relock must not bring the cover down before the snapshot")
        XCTAssertTrue(cover.isCovered)
        cover.sceneDidBecomeActive()
        XCTAssertEqual(presenter.calls, ["show", "hide"])
    }

    func testRelockAndUnlockWhileCoveredNeitherRemovesNorStacksTheCover() {
        cover.sceneWillResignActive()
        readLock.setUnlocked(false)
        readLock.setUnlocked(true)
        XCTAssertEqual(presenter.calls, ["show"])
    }

    func testUnlockingWhileActiveShowsNoCover() {
        readLock.setUnlocked(false)
        cover.sceneDidBecomeActive()
        readLock.setUnlocked(true)
        XCTAssertEqual(presenter.calls, [])
    }

    func testEnteringTheBackgroundWhileUnlockedShowsTheCover() {
        cover.sceneDidEnterBackground()
        XCTAssertEqual(presenter.calls, ["show"])
    }

    func testEnteringTheBackgroundWhileLockedShowsNoCover() {
        readLock.setUnlocked(false)
        cover.sceneDidEnterBackground()
        XCTAssertEqual(presenter.calls, [])
    }

    func testACaptureURLEntryWhileCoveredEndsUncoveredOnceTheSceneIsActive() {
        let center = NotificationCenter()
        let handoff = CaptureEntryHandoff(notificationCenter: center, makeEntryID: { "entry-url" })
        let entrySession = CaptureEntrySession(
            handoff: handoff, defaultMode: { .silent },
            access: { CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true) }, notificationCenter: center)
        _ = entrySession.willEnterForeground()
        entrySession.didEnterBackground()
        cover.sceneWillResignActive()

        XCTAssertNil(entrySession.receive(url: URL(string: "ohand-capture://capture")!), "pending until foreground")
        XCTAssertTrue(handoff.hasPending)
        XCTAssertTrue(cover.isCovered, "the URL alone does not remove the cover")

        XCTAssertNotNil(entrySession.willEnterForeground(), "the capture surface is presented")
        cover.sceneDidBecomeActive()
        XCTAssertFalse(cover.isCovered)
        XCTAssertEqual(presenter.calls, ["show", "hide"])
    }

    func testAHandoffAnnouncementWhileCoveredEndsUncoveredOnceTheSceneIsActive() {
        let center = NotificationCenter()
        let handoff = CaptureEntryHandoff(notificationCenter: center, makeEntryID: { "entry-control" })
        let entrySession = CaptureEntrySession(
            handoff: handoff, defaultMode: { .voice },
            access: { CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true) }, notificationCenter: center)
        _ = entrySession.willEnterForeground()
        entrySession.didEnterBackground()
        cover.sceneWillResignActive()

        handoff.registerAndAnnounce(source: .controlIntent, requestedMode: nil)
        XCTAssertTrue(cover.isCovered, "the announcement alone does not remove the cover")

        XCTAssertEqual(entrySession.willEnterForeground()?.source, .controlIntent)
        cover.sceneDidBecomeActive()
        XCTAssertFalse(cover.isCovered)
        XCTAssertEqual(presenter.calls, ["show", "hide"])
    }

    func testAnUnrelatedURLWhileCoveredLeavesTheCoverUp() {
        let center = NotificationCenter()
        let handoff = CaptureEntryHandoff(notificationCenter: center)
        let entrySession = CaptureEntrySession(
            handoff: handoff, defaultMode: { .silent },
            access: { CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true) }, notificationCenter: center)
        cover.sceneWillResignActive()

        XCTAssertNil(entrySession.receive(url: URL(string: "https://example.com/other")!))
        XCTAssertFalse(handoff.hasPending)
        XCTAssertTrue(cover.isCovered)
        XCTAssertEqual(presenter.calls, ["show"])
    }
}

final class WindowRedactionPresenterArtworkTests: XCTestCase {
    private func hostScene() throws -> UIWindowScene {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        return try XCTUnwrap(scenes.first, "the test host has no window scene to cover")
    }

    func testTheDefaultCoverUsesTheShippedLaunchBackgroundAndMark() throws {
        let scene = try hostScene()
        let launchBackground = try XCTUnwrap(UIColor(named: "LaunchBackground"), "launch colour is in the app bundle")
        let launchMark = try XCTUnwrap(UIImage(named: "LaunchMark"), "launch mark is in the app bundle")
        let presenter = WindowRedactionPresenter(windowScenes: { [scene] in [scene] })

        let view = presenter.makeCoverView()

        XCTAssertEqual(view.backgroundColor, launchBackground)
        let marks = view.subviews.compactMap { $0 as? UIImageView }
        XCTAssertEqual(marks.count, 1)
        XCTAssertTrue(marks.first?.image === launchMark || marks.first?.image?.pngData() == launchMark.pngData())
        XCTAssertTrue(view.subviews.allSatisfy { !($0 is UILabel) }, "no text on the cover")
    }

    func testTheRaisedCoverIsAttachedAsAScreenshotProxyForTheAppSwitcher() throws {
        let scene = try hostScene()
        let presenter = WindowRedactionPresenter(windowScenes: { [scene] in [scene] })

        presenter.showRedaction()
        defer { presenter.hideRedaction() }
        let coverWindow = try XCTUnwrap(scene.windows.first { $0.windowLevel == UIWindow.Level.alert + 1 })
        coverWindow.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: coverWindow.bounds).image { _ in
            coverWindow.drawHierarchy(in: coverWindow.bounds, afterScreenUpdates: true)
        }

        let attachment = XCTAttachment(image: image)
        attachment.name = "app-switcher-cover"
        attachment.lifetime = .keepAlways
        add(attachment)
        XCTAssertGreaterThan(image.size.width, 0)
    }
}
