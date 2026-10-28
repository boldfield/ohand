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

    private(set) var countRequests: [RetrievalSearchRequest] = []
    /// What the count-only query reports for closed matches.
    var closedCount = 0

    func countClosedMatches(for request: RetrievalSearchRequest, completion: @escaping (Int) -> Void) {
        countRequests.append(request)
        completion(closedCount)
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
    private var filterPresentation = RetrievalFilterPresentation()
    private var rendered: [String: RetrievalRenderedElement] = [:]
    private var retainedWindow: UIWindow?
    /// Off once a test replaces the phone-sized host, so only the host under test fills `rendered`.
    private var phoneHostReports = true

    override func setUp() {
        super.setUp()
        scope = .authenticated
        selected = []
        searcher = ScriptedViewSearcher()
        rendered = [:]
        phoneHostReports = true
        filterPresentation = RetrievalFilterPresentation()
        model = RetrievalSearchModel(
            searcher: searcher,
            readScope: { [unowned self] in self.scope },
            onSelect: { [unowned self] in self.selected.append($0) })
        let hosting = UIHostingController(
            rootView: RetrievalSearchView(
                model: model, filterPresentation: filterPresentation,
                renderReporter: { [unowned self] in if phoneHostReports { rendered = $0 } }))
        let window = UIWindow(frame: CGRect(origin: .zero, size: Self.phone))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        settle(until: "the prompt and the search button are drawn") {
            self.rendered[RetrievalSearchView.promptIdentifier] != nil
                && (self.rendered[RetrievalSearchView.searchButtonIdentifier]?.frame ?? .zero) != .zero
        }
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
        originalText: String? = nil, scopeName: String = "Personal", lifecycle: RetrievalLifecycle? = nil
    ) -> RetrievalResultRow {
        RetrievalResultRow(
            reference: RetrievalItemReference(candidate: itemID)!, text: text, originalText: originalText, basis: basis,
            matched: .current, snippet: RetrievalSnippet(text: text, span: nil), scopeName: scopeName,
            capturedAt: Date(timeIntervalSince1970: 1_791_462_600), itemType: itemType, lifecycle: lifecycle)
    }

    private func forEachAppearance(_ body: (String) -> Void) {
        for (name, style) in [("light", UIUserInterfaceStyle.light), ("dark", .dark)] {
            retainedWindow?.overrideUserInterfaceStyle = style
            settle()
            body(name)
        }
    }

    private func runSearch(answering result: Result<RetrievalPage, RetrievalSearchFailure>) {
        model.query = "synthetic"
        model.search()
        searcher.answerLatest(result)
        settle(until: "the screen shows the answer") { self.answerIsShown(result) }
        settle()
    }

    /// Whether the screen has drawn what the answer to a search puts on it: its rows and count, the empty message or
    /// the failure message.
    private func answerIsShown(_ result: Result<RetrievalPage, RetrievalSearchFailure>) -> Bool {
        switch result {
        case .failure:
            return rendered[RetrievalSearchView.failureIdentifier] != nil
        case .success(let page):
            if page.rows.isEmpty { return rendered[RetrievalSearchView.emptyIdentifier] != nil }
            return rendered[RetrievalSearchView.countIdentifier] != nil
                && page.rows.allSatisfy { rendered[RetrievalSearchView.rowIdentifier($0)] != nil }
        }
    }

    private func renderedText(_ identifier: String) -> String? {
        rendered[identifier]?.text
    }

    /// Runs the main loop until the screen shows what the test waits for (a sheet takes a moment to present and lay
    /// out); fails the test naming the condition if ten seconds pass first.
    private func settle(
        until condition: String, file: StaticString = #filePath, line: UInt = #line, _ holds: () -> Bool
    ) {
        waitUntilPumping(condition, timeout: 10, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.setNeedsLayout()
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
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
        settle(until: "the locked message replaces the result") {
            self.rendered[RetrievalSearchView.lockedIdentifier] != nil
                && self.rendered[RetrievalSearchView.rowTextIdentifier(shown)] == nil
                && self.rendered[RetrievalSearchView.countIdentifier] == nil
        }
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
        settle(until: "the locked message is shown") { self.rendered[RetrievalSearchView.lockedIdentifier] != nil }
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
        settle(until: "the problem message is shown") { self.rendered[RetrievalSearchView.problemIdentifier] != nil }
        settle()

        XCTAssertEqual(
            renderedText(RetrievalSearchView.problemIdentifier), "Enter words to find, or choose a filter.")
        XCTAssertTrue(searcher.requests.isEmpty)
        assertNothingStoredIsShown()
    }

    func testAnotherPageOffersLoadMore() {
        let first = row(Self.firstID, text: "synthetic one", itemType: .action)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 5, rows: [first], offset: 0)))
        settle(until: "Load more is shown") { self.rendered[RetrievalSearchView.loadMoreIdentifier] != nil }

        XCTAssertEqual(renderedText(RetrievalSearchView.countIdentifier), "5 results")
        XCTAssertEqual(renderedText(RetrievalSearchView.loadMoreIdentifier), "Load more")
    }

    func testRegularWidthPutsResultsInANarrowLeftColumnWithRoomForTheItemPane() throws {
        let regular = CGSize(width: 700, height: 900)
        let hosting = UIHostingController(
            rootView: RetrievalSearchView(model: model, renderReporter: { [unowned self] in rendered = $0 })
                .environment(\.horizontalSizeClass, .regular))
        retainedWindow?.isHidden = true
        phoneHostReports = false
        rendered = [:]
        let window = UIWindow(frame: CGRect(origin: .zero, size: regular))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        let shown = row(Self.firstID, text: "synthetic idea words", itemType: .idea)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)))

        settle(until: "the result row is drawn in the regular-width host") {
            self.rendered[RetrievalSearchView.rowIdentifier(shown)] != nil
        }
        let frame = try XCTUnwrap(rendered[RetrievalSearchView.rowIdentifier(shown)]?.frame)
        XCTAssertLessThanOrEqual(frame.width, RetrievalSearchView.regularResultsColumnWidth)
        XCTAssertLessThan(frame.maxX, regular.width / 2, "the results sit on the left, leaving the right for the item pane")
    }

    func testAReplacedOriginalHighlightsOnlyTokensTheQueryIsAPrefixOf() {
        let original = "Call the Rooter, then RÖOTER again, proof roof, Rooter-x"
        let ranges = RetrievalResultRowView.highlightRanges(in: original, query: "rooter")
        XCTAssertEqual(ranges.map { String(original[$0]) }, ["Rooter", "RÖOTER", "Rooter"])
        XCTAssertTrue(RetrievalResultRowView.highlightRanges(in: original, query: "plumber").isEmpty)
        let proof = "proof roof"
        XCTAssertEqual(RetrievalResultRowView.highlightRanges(in: proof, query: "roof").map { String(proof[$0]) }, ["roof"])
        XCTAssertTrue(RetrievalResultRowView.highlightRanges(in: "Rooter", query: "oot").isEmpty, "no mid-word match")
    }

    func testTheAttributedOriginalTintsExactlyTheMatchedWords() {
        let attributed = RetrievalResultRowView.originalAttributed("proof roof", highlighting: "roof")
        XCTAssertEqual(String(attributed.characters), "Original: proof roof")
        XCTAssertEqual(RetrievalResultRowView.highlightedWords(in: attributed), ["roof"])
    }

    private func supersededRow(originalText: String = "synthetic proof roof") -> RetrievalResultRow {
        RetrievalResultRow(
            reference: RetrievalItemReference(candidate: Self.firstID)!, text: "synthetic roofer words",
            originalText: originalText, basis: .corrected, matched: .supersededOriginal,
            snippet: RetrievalSnippet(text: "synthetic roofer words", span: nil), scopeName: "Personal",
            capturedAt: Date(timeIntervalSince1970: 1_791_462_600), itemType: .idea)
    }

    func testAReplacedOriginalCardRendersTheHighlightedMatch() {
        let shown = supersededRow()
        model.query = "roof"
        model.search()
        searcher.answerLatest(.success(RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)))
        settle(until: "the replaced original is highlighted") {
            self.rendered[RetrievalSearchView.rowOriginalIdentifier(shown)] != nil
        }
        XCTAssertEqual(renderedText(RetrievalSearchView.rowOriginalIdentifier(shown)), "roof")
    }

    func testEditingTheFieldThenLoadingMoreKeepsHighlightingTheSearchedWords() {
        let shown = supersededRow()
        model.query = "roof"
        model.search()
        searcher.answerLatest(.success(RetrievalPage(totalAccessible: 2, rows: [shown], offset: 0)))
        settle(until: "the replaced original is highlighted") {
            self.renderedText(RetrievalSearchView.rowOriginalIdentifier(shown)) == "roof"
        }
        model.query = "proof"
        model.loadMore()
        // Nothing answers the load-more request; give the screen time to redraw were it going to change.
        settle()
        XCTAssertEqual(renderedText(RetrievalSearchView.rowOriginalIdentifier(shown)), "roof")
    }

    func testFilterChipsReportTheActiveFilterAsTheirTitle() {
        settle(until: "the type chip is drawn") { self.renderedText(RetrievalSearchView.typeChipIdentifier) == "Type" }
        XCTAssertEqual(renderedText(RetrievalSearchView.typeChipIdentifier), "Type")
        model.itemType = .idea
        settle(until: "the type chip shows Idea") { self.renderedText(RetrievalSearchView.typeChipIdentifier) == "Idea" }
        XCTAssertEqual(renderedText(RetrievalSearchView.typeChipIdentifier), "Idea")
    }

    func testAPunctuatedPieceHighlightsOnlyItsAdjacentPhraseLikeTheCore() {
        let original = "football elsewhere foo bar, foo barn, food bar, foo-bar"
        XCTAssertEqual(
            RetrievalResultRowView.highlightRanges(in: original, query: "foo-bar").map { String(original[$0]) },
            ["foo", "bar", "foo", "bar", "foo", "bar"],
            "the earlier tokens match whole and adjacent, only the last as a prefix")
        XCTAssertTrue(
            RetrievalResultRowView.highlightRanges(in: "football elsewhere bar", query: "foo-bar").isEmpty,
            "a phrase whose tokens are not adjacent matches nothing")
        let mixed = "foo bar football"
        XCTAssertEqual(
            RetrievalResultRowView.highlightRanges(in: mixed, query: "foo-bar foot").map { String(mixed[$0]) },
            ["foo", "bar", "foot"], "each piece matches on its own terms")
    }

    func testAReplacedOriginalCardHighlightsAPunctuatedPieceOnlyWhereThePhraseMatched() {
        let shown = supersededRow(originalText: "football elsewhere foo bar")
        model.query = "foo-bar"
        model.search()
        searcher.answerLatest(.success(RetrievalPage(totalAccessible: 1, rows: [shown], offset: 0)))
        settle(until: "the replaced original is highlighted") {
            self.rendered[RetrievalSearchView.rowOriginalIdentifier(shown)] != nil
        }
        XCTAssertEqual(renderedText(RetrievalSearchView.rowOriginalIdentifier(shown)), "foo|bar")
    }

    func testChoosingDatesOpensTheDatesSheetWithItsDayControls() throws {
        filterPresentation.sheet = .dates
        settle(until: "the Dates sheet and its controls are drawn") {
            self.renderedText(RetrievalSearchView.filterSheetTitleIdentifier) == "Dates"
                && self.renderedText(RetrievalSearchView.sinceToggleIdentifier) != nil
                && self.renderedText(RetrievalSearchView.untilToggleIdentifier) != nil
                && self.renderedText(RetrievalSearchView.sheetDoneIdentifier) != nil
        }

        XCTAssertEqual(renderedText(RetrievalSearchView.filterSheetTitleIdentifier), "Dates")
        XCTAssertEqual(renderedText(RetrievalSearchView.sinceToggleIdentifier), "Since: off")
        XCTAssertEqual(renderedText(RetrievalSearchView.untilToggleIdentifier), "Until: off")
        XCTAssertNil(renderedText(RetrievalSearchView.sinceDateIdentifier), "no day is offered until the filter is on")
        XCTAssertEqual(renderedText(RetrievalSearchView.sheetDoneIdentifier), "Done")
        XCTAssertNil(renderedText(RetrievalSearchView.topicIdentifier), "the dates sheet shows no topic field")

        let since = Date(timeIntervalSince1970: 1_791_462_600)
        model.since = since
        settle(until: "the since day is offered and the time chip shows it") {
            self.renderedText(RetrievalSearchView.sinceToggleIdentifier) == "Since: on"
                && self.renderedText(RetrievalSearchView.sinceDateIdentifier) != nil
                && self.renderedText(RetrievalSearchView.timeChipIdentifier)
                    == "From \(since.formatted(date: .abbreviated, time: .omitted))"
        }

        XCTAssertEqual(renderedText(RetrievalSearchView.sinceToggleIdentifier), "Since: on")
        XCTAssertEqual(
            renderedText(RetrievalSearchView.sinceDateIdentifier), since.formatted(date: .abbreviated, time: .omitted))
        XCTAssertNil(renderedText(RetrievalSearchView.untilDateIdentifier))
        XCTAssertEqual(
            renderedText(RetrievalSearchView.timeChipIdentifier),
            "From \(since.formatted(date: .abbreviated, time: .omitted))", "the time chip shows the chosen day")

        filterPresentation.sheet = nil
        settle(until: "the sheet is gone") { self.renderedText(RetrievalSearchView.filterSheetTitleIdentifier) == nil }
        XCTAssertNil(renderedText(RetrievalSearchView.filterSheetTitleIdentifier), "closing the sheet removes its controls")
    }

    func testChoosingATopicOpensTheTopicSheetWithItsFieldAndToggle() {
        filterPresentation.sheet = .topic
        settle(until: "the Session topic sheet and its controls are drawn") {
            self.renderedText(RetrievalSearchView.filterSheetTitleIdentifier) == "Session topic"
                && self.renderedText(RetrievalSearchView.topicIdentifier) != nil
                && self.renderedText(RetrievalSearchView.onlyWithTopicIdentifier) != nil
                && self.renderedText(RetrievalSearchView.sheetDoneIdentifier) != nil
        }

        XCTAssertEqual(renderedText(RetrievalSearchView.filterSheetTitleIdentifier), "Session topic")
        XCTAssertEqual(renderedText(RetrievalSearchView.topicIdentifier), "")
        XCTAssertEqual(
            renderedText(RetrievalSearchView.onlyWithTopicIdentifier), "Only items with a session topic: off")
        XCTAssertEqual(renderedText(RetrievalSearchView.sheetDoneIdentifier), "Done")
        XCTAssertNil(renderedText(RetrievalSearchView.sinceToggleIdentifier), "the topic sheet shows no day controls")

        model.sessionTopic = "synthetic garden"
        model.onlyWithSessionTopic = true
        settle(until: "the topic field, toggle and chip show the topic") {
            self.renderedText(RetrievalSearchView.topicIdentifier) == "synthetic garden"
                && self.renderedText(RetrievalSearchView.onlyWithTopicIdentifier) == "Only items with a session topic: on"
                && self.renderedText(RetrievalSearchView.topicChipIdentifier) == "synthetic garden"
        }

        XCTAssertEqual(renderedText(RetrievalSearchView.topicIdentifier), "synthetic garden")
        XCTAssertEqual(renderedText(RetrievalSearchView.onlyWithTopicIdentifier), "Only items with a session topic: on")
        XCTAssertEqual(
            renderedText(RetrievalSearchView.topicChipIdentifier), "synthetic garden", "the topic chip shows the topic")
    }

    // MARK: Include done

    func testTheDefaultSearchAsksForOpenItemsAndTheChipIsOff() {
        forEachAppearance { appearance in
            XCTAssertEqual(renderedText(RetrievalSearchView.includeDoneChipIdentifier), "Include done: off", appearance)
        }
        runSearch(answering: .success(RetrievalPage(totalAccessible: 0, rows: [], offset: 0)))
        XCTAssertEqual(searcher.requests.last?.lifecycleStates, ["active"])
    }

    func testTheToggleSearchesAgainWithClosedItemsIncluded() {
        let open = row(Self.firstID, text: "synthetic open words", itemType: .idea)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 1, rows: [open], offset: 0)))
        model.setIncludeDone(true)
        let closed = row(Self.secondID, text: "synthetic closed words", itemType: .idea, lifecycle: .completed)
        searcher.answerLatest(.success(RetrievalPage(totalAccessible: 2, rows: [open, closed], offset: 0)))
        settle(until: "the closed row is drawn and the chip is on") {
            self.rendered[RetrievalSearchView.rowIdentifier(closed)] != nil
                && self.renderedText(RetrievalSearchView.includeDoneChipIdentifier) == "Include done: on"
        }
        XCTAssertEqual(searcher.requests.last?.lifecycleStates, ["active", "completed", "cancelled"])
    }

    func testClosedRowsShowDoneAndCancelledMarkersInTheirOwnWords() {
        let done = row(Self.firstID, text: "synthetic finished words", itemType: .action, lifecycle: .completed)
        let dropped = row(Self.secondID, text: "synthetic dropped words", itemType: .action, lifecycle: .cancelled)
        let open = row("2b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f", text: "synthetic open words", itemType: .action, lifecycle: .active)
        model.setIncludeDone(true)
        runSearch(answering: .success(RetrievalPage(totalAccessible: 3, rows: [done, dropped, open], offset: 0)))
        forEachAppearance { appearance in
            XCTAssertEqual(renderedText(RetrievalSearchView.rowMarkerIdentifier(done)), "Done", appearance)
            XCTAssertEqual(renderedText(RetrievalSearchView.rowMarkerIdentifier(dropped)), "Cancelled", appearance)
            XCTAssertNil(rendered[RetrievalSearchView.rowMarkerIdentifier(open)], "an open row has no marker")
        }
    }

    func testEmptyStateWithDoneOffOffersTheCountedHintAndWithDoneOnSaysNothingClosedMatched() {
        searcher.closedCount = 2
        runSearch(answering: .success(RetrievalPage(totalAccessible: 0, rows: [], offset: 0)))
        settle(until: "the hint is drawn") { self.rendered[RetrievalSearchView.closedHintIdentifier] != nil }
        forEachAppearance { appearance in
            XCTAssertEqual(renderedText(RetrievalSearchView.emptyIdentifier), RetrievalSearchView.emptyText, appearance)
            XCTAssertEqual(renderedText(RetrievalSearchView.closedHintIdentifier), "2 done items also match.", appearance)
            XCTAssertNotNil(rendered[RetrievalSearchView.closedHintActionIdentifier], appearance)
        }
        XCTAssertEqual(searcher.countRequests.last?.query, "synthetic")

        model.setIncludeDone(true)
        searcher.answerLatest(.success(RetrievalPage(totalAccessible: 0, rows: [], offset: 0)))
        settle(until: "the empty copy for included done is drawn") {
            self.renderedText(RetrievalSearchView.emptyIdentifier) == RetrievalSearchView.emptyIncludingDoneText
        }
        forEachAppearance { appearance in
            XCTAssertEqual(
                renderedText(RetrievalSearchView.emptyIdentifier), RetrievalSearchView.emptyIncludingDoneText, appearance)
            XCTAssertNil(rendered[RetrievalSearchView.closedHintIdentifier], "no hint while done is included")
        }
    }

    func testTheHintIsOneLineWithTheCountAndIsAbsentWhenNothingClosedMatches() {
        XCTAssertEqual(RetrievalSearchView.closedHintText(count: 1), "1 done item also matches.")
        runSearch(answering: .success(RetrievalPage(totalAccessible: 0, rows: [], offset: 0)))
        XCTAssertNil(rendered[RetrievalSearchView.closedHintIdentifier])
    }
}
