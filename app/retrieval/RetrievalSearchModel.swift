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
/// * Selecting a result hands the host a validated `RetrievalItemReference`, never the row's text.
/// * Leaving the screen, or losing the session, drops every result and the typed words.
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

    private let searcher: RetrievalSearching
    private let readScope: () -> CompositionReadScope
    private let onSelect: (RetrievalItemReference) -> Void
    private let calendar: Calendar
    private var generation = 0
    private var lastRequest: RetrievalSearchRequest?

    init(
        searcher: RetrievalSearching,
        readScope: @escaping () -> CompositionReadScope,
        onSelect: @escaping (RetrievalItemReference) -> Void,
        calendar: Calendar = .current
    ) {
        self.searcher = searcher
        self.readScope = readScope
        self.onSelect = onSelect
        self.calendar = calendar
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

    /// Passes the row's validated identifier to detail navigation. Does nothing unless the session is authenticated and
    /// the row is currently shown.
    func select(_ row: RetrievalResultRow) {
        guard readScope() == .authenticated, rows.contains(where: { $0.reference == row.reference }) else {
            clear()
            return
        }
        onSelect(row.reference)
    }

    /// Call when the screen appears or the app becomes active: a session that is no longer authenticated empties it.
    func revalidate() {
        if readScope() != .authenticated, phase != .idle {
            clear()
            phase = .locked
        }
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
    }

    private func run(_ request: RetrievalSearchRequest, appending: Bool = false) {
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
            rows = []
            totalAccessible = 0
            canLoadMore = false
        }
        phase = .searching
        searcher.search(request) { [weak self] result in
            DispatchQueue.main.async {
                self?.finish(result, for: request, generation: thisGeneration, appending: appending)
            }
        }
    }

    private func finish(
        _ result: Result<RetrievalPage, RetrievalSearchFailure>, for request: RetrievalSearchRequest,
        generation finishedGeneration: Int, appending: Bool
    ) {
        guard finishedGeneration == generation else { return }
        guard readScope() == .authenticated else {
            clear()
            phase = .locked
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
                phase = (failure == .notAuthenticated || failure == .relocked) ? .locked : .failed(failure)
            }
        case .success(let page):
            lastRequest = request
            totalAccessible = page.totalAccessible
            rows = appending ? rows + page.rows : page.rows
            canLoadMore = page.hasMore
            phase = rows.isEmpty ? .empty : .results
        }
    }
}

extension RetrievalSearchModel {
    /// The model as the app assembles it: reads follow the root's session, and a selected result is handed to the
    /// management navigation, which holds it for the detail screen.
    static func assembled(
        searcher: RetrievalSearching, root: ServiceCompositionRoot, navigation: ManagementNavigationModel
    ) -> RetrievalSearchModel {
        RetrievalSearchModel(
            searcher: searcher,
            readScope: { [weak root] in root?.readScope ?? .captureOnly },
            onSelect: { [weak navigation] reference in navigation?.openItem(itemID: reference.value) })
    }
}
