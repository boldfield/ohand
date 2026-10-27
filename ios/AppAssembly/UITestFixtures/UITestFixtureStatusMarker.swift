#if DEBUG
import UIKit

/// A near-invisible accessibility element that tells a UI test how far the fixture is: `seeding`, `ready`, or
/// `failed: <reason>`. A test waits for `ready` before it acts, and a broken fixture shows up as `failed` instead of a
/// half-filled store.
@MainActor
final class UITestFixtureStatusMarker {
    static let identifier = "ui-fixture.status"

    private let label = UILabel(frame: CGRect(x: 0, y: 0, width: 4, height: 4))

    init() {
        label.isAccessibilityElement = true
        label.accessibilityIdentifier = Self.identifier
        label.isUserInteractionEnabled = false
        label.alpha = 0.05
    }

    func show(_ status: String) {
        label.accessibilityLabel = status
        attachWhenAWindowExists(attemptsLeft: 600)
    }

    private func attachWhenAWindowExists(attemptsLeft: Int) {
        guard label.superview == nil else { return }
        let window = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first
        if let window {
            window.addSubview(label)
        } else if attemptsLeft > 0 {
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 100_000_000)
                attachWhenAWindowExists(attemptsLeft: attemptsLeft - 1)
            }
        }
    }
}
#endif
