import OhAndCoreBridge
import XCTest
@testable import OhAnd
@testable import OhAndServices

/// A scriptable session and read: tests flip the access state and answer pages themselves.
final class FakeHomeRecentsSource: HomeRecentsSource {
    var isAuthenticated = false
    var isQuietAttemptInFlight = false
    private(set) var readCursors: [String?] = []
    private(set) var unlockCount = 0
    var pages: [String?: CoreRecentItemsPage] = [:]
    var failure: Error?
    /// Saved items whose interpretation has ended, typed or not; every other status read answers still pending.
    var finishedInterpretations: Set<String> = []
    private(set) var statusReadItemIDs: [String] = []
    /// When set, a read waits until `releaseHeldReads()`, so a test can relock while it is outstanding.
    var holdsReads = false
    /// How many of the next reads to hold until `releaseHeldReads()`. Each answers with the page as it stood when the read
    /// was made, the way a core read that started before a change returns what it saw.
    var heldSnapshotReads = 0
    private var heldReads: [CheckedContinuation<Void, Never>] = []
    private var handlers: [@Sendable () -> Void] = []
    private var changeHandlers: [@Sendable (ItemChange) -> Void] = []

    func readPage(cursor: String?) async throws -> CoreRecentItemsPage {
        readCursors.append(cursor)
        if heldSnapshotReads > 0 {
            heldSnapshotReads -= 1
            let pageAsCalled = pages[cursor] ?? CoreRecentItemsPage(items: [], nextCursor: nil)
            await withCheckedContinuation { heldReads.append($0) }
            if let failure { throw failure }
            return pageAsCalled
        }
        if holdsReads {
            await withCheckedContinuation { heldReads.append($0) }
        }
        if let failure { throw failure }
        return pages[cursor] ?? CoreRecentItemsPage(items: [], nextCursor: nil)
    }

    func interpretationIsFinished(itemID: String) async throws -> Bool {
        statusReadItemIDs.append(itemID)
        if let failure { throw failure }
        return finishedInterpretations.contains(itemID)
    }

    func releaseHeldReads() {
        let continuations = heldReads
        heldReads = []
        continuations.forEach { $0.resume() }
    }

    func unlock() { unlockCount += 1 }

    func observeAccess(_ handler: @escaping @Sendable () -> Void) -> () -> Void {
        handlers.append(handler)
        return {}
    }

    func observeItemChanges(_ handler: @escaping @Sendable (ItemChange) -> Void) -> () -> Void {
        changeHandlers.append(handler)
        return {}
    }

    func publish(_ change: ItemChange) {
        changeHandlers.forEach { $0(change) }
    }

    /// Changes the session the way the real one does: the state first, then every observer.
    func setAccess(authenticated: Bool, quietAttemptInFlight: Bool = false) {
        isAuthenticated = authenticated
        isQuietAttemptInFlight = quietAttemptInFlight
        handlers.forEach { $0() }
    }
}

/// Holds the model's poll pauses until a test releases them, so no real time passes.
actor PollPauses {
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { waiting.append($0) }
    }

    func releaseAll() {
        let continuations = waiting
        waiting = []
        continuations.forEach { $0.resume() }
    }
}

@MainActor
final class HomeRecentsModelTests: XCTestCase {
    private var source: FakeHomeRecentsSource!
    private let referenceNow = ISO8601DateFormatter().date(from: "2026-10-19T12:00:00Z")!

    override func setUp() {
        super.setUp()
        source = FakeHomeRecentsSource()
    }

    private func item(
        _ identifier: String, type: String? = "note", text: String? = "Synthetic text",
        capturedAt: String = "2026-10-19T11:00:00Z", kind: CoreRecentItemSourceKind = .text
    ) -> CoreRecentItem {
        CoreRecentItem(
            itemID: identifier, revision: 1, itemType: type, displayText: text, capturedAt: capturedAt, sourceKind: kind)
    }

