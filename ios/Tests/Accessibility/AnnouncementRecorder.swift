import XCTest
@testable import OhAnd

/// Collects what the app would have announced. `install` swaps it in for the system announcer; `uninstall` restores it.
final class AnnouncementRecorder: AccessibilityAnnouncing {
    private(set) var messages: [String] = []
    private var replaced: AccessibilityAnnouncing?

    func announce(_ message: String) { messages.append(message) }

    func install() {
        replaced = AccessibilityAnnouncer.current
        AccessibilityAnnouncer.current = self
    }

    func uninstall() {
        if let replaced { AccessibilityAnnouncer.current = replaced }
        replaced = nil
    }
}

final class AnnouncementRecorderTests: XCTestCase {
    func testAnnouncementsGoToTheInstalledAnnouncerUntilItIsRemoved() {
        let recorder = AnnouncementRecorder()
        recorder.install()
        AccessibilityAnnouncer.announce("Saved")
        recorder.uninstall()
        XCTAssertEqual(recorder.messages, ["Saved"])
        XCTAssertTrue(AccessibilityAnnouncer.current is SystemAccessibilityAnnouncer)
    }
}
