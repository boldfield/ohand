import Foundation
import OhAndCoreBridge
import OhAndServices

/// The production source: the root's read session for the scope and the quiet-attempt flag, and its recent-captures
/// service for pages. Holds no content of its own.
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

    func unlock() { unlockAction() }

    func observeAccess(_ handler: @escaping @Sendable () -> Void) -> () -> Void {
        root?.observeReadAccess(handler) ?? {}
    }
}
