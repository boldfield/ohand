import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class FakeRetrievalSearcher: RetrievalSearching {
    private(set) var requests: [RetrievalSearchRequest] = []
    private var completions: [(Result<RetrievalPage, RetrievalSearchFailure>) -> Void] = []

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        requests.append(request)
        completions.append(completion)
    }

    func answer(at index: Int, _ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        completions[index](result)
    }
}

final class RetrievalSearchModelTests: XCTestCase {
    private static let firstID = "0b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"
    private static let secondID = "1b6c1f3e-5d4a-4b8e-9c1d-2a3b4c5d6e7f"

    private var scope: CompositionReadScope = .authenticated
    private var selected: [RetrievalItemReference] = []
    private var searcher = FakeRetrievalSearcher()
    private var model: RetrievalSearchModel!
    private var changeHandlers: [@Sendable (ItemChange) -> Void] = []

    override func setUp() {
        super.setUp()
        scope = .authenticated
        selected = []
        searcher = FakeRetrievalSearcher()
        changeHandlers = []
        model = RetrievalSearchModel(
            searcher: searcher,
            readScope: { [unowned self] in self.scope },
            onSelect: { [unowned self] in self.selected.append($0) },
            observeItemChanges: { [unowned self] handler in
                self.changeHandlers.append(handler)
                return {}
            })
    }

    private func publish(_ change: ItemChange) {
        changeHandlers.forEach { $0(change) }
    }

    private func row(_ itemID: String, text: String = "synthetic words") -> RetrievalResultRow {
        RetrievalResultRow(
            reference: RetrievalItemReference(candidate: itemID)!, text: text, originalText: nil, basis: .original,
            matched: .current, snippet: RetrievalSnippet(text: text, span: nil), scopeName: "Personal",
            capturedAt: Date(timeIntervalSince1970: 1_791_462_600), itemType: nil)
    }

    private func page(_ rows: [RetrievalResultRow], total: Int? = nil, offset: Int = 0) -> RetrievalPage {
        RetrievalPage(totalAccessible: total ?? rows.count, rows: rows, offset: offset)
    }

    private func settle() {
        let drained = expectation(description: "main queue drained")
        DispatchQueue.main.async { drained.fulfill() }
        wait(for: [drained], timeout: 5)
    }

    private func deliver(_ result: Result<RetrievalPage, RetrievalSearchFailure>, at index: Int = 0) {
        searcher.answer(at: index, result)
        settle()
    }

    // MARK: Item changes

    private func showResults(_ rows: [RetrievalResultRow]) {
        model.query = "roofer"
        model.search()
        deliver(.success(page(rows)), at: searcher.requests.count - 1)
    }

    func testReturningFromDetailAfterADeletionRunsTheSameQueryAgainWithoutTheDeletedItem() {
        showResults([row(Self.firstID), row(Self.secondID)])
        model.select(model.rows[0])
        model.screenDidDisappear()
        publish(.item(Self.firstID))
        XCTAssertEqual(searcher.requests.count, 1, "waits for the person to come back")

        model.revalidate()
        XCTAssertEqual(searcher.requests.count, 2)
        XCTAssertEqual(searcher.requests[1], searcher.requests[0])
        XCTAssertFalse(model.rows.map(\.id).contains(Self.firstID), "never shown while the query runs")
        deliver(.success(page([row(Self.secondID)])), at: 1)
        XCTAssertEqual(model.rows.map(\.id), [Self.secondID])
        XCTAssertEqual(model.phase, .results)
    }

    func testReturningFromDetailAfterACorrectionShowsTheCurrentText() {
        showResults([row(Self.firstID, text: "old words")])
        model.select(model.rows[0])
        model.screenDidDisappear()
        publish(.item(Self.firstID))
        model.revalidate()
        deliver(.success(page([row(Self.firstID, text: "new words")])), at: 1)
        XCTAssertEqual(model.rows.map(\.text), ["new words"])
    }

