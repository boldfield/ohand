import Foundation
import OhAndCoreBridge
import OhAndServices

/// What the home needs from the read session and the recent-captures read. Every read goes through the private read
/// gate, so a capture-only session is refused there as well; the model still never asks.
protocol HomeRecentsSource: AnyObject {
    var isAuthenticated: Bool { get }
    var isQuietAttemptInFlight: Bool { get }
    func readPage(cursor: String?) async throws -> CoreRecentItemsPage
    /// Whether the saved item's interpretation has reached an end: processed, abstained or uninterpreted. An end need not
    /// assign a type, so the type alone cannot tell the home that sorting is over.
    func interpretationIsFinished(itemID: String) async throws -> Bool
    /// Runs the explicit authentication (biometrics, passcode fallback).
    func unlock()
    /// Calls the handler after every scope change and every change of the quiet-attempt flag, on any thread. Returns the
    /// closure that removes the registration.
    func observeAccess(_ handler: @escaping @Sendable () -> Void) -> () -> Void
    /// Calls the handler on the main thread after an item changed (see `ItemChange`). Returns the closure that removes
    /// the registration.
    func observeItemChanges(_ handler: @escaping @Sendable (ItemChange) -> Void) -> () -> Void
}

extension HomeRecentsSource {
    func observeItemChanges(_ handler: @escaping @Sendable (ItemChange) -> Void) -> () -> Void { {} }
}

/// The three states of the home body. Only `unlocked` ever holds anything derived from stored history.
enum HomeBodyState: Equatable {
    case unlocked
    case unlocking
    case locked
}

/// One card on the home.
struct HomeRecentEntry: Identifiable, Equatable {
    let id: String
    /// The item's type, or nil while it has not been classified.
    let itemType: String?
    let text: String?
    let capturedAt: Date
    let isVoice: Bool
    /// True for a capture saved in this session that has not been interpreted yet.
    let isSorting: Bool
    /// True when the note's reminder was asked for and has no time. Content-free.
    var needsTime = false
}

enum HomeRecentsSection: Equatable {
    case today
    case earlier
}

/// Holds the recent captures the home shows. They exist only while the session is authenticated: a relock drops them at
/// once and no read is started in any other state.
///
/// Confined to the main queue. The session tells the model about access changes on whichever thread caused them; a change
/// on the main thread is applied before the caller returns.
final class HomeRecentsModel: ObservableObject {
    @Published private(set) var bodyState: HomeBodyState
    @Published private(set) var loadedItems: [CoreRecentItem] = []
    @Published private(set) var isLoadingFirstPage = false
    @Published private(set) var isLoadingMore = false
    @Published private(set) var loadFailed = false
    /// Items saved in this session that have not been interpreted yet, with when they were saved.
    @Published private(set) var justSaved: [String: Date] = [:]

    private let source: HomeRecentsSource
    private let now: () -> Date
    private var nextCursor: String?
    /// Raised at every loss of access, so a page that arrives afterwards is dropped.
    private var generation = 0
    private var removeObserver: (() -> Void)?
    private var removeChangeObserver: (() -> Void)?
    /// How many items the last first-page read returned: the head of the list that a reload replaces.
    private var headCount = 0
    /// Set when an item changed while the first page was being read, so that read may predate the change.
    private var reloadFirstPageWhenDone = false
    /// Items that changed while the first page was being read; that page may still hold them in their old state.
    private var changedDuringFirstRead: Set<String> = []
    /// Raised at every item change. A first-page read started before the latest change may hold an item in its old
    /// state, so the poll and the post-interpretation refresh merge only a page read entirely after it.
    private var itemChangeCount = 0

    /// Waits before the given poll attempt; tests replace it so no real time passes.
    private let pause: (TimeInterval) async -> Void
    private var pollTask: Task<Void, Never>?
    private static let firstPollDelay: TimeInterval = 2
    private static let longestPollDelay: TimeInterval = 30
    /// The reads in flight, kept so a shutdown or a relock can cancel them.
    private var firstPageTask: Task<Void, Never>?
    private var moreTask: Task<Void, Never>?
    private var refreshTasks: [UUID: Task<Void, Never>] = [:]
    private var isShutDown = false

