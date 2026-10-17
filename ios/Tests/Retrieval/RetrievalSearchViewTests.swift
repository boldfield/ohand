import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class ScriptedViewSearcher: RetrievalSearching {
    private(set) var requests: [RetrievalSearchRequest] = []
    private var completions: [(Result<RetrievalPage, RetrievalSearchFailure>) -> Void] = []

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        requests.append(request)
        completions.append(completion)
    }

    func answerLatest(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        completions[completions.count - 1](result)
    }
}

/// Hosts the real search screen in a window over the real model. The screen reports where each visible part landed and
/// the words it shows; the tests drive the model calls the screen's buttons make and then read what is on screen.
final class RetrievalSearchViewTests: XCTestCase {
    private static let firstID = "0b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"
    private static let secondID = "1b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"
    private static let minimumTouchTarget: CGFloat = 44
    private static let phone = CGSize(width: 390, height: 844)

    private var scope: CompositionReadScope = .authenticated
    private var selected: [RetrievalItemReference] = []
    private var searcher = ScriptedViewSearcher()
    private var model: RetrievalSearchModel!
    private var rendered: [String: RetrievalRenderedElement] = [:]
    private var retainedWindow: UIWindow?

    override func setUp() {
        super.setUp()
        scope = .authenticated
        selected = []
        searcher = ScriptedViewSearcher()
        rendered = [:]
        model = RetrievalSearchModel(
            searcher: searcher,
            readScope: { [unowned self] in self.scope },
            onSelect: { [unowned self] in self.selected.append($0) })
        let hosting = UIHostingController(
            rootView: RetrievalSearchView(model: model, renderReporter: { [unowned self] in rendered = $0 }))
        let window = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        settle()
    }

    override func tearDown() {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        model = nil
        super.tearDown()
    }