    private func makeModel() -> HomeRecentsModel {
        HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow })
    }

    private func settle() async {
        for _ in 0..<500 { await Task.yield() }
    }

    func testCaptureOnlyShowsTheLockedStateAndNeverReads() async {
        let model = makeModel()
        model.loadFirstPage()
        model.loadMore()
        model.captureDidSave(itemID: "a", interpretationIsResolved: false)
        await settle()
        XCTAssertEqual(model.bodyState, .locked)
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertTrue(source.readCursors.isEmpty, "no read is attempted in capture_only")
    }

    func testAQuietAttemptShowsPlaceholdersAndReadsNothing() async {
        source.isQuietAttemptInFlight = true
        let model = makeModel()
        await settle()
        XCTAssertEqual(model.bodyState, .unlocking)
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertTrue(source.readCursors.isEmpty)
    }

    func testAuthenticatedLoadsAndGroupsTodayAndEarlier() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("a"), item("b", capturedAt: "2026-10-12T09:00:00Z")], nextCursor: nil)
        let model = makeModel()
        await settle()
        XCTAssertEqual(model.bodyState, .unlocked)
        XCTAssertEqual(model.entries.map(\.id), ["a", "b"])
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        XCTAssertEqual(model.section(for: model.entries[0], calendar: calendar), .today)
        XCTAssertEqual(model.section(for: model.entries[1], calendar: calendar), .earlier)
    }

    func testUnlockingThenSuccessLoadsTheFirstPage() async {
        source.isQuietAttemptInFlight = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        let model = makeModel()
        source.setAccess(authenticated: true)
        await settle()
        XCTAssertEqual(model.bodyState, .unlocked)
        XCTAssertEqual(model.entries.map(\.id), ["a"])
    }

    func testAFailedQuietAttemptEndsOnTheLockedCard() async {
        source.isQuietAttemptInFlight = true
        let model = makeModel()
        source.setAccess(authenticated: false)
        XCTAssertEqual(model.bodyState, .locked)
        XCTAssertTrue(source.readCursors.isEmpty)
    }

    func testRelockClearsTheListImmediately() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: "next")
        let model = makeModel()
        await settle()
        XCTAssertEqual(model.entries.count, 1)
        source.setAccess(authenticated: false)
        XCTAssertEqual(model.bodyState, .locked, "applied before setAccess returned")
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertTrue(model.loadedItems.isEmpty)
        XCTAssertFalse(model.canLoadMore)
    }

    func testAPageThatArrivesAfterARelockIsDropped() async {
        source.isAuthenticated = true
        source.holdsReads = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.setAccess(authenticated: false)
        source.releaseHeldReads()
        await settle()
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertEqual(model.bodyState, .locked)
    }

    func testMorePagesLoadWhenTheLastCardsAppear() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a"), item("b")], nextCursor: "c1")
        source.pages["c1"] = CoreRecentItemsPage(items: [item("c")], nextCursor: nil)
        let model = makeModel()
        await settle()
        model.loadMoreIfNeeded(afterAppearing: "a")
        await settle()
        XCTAssertEqual(model.entries.map(\.id), ["a", "b", "c"])
        XCTAssertEqual(source.readCursors, [nil, "c1"])
        model.loadMoreIfNeeded(afterAppearing: "c")
        await settle()
        XCTAssertEqual(source.readCursors.count, 2, "no cursor, no further read")
    }

    func testANewSaveLeadsTheListAsSortingUntilInterpreted() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("old")], nextCursor: nil)
        let model = makeModel()
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        XCTAssertEqual(model.entries.map(\.id), ["new", "old"])
        XCTAssertTrue(model.entries[0].isSorting)
        XCTAssertEqual(HomeBodyView.caption(for: model.entries[0]), "Just now \u{00B7} sorting\u{2026}")

        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        model.captureDidSave(itemID: "new", interpretationIsResolved: true)
        await settle()
        XCTAssertEqual(model.entries.map(\.id), ["new", "old"])
        XCTAssertFalse(model.entries[0].isSorting)
    }

    func testSortingClearsOnItsOwnWhenAPollSeesTheInterpretedItem() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("old")], nextCursor: nil)
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: nil, capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        await settle()
        await pauses.releaseAll()
        await settle()
        XCTAssertTrue(model.entries[0].isSorting, "still unclassified after the first poll")
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        await pauses.releaseAll()
        await settle()
        XCTAssertFalse(model.entries[0].isSorting)
        XCTAssertTrue(model.justSaved.isEmpty)
    }

    func testARelockStopsThePolling() async {
        source.isAuthenticated = true
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await PollPauses().wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        await settle()
        source.setAccess(authenticated: false)
        let readsAtRelock = source.readCursors.count
        let statusReadsAtRelock = source.statusReadItemIDs.count
        await settle()
        XCTAssertEqual(source.readCursors.count, readsAtRelock)
        XCTAssertEqual(source.statusReadItemIDs.count, statusReadsAtRelock)
    }

    func testSortingClearsWhenInterpretationEndsWithoutAType() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("old")], nextCursor: nil)
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: nil, capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        await settle()
        await pauses.releaseAll()
        await settle()
        XCTAssertTrue(model.entries[0].isSorting, "still pending while the status says so")
        XCTAssertEqual(source.statusReadItemIDs, ["new"])

        // Abstained, uninterpreted or a topic-only result: finished, and the page still carries no type.
        source.finishedInterpretations = ["new"]
        await pauses.releaseAll()
        await settle()
        XCTAssertEqual(model.entries.map(\.id), ["new", "old"])
        XCTAssertFalse(model.entries[0].isSorting)
        XCTAssertNil(model.entries[0].itemType)
        XCTAssertTrue(model.justSaved.isEmpty)
        XCTAssertTrue(
            HomeBodyView.caption(for: model.entries[0], now: referenceNow)
                .hasPrefix(HomeBodyView.unsortedTypeTitle + " \u{00B7} "))

        let readsAfterClearing = source.readCursors.count
        let statusReadsAfterClearing = source.statusReadItemIDs.count
        await pauses.releaseAll()
        await settle()
        XCTAssertEqual(source.readCursors.count, readsAfterClearing, "polling stops once nothing is sorting")
        XCTAssertEqual(source.statusReadItemIDs.count, statusReadsAfterClearing)
    }

    func testATypedPageNeedsNoStatusRead() async {
        source.isAuthenticated = true
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z")], nextCursor: nil)
        await settle()
        await pauses.releaseAll()
        await settle()
        XCTAssertTrue(model.justSaved.isEmpty)
        XCTAssertTrue(source.statusReadItemIDs.isEmpty)
    }

    func testTheProductionSourceTreatsEveryEndedInterpretationAsFinished() throws {
        for state in ["processed", "abstained", "uninterpreted"] {
            XCTAssertEqual(
                RootHomeRecentsSource.interpretationIsFinished(.success(try Self.statusReport(processing: state))), true,
                state)
        }
        for state in ["unprocessed", "processing"] {
            XCTAssertEqual(
                RootHomeRecentsSource.interpretationIsFinished(.success(try Self.statusReport(processing: state))), false,
                state)
        }
        XCTAssertNil(RootHomeRecentsSource.interpretationIsFinished(.failure(.handleClosed)))
    }

    private static func statusReport(processing: String) throws -> ItemStatusReport {
        let document = """
            {"operation_id":1,"item_id":"item-1","save_state":"saved","sync_state":"synced",
            "processing_state":"\(processing)","transcription_state":"not_applicable",
            "processing_job_status":null,"reminder_request_state":null,"reminder_schedule_state":null,
            "reminder_delivery_state":null,"reminder_acknowledgment_state":null,"unschedulable_reason":null}
            """
        return try JSONDecoder().decode(ItemStatusReport.self, from: Data(document.utf8))
    }

    func testPollingOutlastsManyAttemptsUntilTheItemIsInterpreted() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("old")], nextCursor: nil)
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: nil, capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        await settle()
        for _ in 0..<30 {
            await pauses.releaseAll()
            await settle()
        }
        XCTAssertTrue(model.entries[0].isSorting, "still pending after more attempts than the old limit")
        let readsBefore = source.readCursors.count
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z"), item("old")], nextCursor: nil)
        await pauses.releaseAll()
        await settle()
        XCTAssertGreaterThan(source.readCursors.count, readsBefore)
        XCTAssertFalse(model.entries[0].isSorting)
    }

    func testShutDownCancelsReadsAndStartsNoMore() async {
        source.isAuthenticated = true
        source.holdsReads = true
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await PollPauses().wait() })
        await settle()
        model.shutDown()
        source.holdsReads = false
        source.releaseHeldReads()
        await settle()
        let reads = source.readCursors.count
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        model.loadFirstPage()
        await settle()
        XCTAssertEqual(source.readCursors.count, reads)
        XCTAssertTrue(model.entries.isEmpty)
    }

    func testASaveWhileLockedIsNotRecorded() async {
        let model = makeModel()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        XCTAssertTrue(model.justSaved.isEmpty)
        XCTAssertTrue(model.entries.isEmpty)
    }

    func testRelockForgetsTheJustSavedItems() async {
        source.isAuthenticated = true
        let model = makeModel()
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.setAccess(authenticated: false)
        XCTAssertTrue(model.justSaved.isEmpty)
    }

    func testUnlockRunsTheExplicitAuthentication() {
        let model = makeModel()
        model.unlock()
        XCTAssertEqual(source.unlockCount, 1)
    }

    func testAFailedReadKeepsTheListAndOffersARetry() async {
        source.isAuthenticated = true
        source.failure = RecentItemsError.core(CoreFailure.handleClosed)
        let model = makeModel()
        await settle()
        XCTAssertTrue(model.loadFailed)
        XCTAssertEqual(model.bodyState, .unlocked)
    }

    // MARK: Item changes

    func testADeletedItemLeavesTheListAtOnceAndTheFirstPageIsReadAgain() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a"), item("b")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.pages[nil] = CoreRecentItemsPage(items: [item("b")], nextCursor: nil)
        source.publish(.item("a"))
        XCTAssertEqual(model.entries.map(\.id), ["b"], "gone before the read returns")
        await settle()
        XCTAssertEqual(source.readCursors.count, 2)
        XCTAssertEqual(model.entries.map(\.id), ["b"])
    }

    func testAChangedItemComesBackWithItsCurrentText() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a", text: "old words")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.pages[nil] = CoreRecentItemsPage(items: [item("a", text: "new words")], nextCursor: nil)
        source.publish(.item("a"))
        await settle()
        XCTAssertEqual(model.entries.map(\.text), ["new words"])
    }

    func testAChangeToAnItemTheListDoesNotShowReadsNothing() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.publish(.item("elsewhere"))
        await settle()
        XCTAssertEqual(source.readCursors.count, 1)
        XCTAssertEqual(model.entries.map(\.id), ["a"])
    }

    func testAChangeWhileLockedLoadsNothing() async {
        let model = makeModel()
        source.publish(.item("a"))
        source.publish(.allItems)
        await settle()
        XCTAssertTrue(source.readCursors.isEmpty)
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertEqual(model.bodyState, .locked)
    }

    func testAllItemsClearsTheListAndReadsTheFirstPageAgain() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a"), item("b")], nextCursor: "next")
        let model = makeModel()
        await settle()
        source.pages[nil] = CoreRecentItemsPage(items: [], nextCursor: nil)
        source.publish(.allItems)
        XCTAssertTrue(model.entries.isEmpty)
        await settle()
        XCTAssertEqual(source.readCursors.count, 2)
        XCTAssertTrue(model.entries.isEmpty)
        XCTAssertFalse(model.canLoadMore)
    }

    func testAChangeDuringTheFirstReadReadsAgainSoAnOutdatedPageDoesNotStay() async {
        source.isAuthenticated = true
        source.holdsReads = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.publish(.item("a"))
        source.pages[nil] = CoreRecentItemsPage(items: [], nextCursor: nil)
        source.holdsReads = false
        source.releaseHeldReads()
        await settle()
        XCTAssertEqual(source.readCursors.count, 2)
        XCTAssertTrue(model.entries.isEmpty)
    }

    func testAChangeToAnItemOnAPageStillBeingReadNeverAppendsItsOldState() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: "second")
        source.pages["second"] = CoreRecentItemsPage(items: [item("z", text: "old words")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.holdsReads = true
        model.loadMore()
        await settle()
        XCTAssertEqual(source.readCursors, [nil, "second"], "the next page is being read")

        source.publish(.item("z"))
        source.pages[nil] = CoreRecentItemsPage(items: [item("a"), item("z", text: "new words")], nextCursor: nil)
        source.holdsReads = false
        source.releaseHeldReads()
        await settle()

        XCTAssertEqual(model.entries.map(\.id), ["a", "z"])
        XCTAssertEqual(model.entries.map(\.text), ["Synthetic text", "new words"], "never the page read before the change")
    }

    func testADeletedItemOnAPageStillBeingReadNeverAppears() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: "second")
        source.pages["second"] = CoreRecentItemsPage(items: [item("z")], nextCursor: nil)
        let model = makeModel()
        await settle()
        source.holdsReads = true
        model.loadMore()
        await settle()

        source.publish(.item("z"))
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        source.holdsReads = false
        source.releaseHeldReads()
        await settle()

        XCTAssertEqual(model.entries.map(\.id), ["a"])
    }

    func testAPollPageReadBeforeADeletionNeverBringsTheItemBack() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a"), item("b")], nextCursor: nil)
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: nil, capturedAt: "2026-10-19T11:59:00Z"), item("a"), item("b")], nextCursor: nil)
        await settle()

        source.heldSnapshotReads = 1
        await pauses.releaseAll()
        await settle()
        XCTAssertEqual(source.readCursors.count, 3, "the poll's read is held with the page before the deletion")

        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: nil, capturedAt: "2026-10-19T11:59:00Z"), item("b")], nextCursor: nil)
        source.publish(.item("a"))
        await settle()
        XCTAssertEqual(model.entries.map(\.id), ["new", "b"], "the reload after the deletion has landed")

        source.releaseHeldReads()
        await settle()
        XCTAssertEqual(model.entries.map(\.id), ["new", "b"], "the poll's older page never restores the deleted item")
    }

    func testARefreshPageReadBeforeACorrectionNeverRestoresTheOldText() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a", text: "old words")], nextCursor: nil)
        let pauses = PollPauses()
        let model = HomeRecentsModel(source: source, now: { [referenceNow] in referenceNow }, pause: { _ in await pauses.wait() })
        await settle()
        model.captureDidSave(itemID: "new", interpretationIsResolved: false)
        await settle()

        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z"), item("a", text: "old words")],
            nextCursor: nil)
        source.heldSnapshotReads = 1
        model.captureDidSave(itemID: "new", interpretationIsResolved: true)
        await settle()

        source.pages[nil] = CoreRecentItemsPage(
            items: [item("new", type: "idea", capturedAt: "2026-10-19T11:59:00Z"), item("a", text: "new words")],
            nextCursor: nil)
        source.publish(.item("a"))
        await settle()
        XCTAssertEqual(model.entries.map(\.text), ["Synthetic text", "new words"], "the reload after the correction has landed")

        source.releaseHeldReads()
        await settle()
        XCTAssertEqual(model.entries.map(\.text), ["Synthetic text", "new words"], "the refresh's older page never restores old text")
        XCTAssertFalse(model.entries[0].isSorting, "the refresh still ends the sorting state with a current page")
        XCTAssertTrue(model.justSaved.isEmpty)
    }

    func testAChangeAfterShutdownDoesNothing() async {
        source.isAuthenticated = true
        source.pages[nil] = CoreRecentItemsPage(items: [item("a")], nextCursor: nil)
        let model = makeModel()
        await settle()
        model.shutDown()
        source.publish(.allItems)
        await settle()
        XCTAssertEqual(source.readCursors.count, 1)
    }

    func testWordingDoesNotLeakIntoTheLockedCard() {
        let visible = [HomeBodyView.lockedTitle, HomeBodyView.lockedDetail, HomeBodyView.unlockTitle].joined(separator: " ")
        XCTAssertFalse(visible.contains("Synthetic"))
        XCTAssertEqual(HomeBodyView.typeTitle(for: nil), "Not yet sorted")
        XCTAssertEqual(HomeBodyView.typeTitle(for: "idea"), "Idea")
    }
}
