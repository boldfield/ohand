import OhAndCoreBridge
import SwiftUI
import UIKit
import XCTest
@testable import OhAnd

private struct SyntheticReadFailure: Error {}

/// Hosts the real home body in a window over the real model and a scripted session. The body reports where each visible
/// part landed and the words it shows; the tests read that report, and tap a button by running the action it reports.
@MainActor
final class HomeBodyViewTests: XCTestCase {
    private static let phone = CGSize(width: 390, height: 844)
    private static let tablet = CGSize(width: 1024, height: 1366)
    private static let minimumTouchTarget: CGFloat = 44
    private static let todayText = "Synthetic note from this morning"
    private static let earlierText = "Synthetic idea from last week"

    /// Noon today in the simulator's own time zone, so "today" and "earlier" do not depend on where the runner is.
    private let referenceNow = Calendar.current.date(bySettingHour: 12, minute: 0, second: 0, of: Date())!
    private var source: FakeHomeRecentsSource!
    private var model: HomeRecentsModel?
    private var rendered: [String: HomeRenderedElement] = [:]
    private var window: UIWindow?
    private var openedItems: [String] = []

    override func setUp() {
        super.setUp()
        source = FakeHomeRecentsSource()
        rendered = [:]
        openedItems = []
    }

    override func tearDown() {
        model?.shutDown()
        model = nil
        window?.isHidden = true
        window = nil
        super.tearDown()
    }

    private func timestamp(hoursBefore hours: Double) -> String {
        ISO8601DateFormatter().string(from: referenceNow.addingTimeInterval(-hours * 3600))
    }

    private func item(_ identifier: String, type: String?, text: String, hoursBefore hours: Double) -> CoreRecentItem {
        CoreRecentItem(
            itemID: identifier, revision: 1, itemType: type, displayText: text,
            capturedAt: timestamp(hoursBefore: hours), sourceKind: .text)
    }

    private var todayItem: CoreRecentItem { item("today-1", type: "note", text: Self.todayText, hoursBefore: 1) }
    private var earlierItem: CoreRecentItem { item("earlier-1", type: "idea", text: Self.earlierText, hoursBefore: 24 * 7) }

