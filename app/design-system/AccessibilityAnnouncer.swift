import SwiftUI
import UIKit

/// Something that can speak a change to the person: the system in the app, a recorder in a hosted test.
protocol AccessibilityAnnouncing {
    func announce(_ message: String)
}

/// Posts a VoiceOver announcement through the system.
struct SystemAccessibilityAnnouncer: AccessibilityAnnouncing {
    func announce(_ message: String) {
        UIAccessibility.post(notification: .announcement, argument: message)
    }
}

/// Posts a VoiceOver announcement for a change the person would otherwise have to look for: a saved capture, a lock, a
/// failure, a reminder that needs a time. Views call `announce`; a hosted test replaces `current` with a recorder and
/// restores it afterwards. In a debug build launched with `-OhAndAnnouncementLog`, each announcement is also written
/// to a near-invisible accessibility element, because a UI test cannot read the announcement queue but can read an element.
enum AccessibilityAnnouncer {
    /// The launch argument that turns the debug log element on.
    static let logLaunchArgument = "-OhAndAnnouncementLog"
    static let logIdentifier = "accessibility.announcement-log"

    /// Where announcements go. Replaced only by tests, on the main thread.
    static var current: AccessibilityAnnouncing = SystemAccessibilityAnnouncer()

    static func announce(_ message: String) {
        current.announce(message)
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
