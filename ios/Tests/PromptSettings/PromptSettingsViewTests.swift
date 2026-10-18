import SwiftUI
import UIKit
import XCTest
@testable import OhAnd

/// Hosts the daily prompt screen's info sheet and reads what it carries.
@MainActor
final class PromptSettingsViewTests: XCTestCase {
    func testTheInfoSheetCarriesEveryParagraphTheScreenShowedBefore() async {
        var paragraphs: [String] = []
        let sheet = SettingsInfoSheet(
            title: "Daily prompt", paragraphs: PromptSettingsView.infoParagraphs, renderReporter: { paragraphs = $0 })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = UIHostingController(rootView: sheet)
        window.makeKeyAndVisible()
        for _ in 0..<3 {
            window.rootViewController?.view.layoutIfNeeded()
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        window.isHidden = true
        XCTAssertEqual(
            paragraphs,
            [PromptSettingsView.introText, PromptSettingsView.horizonText, PromptSettingsView.privacyText,
             PromptSettingsView.remindersText])
    }
}