    /// Builds the model over the session as the test left it and shows the body. Poll pauses never end, so no real time
    /// passes and no read is started behind the test's back.
    @discardableResult
    private func show(
        size: CGSize = HomeBodyViewTests.phone, sizeClass: UserInterfaceSizeClass = .compact,
        waitingFor identifiers: [String] = [], thenSettling: Bool = false,
        file: StaticString = #filePath, line: UInt = #line
    ) async -> HomeRecentsModel {
        let pauses = PollPauses()
        let shownModel = HomeRecentsModel(
            source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        model = shownModel
        let root = HomeBodyView(model: shownModel, openItem: { [unowned self] in openedItems.append($0) }, renderReporter: { [unowned self] in rendered = $0 })
            .environment(\.horizontalSizeClass, sizeClass)
        let shown = UIWindow(frame: CGRect(origin: .zero, size: size))
        shown.rootViewController = UIHostingController(rootView: root)
        shown.makeKeyAndVisible()
        window = shown
        await waitForRendered(identifiers, model: shownModel, file: file, line: line)
        if thenSettling { await settle() }
        return shownModel
    }

    /// Lays the hosted view out once, so what the body reports is current.
    private func layOut() {
        window?.rootViewController?.view.setNeedsLayout()
        window?.rootViewController?.view.layoutIfNeeded()
    }

    /// Waits, laying the view out between checks, until the model has no read in flight and the body has reported every
    /// one of the elements the test goes on to read.
    private func waitForRendered(
        _ identifiers: [String], model waitedModel: HomeRecentsModel, file: StaticString = #filePath, line: UInt = #line
    ) async {
        await waitUntil(
            "the body has reported \(identifiers.isEmpty ? "its first render" : identifiers.joined(separator: ", "))",
            file: file, line: line
        ) {
            layOut()
            return !rendered.isEmpty && !waitedModel.isLoadingFirstPage && !waitedModel.isLoadingMore
                && waitedModel.readsInFlight == 0 && identifiers.allSatisfy { rendered[$0] != nil }
        }
        layOut()
    }

    /// Waits, laying the view out between checks, until the condition on what the body reported holds.
    private func waitForRendered(
        _ condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) async {
        await waitUntil(condition, file: file, line: line) {
            layOut()
            return holds()
        }
        layOut()
    }

    private static var placeholderIdentifiers: [String] {
        [HomeBodyView.placeholderIdentifier]
            + (0..<HomeBodyView.placeholderCount).map { HomeBodyView.placeholderCardIdentifierPrefix + String($0) }
    }

    private static func cardIdentifier(_ itemID: String) -> String { HomeBodyView.cardIdentifierPrefix + itemID }

    /// A fixed run of layout passes and sleeps. Only for proving that nothing appears (or after a positive wait for what
    /// should have appeared): a fixed wait cannot tell whether the body has finished rendering.
    private func settle() async {
        for _ in 0..<3 {
            for _ in 0..<200 { await Task.yield() }
            window?.rootViewController?.view.setNeedsLayout()
            window?.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        window?.rootViewController?.view.layoutIfNeeded()
    }

    private func frame(_ identifier: String, file: StaticString = #filePath, line: UInt = #line) -> CGRect {
        guard let element = rendered[identifier] else {
            XCTFail("\(identifier) is shown", file: file, line: line)
            return .zero
        }
        return element.frame
    }

    private func tap(_ identifier: String, file: StaticString = #filePath, line: UInt = #line) {
        guard let action = rendered[identifier]?.tap else {
            return XCTFail("\(identifier) is a button on screen", file: file, line: line)
        }
        XCTAssertGreaterThanOrEqual(
            rendered[identifier]?.frame.height ?? 0, Self.minimumTouchTarget, identifier, file: file, line: line)
        action()
    }

    private var historyKeys: [String] {
        rendered.keys.filter {
            $0 == HomeBodyView.listIdentifier || $0 == HomeBodyView.todayHeadingIdentifier
                || $0 == HomeBodyView.earlierHeadingIdentifier || $0 == HomeBodyView.retryIdentifier
                || $0.hasPrefix(HomeBodyView.cardIdentifierPrefix)
                || $0.hasPrefix(HomeBodyView.cardCaptionIdentifierPrefix)
                || $0.hasPrefix(HomeBodyView.cardDotIdentifierPrefix)
        }.sorted()
    }

    /// Nothing derived from stored history is on screen: no list, headings or cards, and none of the stored words.
    private func assertNoHistoryIsShown(file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(historyKeys, [], "no history part is shown", file: file, line: line)
        let words = rendered.values.compactMap(\.text).joined(separator: "\n")
        XCTAssertFalse(words.contains(Self.todayText), "no stored text is shown", file: file, line: line)
        XCTAssertFalse(words.contains(Self.earlierText), "no stored text is shown", file: file, line: line)
    }

    // MARK: capture_only

    func testCaptureOnlyShowsTheLockedCardAndItsUnlockButtonRunsTheExplicitAuthentication() async {
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem, earlierItem], nextCursor: nil)
        await show(
            waitingFor: [HomeBodyView.lockedCardIdentifier, HomeBodyView.unlockButtonIdentifier], thenSettling: true)

        XCTAssertEqual(
            rendered[HomeBodyView.lockedCardIdentifier]?.text, HomeBodyView.lockedTitle + "\n" + HomeBodyView.lockedDetail)
        XCTAssertEqual(rendered[HomeBodyView.unlockButtonIdentifier]?.text, HomeBodyView.unlockTitle)
        XCTAssertTrue(
            frame(HomeBodyView.lockedCardIdentifier).contains(frame(HomeBodyView.unlockButtonIdentifier)),
            "the Unlock button is inside the locked card")
        XCTAssertNil(rendered[HomeBodyView.placeholderIdentifier], "no placeholders while locked")
        assertNoHistoryIsShown()

        tap(HomeBodyView.unlockButtonIdentifier)
        XCTAssertEqual(source.unlockCount, 1, "Unlock runs the explicit authentication")
        await settle()
        XCTAssertTrue(source.readCursors.isEmpty, "no read is attempted in capture_only, even after Unlock is tapped")
        assertNoHistoryIsShown()
    }

    // MARK: Quiet attempt

