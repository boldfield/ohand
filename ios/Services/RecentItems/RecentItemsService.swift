import Foundation
import OhAndCoreBridge

/// Why a page of recent captures was not returned. None of the cases carries an identifier or stored content.
public enum RecentItemsError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The session relocked after the read started; the core's answer was discarded undelivered.
    case relocked
    /// The page size or cursor was refused as out of bounds.
    case invalidRequest
    case core(CoreFailure)
}

/// Lists the most recent captures, newest first, through the private read gate, so the home can show them without a
/// search query.
///
/// * **Authenticated.** A capture-only session gets `notAuthenticated` and the core is not asked.
/// * **Relock-safe.** An answer that arrives after the session relocked is discarded (`relocked`) without being decoded.
/// * **Paged.** `page(limit:cursor:)` takes the previous page's `nextCursor`; the cursor is opaque.
public final class RecentItemsService: @unchecked Sendable {
    /// The page size the core uses when none is given.
    public static let defaultLimit = 30
    /// The largest page the core accepts.
    public static let maximumLimit = 100

    private let core: RecentItemsCore

    init(core: RecentItemsCore) {
        self.core = core
    }

    public func page(limit: Int? = nil, cursor: String? = nil) async throws -> CoreRecentItemsPage {
        if let limit, !(1...Self.maximumLimit).contains(limit) {
            throw RecentItemsError.invalidRequest
        }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<CoreRecentItemsPage, CoreFailure>, Never>) in
            core.read(limit: limit, cursor: cursor) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let page):
            return page
        case .failure(let failure):
            switch failure.code {
            case "read_not_authenticated": throw RecentItemsError.notAuthenticated
            case "read_relocked": throw RecentItemsError.relocked
            case "invalid_request": throw RecentItemsError.invalidRequest
            default: throw RecentItemsError.core(failure)
            }
        }
    }
}
