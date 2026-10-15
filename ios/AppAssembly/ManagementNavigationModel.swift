import Combine
import Foundation
import OhAndServices

/// Navigation from the capture surface to the management screens. Capture is the root and never waits for this.
///
/// Rules this type keeps:
/// * Every management screen reads stored data, so opening one needs an authenticated read session. A locked session
///   asks the device owner first, and only a success opens the screen. Cancelling or failing leaves the user on capture.
/// * Leaving the authenticated session (the root relocks when the app goes to the background) returns to capture,
///   so no management screen stays on display after the session ended.
/// * Nothing here carries capture text or stored content; the path holds entry identifiers only.
///
/// Confined to the main queue.
final class ManagementNavigationModel: ObservableObject {
    static let authenticationReason = "Open your saved items and settings."

    @Published var path: [String] = []
    @Published private(set) var notice: String?
    @Published private(set) var isAuthenticating = false
    /// The item a reminder notification tap resolved to, waiting for an item screen to present it. The root reports a
    /// tap only while its session is authenticated, and this is dropped as soon as reads stop being authenticated.
    @Published private(set) var requestedItemID: String?

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

    func open(entryID: String) {
        notice = nil
        if readScope() == .authenticated {
            path = [entryID]
            return
        }
        guard !isAuthenticating else { return }
        isAuthenticating = true
        authenticate(Self.authenticationReason) { [weak self] outcome in
            guard let self = self else { return }
            self.isAuthenticating = false
            self.readAccessDidChange()
            switch outcome {
            case .authenticated:
                if self.readScope() == .authenticated {
                    self.path = [entryID]
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

    /// Records the item a notification tap resolved to. No item screen exists in the shell yet, so this changes
    /// nothing on display; the screen that presents items takes the identifier from `requestedItemID`.
    func openItem(itemID: String) {
        requestedItemID = itemID
    }

    /// Call when the app returns to the foreground or the session may have changed. Leaves every management screen
    /// once reads are no longer authenticated.
    func revalidate() {
        if readScope() != .authenticated {
            if !path.isEmpty { path = [] }
            requestedItemID = nil
        }
        readAccessDidChange()
    }

    func returnToCapture() {
        path = []
    }

    func dismissNotice() {
        notice = nil
    }
}
