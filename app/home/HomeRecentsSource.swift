import Foundation
import OhAndCoreBridge
import OhAndServices

/// The production source: the root's read session for the scope and the quiet-attempt flag, and its recent-captures
/// service for pages, and its item-status read for whether a saved item's interpretation has finished. Holds no content
/// of its own.
final class RootHomeRecentsSource: HomeRecentsSource {
    private weak var root: ServiceCompositionRoot?
    private let unlockAction: () -> Void

    init(root: ServiceCompositionRoot, unlock: @escaping () -> Void) {
        self.root = root
        unlockAction = unlock
    }

    var isAuthenticated: Bool { root?.readScope == .authenticated }

    var isQuietAttemptInFlight: Bool { root?.isQuietReadAuthenticationInFlight ?? false }

    func readPage(cursor: String?) async throws -> CoreRecentItemsPage {
        guard let root else { throw RecentItemsError.notAuthenticated }
        return try await root.recentItems.page(cursor: cursor)
    }

    func interpretationIsFinished(itemID: String) async throws -> Bool {
        guard let root else { throw RecentItemsError.notAuthenticated }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ItemStatusReport, CoreFailure>, Never>) in
            root.itemStatus(itemID: itemID) { continuation.resume(returning: $0) }
        }
        guard let finished = Self.interpretationIsFinished(result) else {
            // A refused read means access ended under it; the model checks the scope and relocks.
            guard isAuthenticated else { throw RecentItemsError.notAuthenticated }
            return false
        }
        return finished
    }

    /// Whether the status says interpretation ended (processed, abstained or uninterpreted); nil when nothing was read.
    static func interpretationIsFinished(_ result: Result<ItemStatusReport, CoreFailure>) -> Bool? {
        guard case .status(let status) = RootCaptureProgressReader.answer(for: result) else { return nil }
        return status.progress.interpretation != .pending
    }

    func unlock() { unlockAction() }

    func observeAccess(_ handler: @escaping @Sendable () -> Void) -> () -> Void {
        root?.observeReadAccess(handler) ?? {}
    }
}
