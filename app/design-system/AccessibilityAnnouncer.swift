import SwiftUI
import UIKit

/// Posts a VoiceOver announcement for a change the person would otherwise have to look for: a lock, a failure, a
/// reminder that needs a time. In a debug build launched with `-OhAndAnnouncementLog`, each announcement is also written
/// to a near-invisible accessibility element, because a UI test cannot read the announcement queue but can read an element.
enum AccessibilityAnnouncer {
    /// The launch argument that turns the debug log element on.
    static let logLaunchArgument = "-OhAndAnnouncementLog"
    static let logIdentifier = "accessibility.announcement-log"

    static func announce(_ message: String) {
        UIAccessibility.post(notification: .announcement, argument: message)
        #if DEBUG
        Task { @MainActor in AnnouncementLogElement.shared.record(message) }
        #endif
    }
}

#if DEBUG
/// The announcements made since launch, joined with " | ", as the label of a 4-point element on the key window.
@MainActor
private final class AnnouncementLogElement {
    static let shared = AnnouncementLogElement()

    private let label = UILabel(frame: CGRect(x: 0, y: 0, width: 4, height: 4))
    private var announcements: [String] = []
    private var isEnabled: Bool { ProcessInfo.processInfo.arguments.contains(AccessibilityAnnouncer.logLaunchArgument) }

    private init() {
        label.isAccessibilityElement = true
        label.accessibilityIdentifier = AccessibilityAnnouncer.logIdentifier
        label.isUserInteractionEnabled = false
        label.alpha = 0.05
    }

    func record(_ message: String) {
        guard isEnabled else { return }
        announcements.append(message)
        label.accessibilityLabel = announcements.joined(separator: " | ")
        attachWhenAWindowExists(attemptsLeft: 100)
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