    private func settle() {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

    private func row(
        _ itemID: String, text: String, itemType: RetrievalItemType?, basis: CoreRetrievalTextBasis = .original,
        originalText: String? = nil, scopeName: String = "Personal"
    ) -> RetrievalResultRow {
        RetrievalResultRow(
            reference: RetrievalItemReference(candidate: itemID)!, text: text, originalText: originalText, basis: basis,
            matched: .current, snippet: RetrievalSnippet(text: text, span: nil), scopeName: scopeName,
            capturedAt: Date(timeIntervalSince1970: 1_791_462_600), itemType: itemType)
    }

    private func runSearch(answering result: Result<RetrievalPage, RetrievalSearchFailure>) {
        model.query = "synthetic"
        model.search()
        searcher.answerLatest(result)
        settle()
    }

    private func renderedText(_ identifier: String) -> String? {
        rendered[identifier]?.text
    }

    private func assertNothingStoredIsShown(file: StaticString = #filePath, line: UInt = #line) {
        let storedParts = rendered.keys.filter {
            $0.hasPrefix("retrieval.search.row") || $0 == RetrievalSearchView.countIdentifier
        }
        XCTAssertTrue(storedParts.isEmpty, "no row or count is shown: \(storedParts)", file: file, line: line)
    }

    func testBeforeAnySearchTheScreenSaysWhatItCanDoAndShowsNoResults() {
        XCTAssertEqual(renderedText(RetrievalSearchView.promptIdentifier), RetrievalSearchView.promptText)
        XCTAssertNotEqual(rendered[RetrievalSearchView.searchButtonIdentifier]?.frame ?? .zero, .zero)
        assertNothingStoredIsShown()
    }

    func testResultsShowTheWordsDateTypeSourceAndAreSelectable() throws {
        let typed = row(Self.firstID, text: "synthetic idea words", itemType: .idea)
        let corrected = row(
            Self.secondID, text: "synthetic fixed words", itemType: nil, basis: .corrected,
            originalText: "synthetic fixd words", scopeName: "Work")

        runSearch(answering: .success(RetrievalPage(totalAccessible: 2, rows: [typed, corrected], offset: 0)))

        XCTAssertEqual(renderedText(RetrievalSearchView.countIdentifier), "2 results")
        XCTAssertEqual(renderedText(RetrievalSearchView.rowTextIdentifier(typed)), "synthetic idea words")
        let typedDetails = try XCTUnwrap(renderedText(RetrievalSearchView.rowDetailsIdentifier(typed)))
        XCTAssertEqual(typedDetails, RetrievalResultRowView.detailsText(for: typed))
        for part in [
            typed.capturedAt.formatted(date: .abbreviated, time: .shortened), "Idea", "Original words", "Personal",
        ] {
            XCTAssertTrue(typedDetails.contains(part), "\(part) is shown for the typed result: \(typedDetails)")
        }
        let correctedDetails = try XCTUnwrap(renderedText(RetrievalSearchView.rowDetailsIdentifier(corrected)))
        for part in ["Not yet sorted", "Corrected", "Work"] {
            XCTAssertTrue(correctedDetails.contains(part), "\(part) is shown for the corrected result: \(correctedDetails)")
        }
        XCTAssertNil(renderedText(RetrievalSearchView.emptyIdentifier))
        XCTAssertNil(renderedText(RetrievalSearchView.lockedIdentifier))

        for shown in [typed, corrected] {
            let frame = try XCTUnwrap(rendered[RetrievalSearchView.rowIdentifier(shown)]?.frame)
            XCTAssertGreaterThanOrEqual(frame.height, Self.minimumTouchTarget, "a result is a full-size touch target")
            XCTAssertTrue(CGRect(origin: .zero, size: Self.phone).intersects(frame), "the result is on screen")
        }

        model.select(corrected)
        XCTAssertEqual(selected, [corrected.reference], "selecting a result hands over only its validated identifier")
    }

    func testANoMatchSearchShowsTheHonestEmptyMessageAndNoCount() {
        runSearch(answering: .success(RetrievalPage(totalAccessible: 0, rows: [], offset: 0)))

        XCTAssertEqual(renderedText(RetrievalSearchView.emptyIdentifier), RetrievalSearchView.emptyText)
        XCTAssertTrue(RetrievalSearchView.emptyText.contains("Your captures are saved"))
        XCTAssertNil(renderedText(RetrievalSearchView.promptIdentifier))
        assertNothingStoredIsShown()
    }

    func testALockedSessionShowsOnlyTheLockedMessageNeverACountOrSnippet() {
        let shown = row(Self.firstID, text: "synthetic private words", itemType: .note)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)))
        XCTAssertNotNil(renderedText(RetrievalSearchView.rowTextIdentifier(shown)), "the result is shown while authenticated")

        scope = .captureOnly
        model.revalidate()
        settle()

        XCTAssertEqual(renderedText(RetrievalSearchView.lockedIdentifier), RetrievalSearchView.lockedText)
        assertNothingStoredIsShown()
        XCTAssertFalse(
            rendered.values.contains { $0.text.contains("synthetic private words") }, "no stored words remain on screen")
    }

    func testASearchStartedWhileLockedNeverReachesTheSearcherAndShowsTheLockedMessage() {
        scope = .captureOnly
        model.query = "synthetic"
        model.search()
        settle()

        XCTAssertTrue(searcher.requests.isEmpty)
        XCTAssertEqual(renderedText(RetrievalSearchView.lockedIdentifier), RetrievalSearchView.lockedText)
        assertNothingStoredIsShown()
    }

    func testAFailureShowsItsMessageAndNoStaleResults() {
        runSearch(answering: .failure(.temporarilyUnavailable))

        XCTAssertEqual(
            renderedText(RetrievalSearchView.failureIdentifier), RetrievalSearchFailure.temporarilyUnavailable.message)
        assertNothingStoredIsShown()
    }

    func testARequestWithNothingToSearchForShowsTheProblemAndSendsNothing() {
        model.search()
        settle()

        XCTAssertEqual(
            renderedText(RetrievalSearchView.problemIdentifier), "Enter words to find, or choose a filter.")
        XCTAssertTrue(searcher.requests.isEmpty)
        assertNothingStoredIsShown()
    }

    func testAnotherPageOffersLoadMore() {
        let first = row(Self.firstID, text: "synthetic one", itemType: .action)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 5, rows: [first], offset: 0)))

        XCTAssertEqual(renderedText(RetrievalSearchView.countIdentifier), "5 results")
        XCTAssertEqual(renderedText(RetrievalSearchView.loadMoreIdentifier), "Load more")
    }

    func testRegularWidthPutsResultsInANarrowLeftColumnWithRoomForTheItemPane() throws {
        let regular = CGSize(width: 700, height: 900)
        let hosting = UIHostingController(
            rootView: RetrievalSearchView(model: model, renderReporter: { [unowned self] in rendered = $0 })
                .environment(\.horizontalSizeClass, .regular))
        retainedWindow?.isHidden = true
        let window = UIWindow(frame: CGRect(origin: .zero, size: regular))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        let shown = row(Self.firstID, text: "synthetic idea words", itemType: .idea)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)))

        let frame = try XCTUnwrap(rendered[RetrievalSearchView.rowIdentifier(shown)]?.frame)
        XCTAssertLessThanOrEqual(frame.width, RetrievalSearchView.regularResultsColumnWidth)
        XCTAssertLessThan(frame.maxX, regular.width / 2, "the results sit on the left, leaving the right for the item pane")
    }

    func testAReplacedOriginalHighlightsTheWordsThatMatched() {
        let original = "Call the Rooter, then RÖOTER again"
        let ranges = RetrievalResultRowView.highlightRanges(in: original, query: "rooter")
        XCTAssertEqual(ranges.map { String(original[$0]) }, ["Rooter", "RÖOTER"])
        XCTAssertTrue(RetrievalResultRowView.highlightRanges(in: original, query: "plumber").isEmpty)
    }

    func testFilterChipsReportTheActiveFilterAsTheirTitle() {
        settle()
        XCTAssertEqual(renderedText(RetrievalSearchView.typeChipIdentifier), "Type")
        model.itemType = .idea
        settle()
        XCTAssertEqual(renderedText(RetrievalSearchView.typeChipIdentifier), "Idea")
    }
}