    init(
        source: HomeRecentsSource, now: @escaping () -> Date = Date.init,
        pause: @escaping (TimeInterval) async -> Void = { try? await Task.sleep(nanoseconds: UInt64($0 * 1_000_000_000)) }
    ) {
        self.source = source
        self.now = now
        self.pause = pause
        bodyState = Self.state(for: source)
        removeObserver = source.observeAccess { [weak self] in
            if Thread.isMainThread {
                self?.accessDidChange()
            } else {
                DispatchQueue.main.async { self?.accessDidChange() }
            }
        }
        removeChangeObserver = source.observeItemChanges { [weak self] change in
            if Thread.isMainThread {
                self?.itemsDidChange(change)
            } else {
                DispatchQueue.main.async { self?.itemsDidChange(change) }
            }
        }
        if bodyState == .unlocked { loadFirstPage() }
    }

    deinit {
        cancelReads()
        removeObserver?()
        removeChangeObserver?()
    }

    /// Ends the model: no read starts afterwards, every read in flight is cancelled and the observer is removed. Called
    /// before the services behind the session shut down, so no read outlives them.
    func shutDown() {
        isShutDown = true
        removeObserver?()
        removeObserver = nil
        removeChangeObserver?()
        removeChangeObserver = nil
        dropHistory()
    }

    private func cancelReads() {
        pollTask?.cancel()
        pollTask = nil
        firstPageTask?.cancel()
        firstPageTask = nil
        moreTask?.cancel()
        moreTask = nil
        refreshTasks.values.forEach { $0.cancel() }
        refreshTasks = [:]
    }

    private static func state(for source: HomeRecentsSource) -> HomeBodyState {
        if source.isAuthenticated { return .unlocked }
        return source.isQuietAttemptInFlight ? .unlocking : .locked
    }

    func unlock() {
        source.unlock()
    }

    /// Re-derives the state from the session. Losing access clears the list in the same turn.
    func accessDidChange() {
        let newState = Self.state(for: source)
        if newState != .unlocked {
            dropHistory()
        }
        let wasUnlocked = bodyState == .unlocked
        bodyState = newState
        if newState == .unlocked, !wasUnlocked || (loadedItems.isEmpty && !isLoadingFirstPage) {
            loadFirstPage()
        }
    }

    private func dropHistory() {
        generation += 1
        cancelReads()
        loadedItems = []
        justSaved = [:]
        nextCursor = nil
        isLoadingFirstPage = false
        isLoadingMore = false
        loadFailed = false
        headCount = 0
        reloadFirstPageWhenDone = false
        changedDuringFirstRead = []
    }

    // MARK: Item changes

    /// An item was deleted, corrected, completed, cancelled or had its reminder changed (or a reset removed them all).
    /// A locked home holds nothing, so it loads nothing; the unlock reads the list afresh. An item the list does not show
    /// needs nothing. A shown one is dropped at once, so a deleted item is gone before the read returns, and the first
    /// page is read again so a changed one comes back current.
    func itemsDidChange(_ change: ItemChange) {
        itemChangeCount += 1
        guard !isShutDown, bodyState == .unlocked else { return }
        switch change {
        case .allItems:
            resetList()
        case .item(let itemID):
            let wasSorting = justSaved.removeValue(forKey: itemID) != nil
            if isLoadingFirstPage { changedDuringFirstRead.insert(itemID) }
            guard let index = loadedItems.firstIndex(where: { $0.itemID == itemID }) else {
                if isLoadingMore {
                    // The next page being read may hold the item in its old state; start over rather than append it.
                    resetList()
                    return
                }
                // A first page still being read may predate the change and hold the item.
                if wasSorting || isLoadingFirstPage { reloadFirstPage() }
                return
            }
            if index >= headCount {
                // Past the head, a reload of the first page would not bring the item back.
                resetList()
                return
            }
            loadedItems.remove(at: index)
            reloadFirstPage()
        }
    }

    private func resetList() {
        dropHistory()
        loadFirstPage()
    }

    private func reloadFirstPage() {
        if isLoadingFirstPage {
            reloadFirstPageWhenDone = true
        } else {
            loadFirstPage()
        }
    }

    // MARK: Entries

