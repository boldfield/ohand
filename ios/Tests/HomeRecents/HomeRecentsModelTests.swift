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
    /// When set, a read waits until `releaseHeldReads()`, so a test can relock while it is outstanding.
    var holdsReads = false
    private var heldReads: [CheckedContinuation<Void, Never>] = []
    private var handlers: [@Sendable () -> Void] = []

    func readPage(cursor: String?) async throws -> CoreRecentItemsPage {
        readCursors.append(cursor)
        if holdsReads {
            await withCheckedContinuation { heldReads.append($0) }
        }
        if let failure { throw failure }
        return pages[cursor] ?? CoreRecentItemsPage(items: [], nextCursor: nil)
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
        await settle()
        XCTAssertEqual(source.readCursors.count, readsAtRelock)
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

    func testWordingDoesNotLeakIntoTheLockedCard() {
        let visible = [HomeBodyView.lockedTitle, HomeBodyView.lockedDetail, HomeBodyView.unlockTitle].joined(separator: " ")
        XCTAssertFalse(visible.contains("Synthetic"))
        XCTAssertEqual(HomeBodyView.typeTitle(for: nil), "Not yet sorted")
        XCTAssertEqual(HomeBodyView.typeTitle(for: "idea"), "Idea")
    }
}