    func testAQuietAttemptShowsPlaceholderCardsWithNothingFromHistory() async {
        source.isQuietAttemptInFlight = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        await show(waitingFor: Self.placeholderIdentifiers, thenSettling: true)

        XCTAssertNotNil(rendered[HomeBodyView.placeholderIdentifier])
        for index in 0..<HomeBodyView.placeholderCount {
            let identifier = HomeBodyView.placeholderCardIdentifierPrefix + String(index)
            XCTAssertNotEqual(frame(identifier).height, 0, "\(identifier) is drawn")
            XCTAssertNil(rendered[identifier]?.text, "a placeholder card carries no words")
        }
        XCTAssertNil(rendered[HomeBodyView.lockedCardIdentifier], "the locked card waits for the attempt to end")
        XCTAssertNil(rendered[HomeBodyView.unlockButtonIdentifier])
        XCTAssertTrue(rendered.values.allSatisfy { $0.text == nil }, "no words at all while unlocking")
        assertNoHistoryIsShown()
        XCTAssertTrue(source.readCursors.isEmpty, "no read while the quiet attempt is in flight")

        source.setAccess(authenticated: true)
        await waitForRendered("the list replaces the placeholders") {
            rendered[HomeBodyView.placeholderIdentifier] == nil && rendered[Self.cardIdentifier("today-1")] != nil
                && model?.isLoadingFirstPage == false
        }
        XCTAssertNil(rendered[HomeBodyView.placeholderIdentifier], "the placeholders give way to the list")
        XCTAssertEqual(rendered[HomeBodyView.cardIdentifierPrefix + "today-1"]?.text, Self.todayText)
    }

    func testAFailedQuietAttemptReplacesThePlaceholdersWithTheLockedCard() async {
        source.isQuietAttemptInFlight = true
        await show(waitingFor: [HomeBodyView.placeholderIdentifier])
        XCTAssertNotNil(rendered[HomeBodyView.placeholderIdentifier])

        source.setAccess(authenticated: false)
        await waitForRendered("the locked card replaces the placeholders") {
            rendered[HomeBodyView.placeholderIdentifier] == nil && rendered[HomeBodyView.lockedCardIdentifier] != nil
                && rendered[HomeBodyView.unlockButtonIdentifier] != nil
        }
        await settle()
        XCTAssertNil(rendered[HomeBodyView.placeholderIdentifier])
        XCTAssertNotNil(rendered[HomeBodyView.lockedCardIdentifier])
        XCTAssertNotNil(rendered[HomeBodyView.unlockButtonIdentifier]?.tap)
        assertNoHistoryIsShown()
        XCTAssertTrue(source.readCursors.isEmpty)
    }

    // MARK: authenticated