    /// Newest first. Captures saved in this session and not yet sorted lead the list.
    var entries: [HomeRecentEntry] {
        guard bodyState == .unlocked else { return [] }
        let loadedIDs = Set(loadedItems.map(\.itemID))
        let pending = justSaved
            .filter { !loadedIDs.contains($0.key) }
            .sorted { $0.value > $1.value }
            .map { HomeRecentEntry(id: $0.key, itemType: nil, text: nil, capturedAt: $0.value, isVoice: false, isSorting: true) }
        let loaded = loadedItems.map { item in
            HomeRecentEntry(
                id: item.itemID, itemType: item.itemType, text: item.displayText,
                capturedAt: Self.date(from: item.capturedAt) ?? now(), isVoice: item.sourceKind == .voice,
                isSorting: item.itemType == nil && justSaved[item.itemID] != nil,
                needsTime: item.reminderNeedsTime)
        }
        let leadingSorting = loaded.filter(\.isSorting)
        return pending + leadingSorting + loaded.filter { !$0.isSorting }
    }

    /// The time relative dates are measured from; the same clock the sections use.
    var currentDate: Date { now() }

    func section(for entry: HomeRecentEntry, calendar: Calendar = .current) -> HomeRecentsSection {
        entry.isSorting || calendar.isDate(entry.capturedAt, inSameDayAs: now()) ? .today : .earlier
    }