    func testReturningWithNothingChangedKeepsTheResultsWithoutANewQuery() {
        showResults([row(Self.firstID)])
        model.select(model.rows[0])
        model.screenDidDisappear()
        model.revalidate()
        XCTAssertEqual(searcher.requests.count, 1)
        XCTAssertEqual(model.rows.map(\.id), [Self.firstID])
    }

    func testAnAnswerThatWasAskedBeforeTheChangeIsNotShown() {
        model.query = "roofer"
        model.search()
        publish(.item(Self.firstID))
        XCTAssertEqual(searcher.requests.count, 2)
        deliver(.success(page([row(Self.firstID)])), at: 0)
        XCTAssertTrue(model.rows.isEmpty, "the older answer is dropped")
        deliver(.success(page([])), at: 1)
        XCTAssertEqual(model.phase, .empty)
    }

    func testAChangeWithTheResultsOnScreenRunsTheQueryAtOnce() {
        showResults([row(Self.firstID), row(Self.secondID)])
        publish(.item(Self.secondID))
        XCTAssertEqual(searcher.requests.count, 2)
        XCTAssertEqual(model.phase, .searching)
        XCTAssertFalse(model.rows.map(\.id).contains(Self.secondID))
        deliver(.success(page([row(Self.firstID)])), at: 1)
        XCTAssertEqual(model.rows.map(\.id), [Self.firstID])
    }

    func testThePaneKeepsItsItemWhenTheChangedItemIsGoneFromTheResults() {
        model.presentsItemsInPane = true
        showResults([row(Self.firstID), row(Self.secondID)])
        model.select(model.rows[0])
        XCTAssertEqual(model.selectedReference?.value, Self.firstID)
        publish(.item(Self.secondID))
        deliver(.success(page([row(Self.firstID)])), at: 1)
        XCTAssertEqual(model.selectedReference?.value, Self.firstID)

        let refreshesBefore = model.paneRefreshCount
        publish(.item(Self.firstID))
        XCTAssertEqual(model.paneRefreshCount, refreshesBefore + 1, "the pane reads the changed item again")
        XCTAssertFalse(model.rows.map(\.id).contains(Self.firstID), "the row is dropped at once")
        deliver(.success(page([])), at: 2)
        XCTAssertEqual(
            model.selectedReference?.value, Self.firstID,
            "a deleted item stays selected so the pane still shows its deletion result")
        XCTAssertTrue(model.rows.isEmpty)

        model.search()
        XCTAssertNil(model.selectedReference, "searching again drops it")
    }

    func testAChangeToAnotherItemDoesNotRefreshThePane() {
        model.presentsItemsInPane = true
        showResults([row(Self.firstID), row(Self.secondID)])
        model.select(model.rows[0])
        let refreshesBefore = model.paneRefreshCount
        publish(.item(Self.secondID))
        XCTAssertEqual(model.paneRefreshCount, refreshesBefore)
    }

    func testAllItemsEmptiesTheResultsAndAsksAgain() {
        showResults([row(Self.firstID)])
        publish(.allItems)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(searcher.requests.count, 2)
    }

    func testAChangeBeforeAnySearchDoesNothing() {
        publish(.item(Self.firstID))
        publish(.allItems)
        XCTAssertTrue(searcher.requests.isEmpty)
    }

    func testASearchWithNothingToLookForAsksInsteadOfListingEverything() {
        model.search()
        XCTAssertTrue(searcher.requests.isEmpty)
        XCTAssertNotNil(model.problem)
        XCTAssertEqual(model.phase, .idle)
    }

