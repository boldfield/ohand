import Combine
import Foundation
import OhAndServices

/// What the search screen is showing.
enum RetrievalSearchPhase: Equatable {
    /// Nothing has been searched, or the request lacked words and filters.
    case idle
    case searching
    /// At least one result row is shown.
    case results
    /// The search ran and nothing matched.
    case empty
    /// The read session is not authenticated, so nothing stored is shown.
    case locked
    case failed(RetrievalSearchFailure)
}

/// The state of the search screen: the words and filters, the page of results and what to say about them.
///
/// Rules this type keeps:
/// * Stored content appears only while the read session is authenticated. The scope is checked before a search is sent
///   and again when its answer arrives; a relock in between empties the screen instead of showing counts or snippets.
/// * A search needs words or a filter. It never asks a provider or the network anything.
/// * Selecting a result hands the host a validated `RetrievalItemReference`, never the row's text. Where the screen
///   shows the item beside the results (regular width), the selection is kept here instead and nothing is pushed.
/// * Leaving the screen, or losing the session, drops every result and the typed words. Opening a result's detail
///   while authenticated is the one exception: the search is still there on return.
///
/// Confined to the main queue.
final class RetrievalSearchModel: ObservableObject {
    @Published var query = ""
    @Published var sessionTopic = ""
    @Published var onlyWithSessionTopic = false
    @Published var since: Date?
    @Published var until: Date?
    @Published var itemType: RetrievalItemType?

    @Published private(set) var phase: RetrievalSearchPhase = .idle
    @Published private(set) var rows: [RetrievalResultRow] = []
    @Published private(set) var totalAccessible = 0
    @Published private(set) var canLoadMore = false
    @Published private(set) var problem: String?
    /// The result shown in the item pane beside the results. Only set while the session is authenticated and the row
    /// is on screen; a new search, a relock or leaving the screen drops it.
    @Published private(set) var selectedReference: RetrievalItemReference?
    /// Set by the screen while it has room to show an item beside the results (regular width with a pane).
    var presentsItemsInPane = false {
        didSet { if !presentsItemsInPane { selectedReference = nil } }
    }

    private let searcher: RetrievalSearching
    private let readScope: () -> CompositionReadScope
    private let onSelect: (RetrievalItemReference) -> Void
    private let calendar: Calendar
    private var generation = 0
    private var lastRequest: RetrievalSearchRequest?
    /// The request whose answer is awaited, before any search has completed.
    private var inFlightRequest: RetrievalSearchRequest?
    private var handedOffToDetail = false
    /// True from leaving for a result's detail (search kept) until the screen is back.
    private var awayInDetail = false
    private var isInDetail: Bool { handedOffToDetail || awayInDetail }
    /// True once an item changed after the shown results were asked for, so they may be out of date.
    private var resultsMayBeStale = false
    private var removeChangeObserver: (() -> Void)?

    init(
        searcher: RetrievalSearching,
        readScope: @escaping () -> CompositionReadScope,
        onSelect: @escaping (RetrievalItemReference) -> Void,
        calendar: Calendar = .current,
        observeItemChanges: (@escaping @Sendable (ItemChange) -> Void) -> () -> Void = { _ in {} }
    ) {
        self.searcher = searcher
        self.readScope = readScope
        self.onSelect = onSelect
        self.calendar = calendar
        removeChangeObserver = observeItemChanges { [weak self] change in
            if Thread.isMainThread {
                self?.itemsDidChange(change)
            } else {
                DispatchQueue.main.async { self?.itemsDidChange(change) }
            }
        }
    }

    deinit {
        removeChangeObserver?()
    }

    var currentRequest: RetrievalSearchRequest {
        RetrievalSearchRequest(
            query: query, sessionTopic: sessionTopic, onlyWithSessionTopic: onlyWithSessionTopic,
            since: since, until: until, itemType: itemType)
    }

    /// Runs the current words and filters from the first result.
    func search() {
        run(currentRequest)
    }

    /// Fetches the next page of the last search that ran and appends it.
    func loadMore() {
        guard canLoadMore, phase == .results, var next = lastRequest else { return }
        next.offset = rows.count
        run(next, appending: true)
    }

    /// Passes the row's validated identifier to detail navigation, or, while the screen presents items in its pane, keeps
    /// it as the item the pane shows. Does nothing unless the session is authenticated and the row is currently shown.
    func select(_ row: RetrievalResultRow) {
        guard readScope() == .authenticated, rows.contains(where: { $0.reference == row.reference }) else {
            clear()
            return
        }
        if presentsItemsInPane {
            selectedReference = row.reference
            return
        }
        handedOffToDetail = true
        onSelect(row.reference)
    }

    /// Call when the screen appears or the app becomes active: a session that is no longer authenticated empties it.
    func revalidate() {
        handedOffToDetail = false
        awayInDetail = false
        if readScope() != .authenticated, phase != .idle {
            clear()
            phase = .locked
            return
        }
        if resultsMayBeStale { runAgain() }
    }

