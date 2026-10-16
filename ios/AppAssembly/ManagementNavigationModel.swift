import Combine
import Foundation
import OhAndServices

/// One place the management host can show. The navigation path stores each route as a string, so a route survives as
/// plain identifiers: a catalog entry id, `item:<item id>`, or `suggestion`.
enum ManagementRoute: Equatable {
    case entry(String)
    case item(String)
    case suggestion

    private static let itemPrefix = "item:"
    private static let suggestionComponent = "suggestion"

    init(pathComponent: String) {
        if pathComponent == ManagementRoute.suggestionComponent {
            self = .suggestion
        } else if pathComponent.hasPrefix(ManagementRoute.itemPrefix) {
            self = .item(String(pathComponent.dropFirst(ManagementRoute.itemPrefix.count)))
        } else {
            self = .entry(pathComponent)
        }
    }

    var pathComponent: String {
        switch self {
        case .entry(let entryID): return entryID
        case .item(let itemID): return ManagementRoute.itemPrefix + itemID
        case .suggestion: return ManagementRoute.suggestionComponent
        }
    }
}

/// Navigation from the capture surface to the management screens. Capture is the root and never waits for this.
///
/// Rules this type keeps:
/// * Every management screen reads stored data, so opening one needs an authenticated read session. A locked session
///   asks the device owner first, and only a success opens the screen. Cancelling or failing leaves the user on capture.
/// * Leaving the authenticated session (the root relocks when the app goes to the background) returns to capture,
///   so no management screen stays on display after the session ended.
/// * An item or suggestion request (a notification tap, another screen) follows the same rule: it presents only after
///   the session is authenticated, waits for the owner's answer when locked, and is dropped if the owner does not
///   answer successfully, so a tap before authentication is never lost and never shows stored content early.
/// * A capture entry always lands on capture, and drops any request still waiting for authentication.
/// * Nothing here carries capture text or stored content; the path holds entry, item and suggestion identifiers only.
///
/// Confined to the main queue.
final class ManagementNavigationModel: ObservableObject {
    static let authenticationReason = "Open your saved items and settings."

    @Published var path: [String] = []
    @Published private(set) var notice: String?
    @Published private(set) var isAuthenticating = false
    private var pendingRoute: ManagementRoute?

    private let readScope: () -> CompositionReadScope
    private let authenticate: (_ reason: String, _ completion: @escaping (CompositionAuthenticationOutcome) -> Void) -> Void
    private let readAccessDidChange: () -> Void

    init(
        readScope: @escaping () -> CompositionReadScope,
        authenticate: @escaping (_ reason: String, _ completion: @escaping (CompositionAuthenticationOutcome) -> Void) -> Void,
        readAccessDidChange: @escaping () -> Void = {}
    ) {
        self.readScope = readScope
        self.authenticate = authenticate
        self.readAccessDidChange = readAccessDidChange
    }

    /// The item whose screen is on display (or a placeholder for it), if any. Empty once the screen is left or reads stop
    /// being authenticated.
    var requestedItemID: String? {
        guard case .item(let itemID)? = displayedRoute else { return nil }
        return itemID
    }

    /// True while the suggestion screen (or a placeholder for it) is on display.
    var isSuggestionRequested: Bool {
        displayedRoute == ManagementRoute.suggestion
    }

    private var displayedRoute: ManagementRoute? {
        path.last.map { ManagementRoute(pathComponent: $0) }
    }

    func open(entryID: String) {
        present(.entry(entryID))
    }

    /// Presents the screen for an item a notification tap or another screen resolved to. A locked session asks the
    /// device owner first; the host shows the registered item destination, or a placeholder when none is registered.
    func openItem(itemID: String) {
        present(.item(itemID))
    }

    /// Opens an item's screen on top of the screens already open, so Back returns to the screen the user chose it from
    /// (the search results, for one). Under the same authentication rule as `openItem`; when reads are not authenticated
    /// nothing stays open to return to, so it behaves like `openItem` and asks the device owner first.
    func pushItem(itemID: String) {
        present(.item(itemID), keepingOpenScreens: true)
    }

    /// Presents the current suggestion, under the same authentication rule as an item.
    func openSuggestion() {
        present(.suggestion)
    }

    private func present(_ route: ManagementRoute, keepingOpenScreens: Bool = false) {
        notice = nil
        if readScope() == .authenticated {
            pendingRoute = nil
            path = keepingOpenScreens ? path + [route.pathComponent] : [route.pathComponent]
            return
        }
        pendingRoute = route
        guard !isAuthenticating else { return }
        isAuthenticating = true
        authenticate(Self.authenticationReason) { [weak self] outcome in
            guard let self = self else { return }
            self.isAuthenticating = false
            let awaitedRoute = self.pendingRoute
            self.pendingRoute = nil
            self.readAccessDidChange()
            switch outcome {
            case .authenticated:
                if let awaitedRoute = awaitedRoute, self.readScope() == .authenticated {
                    self.path = [awaitedRoute.pathComponent]
                }
            case .cancelled, .interrupted:
                break
            case .denied:
                self.notice = "Oh And could not confirm it is you, so your saved items stay locked."
            case .unavailable:
                self.notice = "This device has no passcode or biometric set up, so your saved items stay locked."
            }
        }
    }

    /// Call when the app returns to the foreground or the session may have changed. Leaves every management screen
    /// once reads are no longer authenticated.
    func revalidate() {
        if readScope() != .authenticated {
            if !path.isEmpty { path = [] }
        }
        readAccessDidChange()
    }

    func returnToCapture() {
        pendingRoute = nil
        path = []
    }

    func dismissNotice() {
        notice = nil
    }
}