    static func date(from rfc3339: String) -> Date? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = withFraction.date(from: rfc3339) { return date }
        return ISO8601DateFormatter().date(from: rfc3339)
    }

    // MARK: Reads

    /// Reads started and not yet returned. A cancelled task still waits for the core's answer, so shutdown cannot end
    /// them at once; tests wait for this to reach zero before checking that nothing leaked.
    private(set) var readsInFlight = 0

    private func readPage(cursor: String?) async throws -> CoreRecentItemsPage {
        readsInFlight += 1
        defer { readsInFlight -= 1 }
        return try await source.readPage(cursor: cursor)
    }

    private func interpretationIsFinished(itemID: String) async throws -> Bool {
        readsInFlight += 1
        defer { readsInFlight -= 1 }
        return try await source.interpretationIsFinished(itemID: itemID)
    }

    /// Reads the status of each saved item still unsorted and drops the ones whose interpretation ended without a type.
    @MainActor
    private func clearFinishedInterpretations(startedIn: Int) async throws {
        for itemID in justSaved.keys.sorted() {
            guard !Task.isCancelled, startedIn == generation else { return }
            let finished = try await interpretationIsFinished(itemID: itemID)
            guard startedIn == generation, source.isAuthenticated else { return }
            if finished { justSaved.removeValue(forKey: itemID) }
        }
    }

    var canLoadMore: Bool { nextCursor != nil }

    func loadFirstPage() {
        guard !isShutDown, source.isAuthenticated, !isLoadingFirstPage else { return }
        isLoadingFirstPage = true
        loadFailed = false
        let startedIn = generation
        firstPageTask = Task { @MainActor [weak self] in
            await self?.performFirstPage(startedIn: startedIn)
        }
    }

    @MainActor
    private func performFirstPage(startedIn: Int) async {
        // The task may have been cancelled before it first ran; no read starts then.
        guard !Task.isCancelled, startedIn == generation else { return }
        do {
            let page = try await readPage(cursor: nil)
            guard startedIn == generation, source.isAuthenticated else { return }
            merge(firstPage: page)
            isLoadingFirstPage = false
        } catch {
            handleFailure(error, startedIn: startedIn)
            if startedIn == generation { isLoadingFirstPage = false }
        }
        if startedIn == generation, reloadFirstPageWhenDone {
            reloadFirstPageWhenDone = false
            // The page just merged may have brought a changed item back; the next read puts it right.
            loadedItems.removeAll { changedDuringFirstRead.contains($0.itemID) }
            changedDuringFirstRead = []
            loadFirstPage()
        }
    }

    /// Asks for the next page when the card that just appeared is among the last few.
    func loadMoreIfNeeded(afterAppearing itemID: String) {
        let identifiers = loadedItems.map(\.itemID)
        guard let index = identifiers.firstIndex(of: itemID), index >= identifiers.count - 3 else { return }
        loadMore()
    }

    func loadMore() {
        guard !isShutDown, source.isAuthenticated, !isLoadingMore, !isLoadingFirstPage, let cursor = nextCursor else { return }
        isLoadingMore = true
        let startedIn = generation
        moreTask = Task { @MainActor [weak self] in
            await self?.performMore(cursor: cursor, startedIn: startedIn)
        }
    }

    @MainActor
    private func performMore(cursor: String, startedIn: Int) async {
        guard !Task.isCancelled, startedIn == generation else { return }
        do {
            let page = try await readPage(cursor: cursor)
            guard startedIn == generation, source.isAuthenticated else { return }
            let known = Set(loadedItems.map(\.itemID))
            loadedItems += page.items.filter { !known.contains($0.itemID) }
            nextCursor = page.nextCursor
            isLoadingMore = false
        } catch {
            handleFailure(error, startedIn: startedIn)
            if startedIn == generation { isLoadingMore = false }
        }
    }

    private func handleFailure(_ error: Error, startedIn: Int) {
        guard startedIn == generation else { return }
        // A refusal means access ended under the read; anything else leaves the list and offers a retry.
        guard source.isAuthenticated else {
            accessDidChange()
            return
        }
        loadFailed = true
    }

    /// Replaces the head of the list with a fresh first page, keeping the pages already scrolled to.
    private func merge(firstPage page: CoreRecentItemsPage) {
        headCount = page.items.count
        if loadedItems.isEmpty {
            loadedItems = page.items
            nextCursor = page.nextCursor
        } else {
            let fresh = Set(page.items.map(\.itemID))
            loadedItems = page.items + loadedItems.filter { !fresh.contains($0.itemID) }
        }
        for item in page.items where item.itemType != nil {
            justSaved.removeValue(forKey: item.itemID)
        }
    }

    // MARK: Saves

    /// Called with the acknowledgment's saved item. A new save is shown at the top as "Just now · sorting…" until it is
    /// interpreted; nothing is recorded unless reads are authenticated, because a locked home shows nothing of the kind.
    func captureDidSave(itemID: String, interpretationIsResolved: Bool) {
        guard !isShutDown, bodyState == .unlocked else { return }
        if justSaved[itemID] == nil, !interpretationIsResolved {
            justSaved[itemID] = now()
            loadFirstPage()
            startPollingForInterpretation()
        } else if interpretationIsResolved, justSaved[itemID] != nil {
            let taskID = UUID()
            refreshTasks[taskID] = Task { @MainActor [weak self] in
                await self?.refreshAfterInterpretation(itemID: itemID)
                self?.refreshTasks[taskID] = nil
            }
        }
    }

    /// Nothing tells the home when an interpretation finishes, so while saved items are unsorted the first page is read
    /// again, followed by the status of each saved item it does not show as typed (an interpretation can end without a
    /// type). The pauses grow to a longest pause and then repeat, so a slow interpretation still clears. It stops when
    /// none are left, on a relock or at shutdown.
    private func startPollingForInterpretation() {
        guard pollTask == nil else { return }
        let startedIn = generation
        pollTask = Task { @MainActor [weak self] in
            var delay = Self.firstPollDelay
            while true {
                guard let pause = self?.pause else { return }
                await pause(delay)
                guard let self, !Task.isCancelled, startedIn == self.generation, !self.justSaved.isEmpty else { break }
                delay = min(delay * 2, Self.longestPollDelay)
                do {
                    let changesBeforeRead = self.itemChangeCount
                    let page = try await self.readPage(cursor: nil)
                    guard startedIn == self.generation, self.source.isAuthenticated else { break }
                    // An item changed under the read: the page may hold it as it was, so leave it to the next round.
                    guard changesBeforeRead == self.itemChangeCount else { continue }
                    self.merge(firstPage: page)
                    try await self.clearFinishedInterpretations(startedIn: startedIn)
                } catch {
                    self.handleFailure(error, startedIn: startedIn)
                    if startedIn != self.generation { break }
                }
                if self.justSaved.isEmpty { break }
            }
            guard let self, startedIn == self.generation else { return }
            self.pollTask = nil
        }
    }

    @MainActor
    private func refreshAfterInterpretation(itemID: String) async {
        let startedIn = generation
        do {
            while true {
                guard !Task.isCancelled, startedIn == generation else { return }
                let changesBeforeRead = itemChangeCount
                let page = try await readPage(cursor: nil)
                guard startedIn == generation, source.isAuthenticated else { return }
                // An item changed under the read: the page may hold it as it was, so read it again.
                guard changesBeforeRead == itemChangeCount else { continue }
                merge(firstPage: page)
                justSaved.removeValue(forKey: itemID)
                return
            }
        } catch {
            handleFailure(error, startedIn: startedIn)
        }
    }
}