    func testAuthenticatedShowsTodayAndEarlierCardsWithTypeDotTypeAndRelativeDate() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem, earlierItem], nextCursor: nil)
        await show(
            waitingFor: [
                HomeBodyView.todayHeadingIdentifier, HomeBodyView.earlierHeadingIdentifier,
                Self.cardIdentifier("today-1"), Self.cardIdentifier("earlier-1"),
                HomeBodyView.cardDotIdentifierPrefix + "today-1", HomeBodyView.cardDotIdentifierPrefix + "earlier-1",
                HomeBodyView.cardCaptionIdentifierPrefix + "today-1", HomeBodyView.cardCaptionIdentifierPrefix + "earlier-1",
            ])

        XCTAssertNil(rendered[HomeBodyView.lockedCardIdentifier])
        XCTAssertNil(rendered[HomeBodyView.placeholderIdentifier])
        XCTAssertEqual(rendered[HomeBodyView.todayHeadingIdentifier]?.text, "Today")
        XCTAssertEqual(rendered[HomeBodyView.earlierHeadingIdentifier]?.text, "Earlier")

        let todayCard = HomeBodyView.cardIdentifierPrefix + "today-1"
        let earlierCard = HomeBodyView.cardIdentifierPrefix + "earlier-1"
        XCTAssertEqual(rendered[todayCard]?.text, Self.todayText)
        XCTAssertEqual(rendered[earlierCard]?.text, Self.earlierText)
        XCTAssertEqual(rendered[HomeBodyView.cardDotIdentifierPrefix + "today-1"]?.text, "note", "the dot is drawn")
        XCTAssertEqual(rendered[HomeBodyView.cardDotIdentifierPrefix + "earlier-1"]?.text, "idea")

        let todayDate = HomeBodyView.relativeDate(referenceNow.addingTimeInterval(-3600), now: referenceNow)
        let earlierDate = HomeBodyView.relativeDate(referenceNow.addingTimeInterval(-7 * 86_400), now: referenceNow)
        XCTAssertEqual(
            rendered[HomeBodyView.cardCaptionIdentifierPrefix + "today-1"]?.text,
            "\(HomeBodyView.typeTitle(for: "note")) \u{00B7} \(todayDate)", "type and date from the model's clock")
        XCTAssertEqual(
            rendered[HomeBodyView.cardCaptionIdentifierPrefix + "earlier-1"]?.text,
            "\(HomeBodyView.typeTitle(for: "idea")) \u{00B7} \(earlierDate)")

        XCTAssertLessThan(frame(HomeBodyView.todayHeadingIdentifier).maxY, frame(todayCard).minY + 0.5)
        XCTAssertLessThan(frame(todayCard).maxY, frame(HomeBodyView.earlierHeadingIdentifier).minY + 0.5)
        XCTAssertLessThan(frame(HomeBodyView.earlierHeadingIdentifier).maxY, frame(earlierCard).minY + 0.5)
        XCTAssertEqual(source.readCursors, [nil])
    }

    func testRegularWidthShowsALargeTodayOverTwoColumns() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(
            items: [
                item("today-1", type: "note", text: Self.todayText, hoursBefore: 1),
                item("today-2", type: "action", text: "Synthetic action from this morning", hoursBefore: 2),
                earlierItem,
            ],
            nextCursor: nil)
        await show(
            size: Self.tablet, sizeClass: .regular,
            waitingFor: [
                HomeBodyView.todayHeadingIdentifier, HomeBodyView.earlierHeadingIdentifier,
                Self.cardIdentifier("today-1"), Self.cardIdentifier("today-2"), Self.cardIdentifier("earlier-1"),
            ])

        let first = frame(HomeBodyView.cardIdentifierPrefix + "today-1")
        let second = frame(HomeBodyView.cardIdentifierPrefix + "today-2")
        XCTAssertEqual(first.minY, second.minY, accuracy: 0.5, "two cards share a row")
        XCTAssertLessThan(first.maxX, second.minX + 0.5, "side by side")
        XCTAssertGreaterThan(
            frame(HomeBodyView.todayHeadingIdentifier).height, frame(HomeBodyView.earlierHeadingIdentifier).height,
            "Today is large at regular width")
    }

    func testCompactWidthShowsOneColumn() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(
            items: [
                item("today-1", type: "note", text: Self.todayText, hoursBefore: 1),
                item("today-2", type: "action", text: "Synthetic action from this morning", hoursBefore: 2),
            ],
            nextCursor: nil)
        await show(waitingFor: [Self.cardIdentifier("today-1"), Self.cardIdentifier("today-2")])

        let first = frame(HomeBodyView.cardIdentifierPrefix + "today-1")
        let second = frame(HomeBodyView.cardIdentifierPrefix + "today-2")
        XCTAssertLessThan(first.maxY, second.minY + 0.5, "one card per row")
    }

    func testTheNextPageLoadsWhenTheLastCardsAreDrawn() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: "page-2")
        source.pages["page-2"] = CoreRecentItemsPage(items: [earlierItem], nextCursor: nil)
        await show(waitingFor: [Self.cardIdentifier("today-1"), Self.cardIdentifier("earlier-1")])

        XCTAssertEqual(source.readCursors, [nil, "page-2"], "drawing the last card asks for the next page")
        XCTAssertEqual(rendered[HomeBodyView.cardIdentifierPrefix + "earlier-1"]?.text, Self.earlierText)
    }

    func testAFailedReadOffersTryAgainWhichReadsAgain() async {
        source.isAuthenticated = true
        source.failure = SyntheticReadFailure()
        await show(waitingFor: [HomeBodyView.retryIdentifier])
        XCTAssertNotNil(rendered[HomeBodyView.retryIdentifier])
        let readsBeforeRetry = source.readCursors.count

        source.failure = nil
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        tap(HomeBodyView.retryIdentifier)
        await waitForRendered("the retry has read the list") {
            rendered[HomeBodyView.retryIdentifier] == nil && rendered[Self.cardIdentifier("today-1")] != nil
                && model?.isLoadingFirstPage == false
        }
        XCTAssertEqual(source.readCursors.count, readsBeforeRetry + 1)
        XCTAssertNil(rendered[HomeBodyView.retryIdentifier])
        XCTAssertEqual(rendered[HomeBodyView.cardIdentifierPrefix + "today-1"]?.text, Self.todayText)
    }

    // MARK: A new save and a relock

    func testANewSaveIsDrawnFirstAsJustNowSorting() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        let shownModel = await show(waitingFor: [Self.cardIdentifier("today-1")])

        shownModel.captureDidSave(itemID: "new-1", interpretationIsResolved: false)
        await waitForRendered("the new save is drawn as sorting") {
            rendered[HomeBodyView.cardCaptionIdentifierPrefix + "new-1"] != nil
                && rendered[Self.cardIdentifier("new-1")] != nil && rendered[Self.cardIdentifier("today-1")] != nil
                && !shownModel.isLoadingFirstPage
        }
        XCTAssertEqual(rendered[HomeBodyView.cardCaptionIdentifierPrefix + "new-1"]?.text, HomeBodyView.sortingLabel)
        XCTAssertLessThan(
            frame(HomeBodyView.cardIdentifierPrefix + "new-1").maxY,
            frame(HomeBodyView.cardIdentifierPrefix + "today-1").minY + 0.5, "the new save is at the top")
    }

    func testARelockClearsTheDrawnListAtOnceAndReadsNothingMore() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem, earlierItem], nextCursor: nil)
        await show(waitingFor: [Self.cardIdentifier("today-1"), Self.cardIdentifier("earlier-1")])
        XCTAssertNotNil(rendered[HomeBodyView.cardIdentifierPrefix + "today-1"])
        let readsAtRelock = source.readCursors.count

        source.setAccess(authenticated: false)
        await waitForRendered("the locked card replaces the list") {
            rendered[HomeBodyView.lockedCardIdentifier] != nil && rendered[HomeBodyView.unlockButtonIdentifier] != nil
                && rendered[Self.cardIdentifier("today-1")] == nil
        }
        await settle()
        assertNoHistoryIsShown()
        XCTAssertNotNil(rendered[HomeBodyView.lockedCardIdentifier], "the locked card replaces the list")
        XCTAssertNotNil(rendered[HomeBodyView.unlockButtonIdentifier]?.tap)
        XCTAssertEqual(source.readCursors.count, readsAtRelock, "no read after the relock")
    }

    // MARK: Opening an item

    func testTappingACardOpensThatItemAndASortingCardOpensTheSameWay() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem, earlierItem], nextCursor: nil)
        let shownModel = await show(waitingFor: [Self.cardIdentifier("today-1"), Self.cardIdentifier("earlier-1")])

        tap(HomeBodyView.cardIdentifierPrefix + "earlier-1")
        XCTAssertEqual(openedItems, ["earlier-1"])

        shownModel.captureDidSave(itemID: "new-1", interpretationIsResolved: false)
        await waitForRendered("the new save is drawn as sorting") {
            rendered[HomeBodyView.cardCaptionIdentifierPrefix + "new-1"] != nil
                && rendered[Self.cardIdentifier("new-1")] != nil && !shownModel.isLoadingFirstPage
        }
        XCTAssertEqual(rendered[HomeBodyView.cardCaptionIdentifierPrefix + "new-1"]?.text, HomeBodyView.sortingLabel)
        tap(HomeBodyView.cardIdentifierPrefix + "new-1")
        XCTAssertEqual(openedItems, ["earlier-1", "new-1"])
    }

    func testPlaceholderStateExposesNoTappableCard() async {
        source.isQuietAttemptInFlight = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        await show(waitingFor: Self.placeholderIdentifiers, thenSettling: true)
        XCTAssertNotNil(rendered[HomeBodyView.placeholderIdentifier])
        XCTAssertNil(rendered[HomeBodyView.placeholderIdentifier]?.tap)
        for index in 0..<HomeBodyView.placeholderCount {
            let identifier = HomeBodyView.placeholderCardIdentifierPrefix + String(index)
            XCTAssertNotNil(rendered[identifier], "\(identifier) is drawn")
            XCTAssertNil(rendered[identifier]?.tap, "\(identifier) is not tappable")
        }
        XCTAssertTrue(rendered.keys.filter { $0.hasPrefix(HomeBodyView.cardIdentifierPrefix) }.isEmpty)
        XCTAssertTrue(openedItems.isEmpty)
    }

    func testLockedStateExposesNoTappableCard() async {
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        await show(
            waitingFor: [HomeBodyView.lockedCardIdentifier, HomeBodyView.unlockButtonIdentifier], thenSettling: true)
        XCTAssertTrue(rendered.keys.filter { $0.hasPrefix(HomeBodyView.cardIdentifierPrefix) }.isEmpty)
        for (identifier, element) in rendered where identifier != HomeBodyView.unlockButtonIdentifier {
            XCTAssertNil(element.tap, "\(identifier) is not tappable")
        }
        XCTAssertTrue(openedItems.isEmpty)
    }

    func testACardDoesNotOpenAfterARelockClearedTheList() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [todayItem], nextCursor: nil)
        await show(waitingFor: [Self.cardIdentifier("today-1")])
        let staleOpen = rendered[HomeBodyView.cardIdentifierPrefix + "today-1"]?.tap
        XCTAssertNotNil(staleOpen)

        source.setAccess(authenticated: false)
        await waitForRendered("the locked card replaces the list") {
            rendered[HomeBodyView.lockedCardIdentifier] != nil && rendered[Self.cardIdentifier("today-1")] == nil
        }
        staleOpen?()
        XCTAssertTrue(openedItems.isEmpty, "a relock wins over a stale card action")
    }
}
