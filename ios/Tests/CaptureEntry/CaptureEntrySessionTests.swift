import XCTest
@testable import OhAnd

final class CaptureEntryHandoffTests: XCTestCase {
    private var mintedIDs = 0
    private var handoff: CaptureEntryHandoff!

    override func setUp() {
        super.setUp()
        mintedIDs = 0
        handoff = CaptureEntryHandoff(notificationCenter: NotificationCenter(), makeEntryID: { [unowned self] in
            mintedIDs += 1
            return "entry-\(mintedIDs)"
        })
    }

    func testAPlainLaunchCreatesNoEntry() {
        XCTAssertFalse(handoff.hasPending)
        XCTAssertNil(handoff.takePending())
        XCTAssertEqual(mintedIDs, 0)
    }

    func testTheEntryIDIsMintedOncePerHandoffAndTakenOnce() {
        let first = handoff.register(source: .controlIntent, requestedMode: nil)
        let second = handoff.register(source: .controlIntent, requestedMode: nil)

        XCTAssertEqual(first.entryID, second.entryID, "a repeated perform adopts the pending request")
        XCTAssertEqual(mintedIDs, 1)
        XCTAssertEqual(handoff.takePending()?.entryID, "entry-1")
        XCTAssertNil(handoff.takePending(), "one press is presented once")
    }

    func testARequestAfterOneWasTakenIsANewEntry() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        _ = handoff.takePending()

        let next = handoff.register(source: .shortcutURL, requestedMode: nil)

        XCTAssertEqual(next.entryID, "entry-2")
        XCTAssertEqual(next.source, .shortcutURL)
    }

    func testTheNewestExplicitModeWinsAndAnUnspecifiedModeKeepsTheEarlierOne() {
        handoff.register(source: .controlIntent, requestedMode: .voice)
        XCTAssertEqual(handoff.register(source: .controlIntent, requestedMode: nil).requestedMode, .voice)
        XCTAssertEqual(handoff.register(source: .shortcutURL, requestedMode: .silent).requestedMode, .silent)
    }

    func testAnnouncingPostsTheEntryID() {
        let center = NotificationCenter()
        let announcing = CaptureEntryHandoff(notificationCenter: center, makeEntryID: { "entry-x" })
        var announcedIDs: [String] = []
        let token = center.addObserver(forName: CaptureEntryHandoff.registeredNotification, object: announcing, queue: nil) {
            announcedIDs.append($0.userInfo?["entryID"] as? String ?? "")
        }
        defer { center.removeObserver(token) }

        announcing.registerAndAnnounce(source: .controlIntent, requestedMode: nil)

        XCTAssertEqual(announcedIDs, ["entry-x"])
    }

    func testDestinationsMapToModes() {
        XCTAssertNil(CaptureDestination.capture.requestedMode)
        XCTAssertEqual(CaptureDestination.voice.requestedMode, .voice)
        XCTAssertEqual(CaptureDestination.silent.requestedMode, .silent)
    }
}