    func testALockedSessionNeverSendsASearchOrShowsAnything() {
        scope = .captureOnly
        model.query = "roofer"
        model.search()
        XCTAssertTrue(searcher.requests.isEmpty, "scope is checked before the core is asked")
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.totalAccessible, 0)
    }

    func testResultsAppearOnlyAfterTheAnswerAndShowTheCount() {
        model.query = "roofer"
        model.search()
        XCTAssertEqual(model.phase, .searching)
        XCTAssertEqual(searcher.requests.first?.query, "roofer")

        deliver(.success(page([row(Self.firstID)], total: 1)))

        XCTAssertEqual(model.phase, .results)
        XCTAssertEqual(model.rows.map(\.id), [Self.firstID])
        XCTAssertEqual(model.totalAccessible, 1)
        XCTAssertFalse(model.canLoadMore)
    }

    func testNoMatchIsAnHonestEmptyState() {
        model.query = "zebra"
        model.search()
        deliver(.success(page([])))
        XCTAssertEqual(model.phase, .empty)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.totalAccessible, 0)
    }

    func testFiltersAloneSearchWithoutWords() {
        model.sessionTopic = "garage"
        model.since = Date(timeIntervalSince1970: 1_791_000_000)
        model.search()
        XCTAssertEqual(searcher.requests.count, 1)
        XCTAssertFalse(searcher.requests[0].isWordSearch)
        XCTAssertEqual(searcher.requests[0].trimmedSessionTopic, "garage")
        XCTAssertNotNil(searcher.requests[0].since)
    }

    func testARelockWhileSearchingDiscardsTheAnswerWithoutShowingCountsOrText() {
        model.query = "roofer"
        model.search()
        scope = .captureOnly

        deliver(.success(page([row(Self.firstID)], total: 1)))

        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.totalAccessible, 0)
        XCTAssertEqual(model.query, "", "the typed words are dropped with the session")
    }

    func testAnAnswerRefusedByTheGateAfterARelockShowsTheLockedState() {
        model.query = "roofer"
        model.search()
        deliver(.failure(.relocked))
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testAFailureNeverLeavesStaleRowsOnDisplay() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)])))

        model.search()
        XCTAssertTrue(model.rows.isEmpty, "a new search clears the old rows while it runs")
        deliver(.failure(.temporarilyUnavailable), at: 1)

        XCTAssertEqual(model.phase, .failed(.temporarilyUnavailable))
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testAnAnswerToASupersededSearchIsIgnored() {
        model.query = "first"
        model.search()
        model.query = "second"
        model.search()

        deliver(.success(page([row(Self.firstID)])), at: 0)
        XCTAssertEqual(model.phase, .searching, "the older answer does not stand in for the newer search")
        XCTAssertTrue(model.rows.isEmpty)

        deliver(.success(page([row(Self.secondID)])), at: 1)
        XCTAssertEqual(model.rows.map(\.id), [Self.secondID])
    }

    func testLoadMoreAppendsTheNextPageFromTheRowsAlreadyShown() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)], total: 2)))
        XCTAssertTrue(model.canLoadMore)

        model.loadMore()
        XCTAssertEqual(searcher.requests.last?.offset, 1)
        XCTAssertEqual(searcher.requests.last?.query, "roofer")
        deliver(.success(page([row(Self.secondID)], total: 2, offset: 1)), at: 1)

        XCTAssertEqual(model.rows.map(\.id), [Self.firstID, Self.secondID])
        XCTAssertFalse(model.canLoadMore)
    }

    func testLoadMoreUsesTheSearchThatRanNotWhatWasTypedSince() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)], total: 2)))
        model.query = "something else"

        model.loadMore()

        XCTAssertEqual(searcher.requests.last?.query, "roofer")
    }

    func testAFailedLoadMoreKeepsTheRowsAndSaysSo() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)], total: 2)))
        model.loadMore()
        deliver(.failure(.temporarilyUnavailable), at: 1)

        XCTAssertEqual(model.phase, .results)
        XCTAssertEqual(model.rows.map(\.id), [Self.firstID])
        XCTAssertNotNil(model.problem)
    }

    func testSelectingAShownResultPassesItsValidatedReferenceOnly() {
        model.query = "roofer"
        model.search()
        let shown = row(Self.firstID)
        deliver(.success(page([shown])))

        model.select(shown)

        XCTAssertEqual(selected.map(\.value), [Self.firstID])
    }

    func testSelectingARowThatIsNotShownPassesNothing() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)])))

        model.select(row(Self.secondID))

        XCTAssertTrue(selected.isEmpty)
    }

    func testSelectingAfterTheSessionLockedPassesNothingAndEmptiesTheScreen() {
        model.query = "roofer"
        model.search()
        let shown = row(Self.firstID)
        deliver(.success(page([shown])))
        scope = .captureOnly

        model.select(shown)

        XCTAssertTrue(selected.isEmpty)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.phase, .idle)
    }

    func testRevalidatingALockedSessionEmptiesTheScreenAndSaysItIsLocked() {
        model.query = "roofer"
        model.search()
        deliver(.success(page([row(Self.firstID)])))
        model.revalidate()
        XCTAssertEqual(model.phase, .results, "an authenticated session keeps its results")

        scope = .captureOnly
        model.revalidate()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testClearingDropsResultsWordsFiltersAndAnyRunningSearch() {
        model.query = "roofer"
        model.sessionTopic = "garage"
        model.itemType = .idea
        model.since = Date()
        model.search()

        model.clear()
        deliver(.success(page([row(Self.firstID)])))

        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.query, "")
        XCTAssertEqual(model.sessionTopic, "")
        XCTAssertNil(model.itemType)
        XCTAssertNil(model.since)
    }

    private func showOneResult() throws -> RetrievalResultRow {
        model.query = "roofer"
        model.search()
        let shown = row(Self.firstID)
        deliver(.success(page([shown])))
        return shown
    }

    func testLeavingForAResultsDetailKeepsTheSearchForTheWayBack() throws {
        let shown = try showOneResult()

        model.select(shown)
        model.screenDidDisappear()
        model.revalidate()

        XCTAssertEqual(selected, [shown.reference])
        XCTAssertEqual(model.phase, .results)
        XCTAssertEqual(model.rows, [shown])
        XCTAssertEqual(model.query, "roofer")
    }

    func testLeavingTheScreenAnyOtherWayDropsTheSearch() throws {
        _ = try showOneResult()

        model.screenDidDisappear()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.query, "")
    }

    func testReturningFromDetailThenLeavingDropsTheSearch() throws {
        let shown = try showOneResult()
        model.select(shown)
        model.screenDidDisappear()
        model.revalidate()

        model.screenDidDisappear()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testARelockWhileDetailIsOpenDropsTheSearch() throws {
        let shown = try showOneResult()
        model.select(shown)
        scope = .captureOnly

        model.screenDidDisappear()

        XCTAssertEqual(model.phase, .idle)
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.query, "")
    }

    func testInThePaneSelectingKeepsTheReferenceAndPushesNothing() throws {
        let shown = try showOneResult()
        model.presentsItemsInPane = true

        model.select(shown)

        XCTAssertTrue(selected.isEmpty, "nothing is pushed while the pane shows the item")
        XCTAssertEqual(model.selectedReference, shown.reference)
    }

    func testInThePaneSelectingAfterARelockKeepsNothing() throws {
        let shown = try showOneResult()
        model.presentsItemsInPane = true
        scope = .captureOnly

        model.select(shown)

        XCTAssertNil(model.selectedReference)
        XCTAssertTrue(selected.isEmpty)
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testThePaneSelectionIsDroppedByANewSearchARelockLeavingOrLosingThePane() {
        let shown = row(Self.firstID)
        func showAndSelect() {
            model.query = "roofer"
            model.search()
            deliver(.success(page([shown])), at: searcher.requests.count - 1)
            model.presentsItemsInPane = true
            model.select(shown)
            XCTAssertEqual(model.selectedReference, shown.reference)
        }

        showAndSelect()
        model.search()
        XCTAssertNil(model.selectedReference, "a new search drops the selection")

        showAndSelect()
        scope = .captureOnly
        model.revalidate()
        XCTAssertNil(model.selectedReference, "a relock drops the selection")
        scope = .authenticated

        showAndSelect()
        model.screenDidDisappear()
        XCTAssertNil(model.selectedReference, "leaving the screen drops the selection")

        showAndSelect()
        model.presentsItemsInPane = false
        XCTAssertNil(model.selectedReference, "losing the pane, at compact width, drops the selection")
        XCTAssertTrue(selected.isEmpty)
    }
}