    /// An item changed (deleted, corrected, completed, cancelled, reminder edited, or everything reset). The result for
    /// it is dropped at once, so a deleted item is never shown while the query runs again. The query runs again right
    /// away when the results are on screen (or beside an item pane), and on return when the person is in an item's
    /// detail. Results nobody asked for yet, or none, need nothing.
    func itemsDidChange(_ change: ItemChange) {
        guard phase != .idle, phase != .locked else { return }
        resultsMayBeStale = true
        switch change {
        case .allItems:
            rows = []
            totalAccessible = 0
            canLoadMore = false
            selectedReference = nil
        case .item(let itemID):
            let before = rows.count
            rows.removeAll { $0.reference.value == itemID.lowercased() }
            totalAccessible = max(0, totalAccessible - (before - rows.count))
        }
        if !isInDetail { runAgain() }
    }

    /// Runs the last query again from its first result, keeping the item pane's selection.
    private func runAgain() {
        guard let request = lastRequest ?? (phase == .searching ? inFlightRequest : nil) else { return }
        resultsMayBeStale = false
        var first = request
        first.offset = 0
        run(first, restoring: selectedReference)
    }

    /// Call when the screen goes away. Going on to a result's detail keeps the search, so coming back finds it, but only
    /// while the session is still authenticated; any other way of leaving drops everything.
    func screenDidDisappear() {
        let keepsSearch = handedOffToDetail && readScope() == .authenticated
        handedOffToDetail = false
        awayInDetail = keepsSearch
        if !keepsSearch { clear() }
    }

    /// Drops every result and the typed words, and ignores any search still running.
    func clear() {
        generation += 1
        query = ""
        sessionTopic = ""
        onlyWithSessionTopic = false
        since = nil
        until = nil
        itemType = nil
        phase = .idle
        rows = []
        totalAccessible = 0
        canLoadMore = false
        problem = nil
        lastRequest = nil
        selectedReference = nil
        resultsMayBeStale = false
        awayInDetail = false
        inFlightRequest = nil
    }

    private func run(
        _ request: RetrievalSearchRequest, appending: Bool = false, restoring: RetrievalItemReference? = nil
    ) {
        problem = nil
        if let message = request.problem(calendar: calendar) {
            problem = message
            return
        }
        guard readScope() == .authenticated else {
            clear()
            phase = .locked
            return
        }
        generation += 1
        let thisGeneration = generation
        if !appending {
            // A repeat of the query keeps the pane's item; a new search drops it.
            if restoring == nil { selectedReference = nil }
            rows = []
            totalAccessible = 0
            canLoadMore = false
        }
        phase = .searching
        inFlightRequest = request
        searcher.search(request) { [weak self] result in
            DispatchQueue.main.async {
                self?.finish(
                    result, for: request, generation: thisGeneration, appending: appending, restoring: restoring)
            }
        }
    }

    private func finish(
        _ result: Result<RetrievalPage, RetrievalSearchFailure>, for request: RetrievalSearchRequest,
        generation finishedGeneration: Int, appending: Bool, restoring: RetrievalItemReference? = nil
    ) {
        guard finishedGeneration == generation else { return }
        guard readScope() == .authenticated else {
            clear()
            phase = .locked
            return
        }
        // An item changed while this was being read, so the page may still hold it. Ask again instead of showing it.
        if resultsMayBeStale, case .success = result {
            lastRequest = request
            if isInDetail {
                if !appending { rows = [] }
            } else {
                runAgain()
            }
            return
        }
        switch result {
        case .failure(let failure):
            if appending {
                phase = .results
                problem = failure.message
            } else {
                rows = []
                totalAccessible = 0
                canLoadMore = false
                selectedReference = nil
                phase = (failure == .notAuthenticated || failure == .relocked) ? .locked : .failed(failure)
            }
        case .success(let page):
            lastRequest = request
            totalAccessible = page.totalAccessible
            rows = appending ? rows + page.rows : page.rows
            canLoadMore = page.hasMore
            phase = rows.isEmpty ? .empty : .results
            // The pane keeps its item even when the row is gone: a deleted item still shows its deletion result there,
            // until the person selects another result or searches again.
        }
    }
}

extension RetrievalSearchModel {
    /// The model as the app assembles it: reads follow the root's session, and a selected result is handed to the
    /// management navigation, which pushes the detail screen above the search so Back returns to it.
    static func assembled(
        searcher: RetrievalSearching, root: ServiceCompositionRoot, navigation: ManagementNavigationModel
    ) -> RetrievalSearchModel {
        RetrievalSearchModel(
            searcher: searcher,
            readScope: { [weak root] in root?.readScope ?? .captureOnly },
            onSelect: { [weak navigation] reference in navigation?.pushItem(itemID: reference.value) },
            observeItemChanges: { [weak root] handler in root?.observeItemChanges(handler) ?? {} })
    }
}