final class CaptureEntrySessionTests: XCTestCase {
    private var center: NotificationCenter!
    private var handoff: CaptureEntryHandoff!
    private var mintedIDs = 0
    private var access = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true)
    private var defaultMode = CaptureEntryMode.silent
    private var session: CaptureEntrySession!

    override func setUp() {
        super.setUp()
        mintedIDs = 0
        center = NotificationCenter()
        handoff = CaptureEntryHandoff(notificationCenter: center, makeEntryID: { [unowned self] in
            mintedIDs += 1
            return "entry-\(mintedIDs)"
        })
        session = CaptureEntrySession(
            handoff: handoff, defaultMode: { [unowned self] in defaultMode }, access: { [unowned self] in access },
            notificationCenter: center)
    }

    func testAPlainForegroundPresentsNothing() {
        XCTAssertNil(session.willEnterForeground())
        XCTAssertEqual(mintedIDs, 0)
    }

    func testAColdLaunchEntryUsesTheDefaultModeAndIsClassifiedCold() {
        handoff.register(source: .controlIntent, requestedMode: nil)

        let presentation = session.willEnterForeground()

        XCTAssertEqual(presentation?.launchKind, .cold)
        XCTAssertEqual(presentation?.mode, .silent)
        XCTAssertEqual(presentation?.source, .controlIntent)
    }

    func testAWarmEntryIsClassifiedWarmAndHonorsTheRequestedMode() {
        XCTAssertNil(session.willEnterForeground())
        session.didEnterBackground()
        handoff.register(source: .controlIntent, requestedMode: .voice)

        let presentation = session.willEnterForeground()

        XCTAssertEqual(presentation?.launchKind, .warm)
        XCTAssertEqual(presentation?.mode, .voice)
    }

    func testAnEntryWhileTheSceneIsInTheForegroundIsPresentedWithoutWaiting() {
        XCTAssertNil(session.willEnterForeground())

        let presentation = session.receive(url: URL(string: "ohand-capture://capture?mode=voice")!)

        XCTAssertEqual(presentation?.mode, .voice)
        XCTAssertEqual(presentation?.source, .shortcutURL)
        XCTAssertEqual(presentation?.launchKind, .cold)
    }

    func testAnEntryThatArrivesInTheBackgroundStaysPendingUntilForeground() {
        session.didEnterBackground()

        XCTAssertNil(session.receive(url: URL(string: "ohand-capture://capture?mode=silent")!))
        XCTAssertTrue(handoff.hasPending)

        let presentation = session.willEnterForeground()

        XCTAssertEqual(presentation?.mode, .silent)
        XCTAssertFalse(handoff.hasPending)
    }

    func testTheIntentAndTheForegroundTogetherPresentOneEntry() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        handoff.register(source: .controlIntent, requestedMode: nil)

        XCTAssertNotNil(session.willEnterForeground())
        session.didEnterBackground()
        XCTAssertNil(session.willEnterForeground(), "a request is presented at most once")
        XCTAssertEqual(mintedIDs, 1)
    }

    func testPresentationCarriesTheCurrentAccessAndNoStoredContent() {
        access = CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: false)
        handoff.register(source: .controlIntent, requestedMode: nil)

        let presentation = session.willEnterForeground()

        XCTAssertEqual(presentation?.access, access)
        XCTAssertFalse(presentation?.access.canReadStoredStatus ?? true)
    }

    func testTheObserverPresentsAnAnnouncedEntryOnlyInTheForeground() {
        var presented: [CaptureEntryPresentation] = []
        session.observeHandoffs { presented.append($0) }

        handoff.registerAndAnnounce(source: .controlIntent, requestedMode: nil)
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))
        XCTAssertTrue(presented.isEmpty)
        XCTAssertTrue(handoff.hasPending)

        XCTAssertNotNil(session.willEnterForeground())
        handoff.registerAndAnnounce(source: .controlIntent, requestedMode: .voice)
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))

        XCTAssertEqual(presented.map(\.mode), [.voice])
    }

    func testShortcutURLParsing() {
        XCTAssertEqual(CaptureEntryURL.parse(URL(string: "ohand-capture://capture")!).isEntry, true)
        XCTAssertNil(CaptureEntryURL.parse(URL(string: "ohand-capture://capture")!).requestedMode)
        XCTAssertEqual(CaptureEntryURL.parse(URL(string: "ohand-capture://capture?mode=VOICE")!).requestedMode, .voice)
        XCTAssertNil(CaptureEntryURL.parse(URL(string: "ohand-capture://capture?mode=other")!).requestedMode)
        XCTAssertFalse(CaptureEntryURL.parse(URL(string: "ohand-capture://settings")!).isEntry)
        XCTAssertFalse(CaptureEntryURL.parse(URL(string: "https://example.com/capture")!).isEntry)
    }

    func testAnUnrelatedURLCreatesNoEntry() {
        XCTAssertNil(session.willEnterForeground())
        XCTAssertNil(session.receive(url: URL(string: "https://example.com/capture")!))
        XCTAssertFalse(handoff.hasPending)
    }
}
