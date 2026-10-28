import Foundation
import OhAndCoreBridge
import OhAndServices

/// Runs one search and reports the page or why there is none. Completions may arrive on any thread.
protocol RetrievalSearching: AnyObject {
    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void)

    /// How many completed or cancelled items match the request's words and filters. A count-only query that asks the
    /// core for one result; a failure reports nothing at all.
    func countClosedMatches(for request: RetrievalSearchRequest, completion: @escaping (Int) -> Void)
}

extension RetrievalSearching {
    /// Searchers that cannot count closed matches say nothing, so no hint is shown.
    func countClosedMatches(for request: RetrievalSearchRequest, completion: @escaping (Int) -> Void) {}
}

/// Sends searches to the core through the root's private read gate and nothing else. The gate refuses a capture-only
/// session before anything reaches the core, states the session's own authorized scopes, and refuses an answer that
/// arrives after a relock; this module decodes an answer only after the gate admitted it. It works offline: the core
/// answers from its own store, with no provider, network or generated text involved.
final class RetrievalSearchModule: CompositionModule, RetrievalSearching, @unchecked Sendable {
    static let rangeStart: UInt64 = 1 << 44

    let identifier = "retrieval-search"
    let firstOperationID: UInt64 = RetrievalSearchModule.rangeStart

    private struct Pending {
        let request: RetrievalSearchRequest
        let completion: (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    }

    private let lock = NSLock()
    private let calendar: Calendar
    private var privateReads: CompositionPrivateReads?
    private var nextOperationID = RetrievalSearchModule.rangeStart
    private var pending: [UInt64: Pending] = [:]
    private var isShutDown = false

    init(calendar: Calendar = .current) {
        self.calendar = calendar
    }

    func attach(to context: CompositionContext) throws {
        lock.lock()
        privateReads = context.privateReads
        lock.unlock()
    }

    func search(
        _ request: RetrievalSearchRequest,
        completion: @escaping (Result<RetrievalPage, RetrievalSearchFailure>) -> Void
    ) {
        guard request.problem(calendar: calendar) == nil else {
            completion(.failure(.invalidRequest))
            return
        }
        lock.lock()
        guard !isShutDown, let reads = privateReads else {
            lock.unlock()
            completion(.failure(.unavailable))
            return
        }
        let operationID = nextOperationID
        nextOperationID += 1
        pending[operationID] = Pending(request: request, completion: completion)
        lock.unlock()

        let filters = request.privateFilters(calendar: calendar)
        do {
            if request.isWordSearch {
                try reads.startSearch(operationID: operationID, query: request.trimmedQuery, filters: filters)
            } else {
                try reads.startList(operationID: operationID, filters: filters)
            }
        } catch {
            lock.lock()
            let claimed = pending.removeValue(forKey: operationID)
            lock.unlock()
            let failure = (error as? CoreFailure).map(RetrievalSearchFailure.init(core:)) ?? .unavailable
            claimed?.completion(.failure(failure))
        }
    }

    func countClosedMatches(for request: RetrievalSearchRequest, completion: @escaping (Int) -> Void) {
        var counting = request
        counting.closedOnly = true
        counting.includeDone = false
        counting.offset = 0
        search(counting) { result in
            if case .success(let page) = result { completion(page.totalAccessible) }
        }
    }

    func handle(_ event: CoreEvent) -> Bool {
        lock.lock()
        let claimed = pending.removeValue(forKey: event.operationID)
        let reads = privateReads
        lock.unlock()
        guard let claimed else { return false }
        guard let reads else {
            claimed.completion(.failure(.unavailable))
            return true
        }
        do {
            try reads.admit(operationID: event.operationID)
        } catch {
            let failure = (error as? CoreFailure).map(RetrievalSearchFailure.init(core:)) ?? .relocked
            claimed.completion(.failure(failure))
            return true
        }
        switch event.outcome {
        case .failure(let failure):
            claimed.completion(.failure(RetrievalSearchFailure(core: failure)))
        case .success:
            do {
                let answer = try event.decode(PrivateRetrievalAnswer.self)
                claimed.completion(.success(try RetrievalPage.make(from: answer, for: claimed.request)))
            } catch let failure as RetrievalSearchFailure {
                claimed.completion(.failure(failure))
            } catch {
                claimed.completion(.failure(.unreadable))
            }
        }
        return true
    }

    func shutDown() {
        lock.lock()
        isShutDown = true
        let abandoned = pending
        pending = [:]
        lock.unlock()
        for waiting in abandoned.values {
            waiting.completion(.failure(.unavailable))
        }
    }
}
