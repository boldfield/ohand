import Foundation
import OhAndCoreBridge

/// Why an item's detail was not returned. None of the cases carries an identifier or stored content.
public enum ItemDetailError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The session relocked after the read started; the core's answer was discarded undelivered.
    case relocked
    /// The identifier was refused before it reached the core (empty or too long).
    case invalidItemID
    /// No readable item has that identifier. An unknown, a deleted and an out-of-scope item are not told apart.
    case notFound
    case core(CoreFailure)
}

/// Reads one item's full detail through the private read gate: current text and where it comes from, the original
/// capture text, the user's correction, type, scope, session topic, lifecycle state, capture time, audio reference,
/// transcript provenance and a bounded newest-first history.
///
/// * **Authenticated.** A capture-only session gets `notAuthenticated` and the core is not asked.
/// * **Relock-safe.** An answer that arrives after the session relocked is discarded (`relocked`) without being decoded.
/// * **Opaque IDs.** The item ID is only checked for being non-empty and within the core's length bound; nothing is
///   parsed out of it.
public final class ItemDetailService: @unchecked Sendable {
    /// The longest item ID the core accepts, in UTF-8 bytes.
    public static let maximumItemIDBytes = 512

    private let core: ItemDetailCore

    init(core: ItemDetailCore) {
        self.core = core
    }

    public func detail(itemID: String) async throws -> CoreItemDetail {
        guard !itemID.isEmpty, itemID.utf8.count <= Self.maximumItemIDBytes else {
            throw ItemDetailError.invalidItemID
        }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<CoreItemDetail, CoreFailure>, Never>) in
            core.read(itemID: itemID) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let detail):
            return detail
        case .failure(let failure):
            switch failure.code {
            case "read_not_authenticated": throw ItemDetailError.notAuthenticated
            case "read_relocked": throw ItemDetailError.relocked
            case "not_found": throw ItemDetailError.notFound
            case "invalid_request": throw ItemDetailError.invalidItemID
            default: throw ItemDetailError.core(failure)
            }
        }
    }
}
