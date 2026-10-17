import UIKit
import XCTest

/// Walks every main screen of the app in light and dark and writes one PNG per screen into the directory named by
/// `WALKTHROUGH_OUTPUT_DIR` (passed as `TEST_RUNNER_WALKTHROUGH_OUTPUT_DIR`), so a reviewer can see what a change looks
/// like. Captures are synthetic text typed through the capture screen. Opening Search or Settings asks for the device
/// owner; the driving script (`ios/scripts/walkthrough-simulator.sh`) enrols Face ID on the simulator and sends matching
/// faces, so the app's own authentication runs unmodified. A screen that cannot be reached fails the test.
final class WalkthroughUITests: XCTestCase {
    private let app = XCUIApplication()
    private var outputDirectory: URL!
    private var screenshotIndex = 0

    private let syntheticCaptures = [
        "Buy oat milk and a birthday card for Sam",
        "Idea: sort the garage shelves by season",
        "Remind me to book the dentist next month",
    ]
    private let searchQuery = "garage"
    private let settingsScreens: [(title: String, label: String)] = [
        ("Unfinished recordings", "Unfinished recordings"),
        ("Audio retention", "Audio retention"),
        ("Providers", "Providers"),
        ("Daily prompt", "Daily prompt"),
    ]
    private let screenTimeout: TimeInterval = 90

    override func setUpWithError() throws {
        continueAfterFailure = false
        let path = try XCTUnwrap(
            ProcessInfo.processInfo.environment["WALKTHROUGH_OUTPUT_DIR"], "WALKTHROUGH_OUTPUT_DIR is not set")
        outputDirectory = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
    }

    func testWalkthroughInLightAndDark() throws {
        let widthName = UIDevice.current.userInterfaceIdiom == .pad ? "regular" : "compact"
        for (appearance, appearanceName) in [(XCUIDevice.Appearance.light, "light"), (.dark, "dark")] {
            XCUIDevice.shared.appearance = appearance
            screenshotIndex = 0
            app.terminate()
            app.launchArguments = ["-AppleInterfaceStyle", appearanceName == "dark" ? "Dark" : "Light"]
            app.launch()
            try walk(prefix: "\(widthName)-\(appearanceName)")
        }
        XCUIDevice.shared.appearance = .light
    }

    private func walk(prefix: String) throws {
        XCTAssertTrue(app.buttons["management.open.search"].waitForExistence(timeout: screenTimeout), "home screen did not appear")
        saveCaptures()
        capture(prefix, "home")

        // Search, with a query.
        openFromHome("management.open.search", title: "Search")
        let queryField = app.textFields["retrieval.search.query"].firstMatch
        XCTAssertTrue(queryField.waitForExistence(timeout: screenTimeout), "search query field did not appear")
        queryField.tap()
        queryField.typeText(searchQuery)
        let submit = app.buttons["retrieval.search.submit"]
        if submit.exists { submit.tap() }
        // Put the keyboard away so the results are visible.
        let searchKey = app.keyboards.buttons["Search"]
        if searchKey.exists { searchKey.tap() }
        _ = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Showing'")).firstMatch.waitForExistence(timeout: 10)
        capture(prefix, "search")
        goBack()

        // Settings and each settings screen.
        openFromHome("management.open.settings", title: "Settings")
        capture(prefix, "settings")
        for screen in settingsScreens {
            let row = app.buttons[screen.label].firstMatch
            XCTAssertTrue(row.waitForExistence(timeout: screenTimeout), "settings row \(screen.label) not found")
            row.tap()
            XCTAssertTrue(
                app.navigationBars[screen.title].waitForExistence(timeout: screenTimeout), "\(screen.title) did not open")
            capture(prefix, screen.title)
            if screen.title == "Audio retention" {
                let exportLink = app.buttons["data-settings.retention.export-link"]
                if exportLink.waitForExistence(timeout: 10) {
                    exportLink.tap()
                    XCTAssertTrue(app.navigationBars["Export"].waitForExistence(timeout: screenTimeout), "Export did not open")
                    capture(prefix, "Export")
                    goBack()
                }
            }
            goBack()
        }
        goBack()
    }

    private func saveCaptures() {
        let field = app.textViews["capture-entry.field"]
        guard field.waitForExistence(timeout: screenTimeout) else { return }
        for text in syntheticCaptures {
            field.tap()
            field.typeText(text)
            let save = app.buttons["capture-entry.save"]
            if save.waitForExistence(timeout: 10), save.isEnabled { save.tap() }
        }
    }

    /// Taps a home link and waits for the destination. The first open per launch shows the system Face ID prompt,
    /// which the driving script answers with a matching face.
    private func openFromHome(_ identifier: String, title: String) {
        // A failed or cancelled Face ID attempt leaves the user on home with a notice; asking again is what a person does.
        for attempt in 1...3 {
            let link = app.buttons[identifier]
            XCTAssertTrue(link.waitForExistence(timeout: screenTimeout), "\(identifier) not on home")
            if link.isEnabled { link.tap() }
            if app.navigationBars[title].waitForExistence(timeout: 40) { return }
            let evidence = XCUIScreen.main.screenshot()
            try? evidence.pngRepresentation.write(
                to: outputDirectory.appendingPathComponent("failure-\(title.lowercased())-attempt\(attempt).png"))
        }
        XCTFail("\(title) did not open (authentication was not matched or the screen is unreachable)")
    }

    private func goBack() {
        let back = app.navigationBars.buttons.element(boundBy: 0)
        XCTAssertTrue(back.waitForExistence(timeout: 10), "no back button")
        back.tap()
    }

    private func capture(_ prefix: String, _ screen: String) {
        screenshotIndex += 1
        let slug = screen.lowercased().replacingOccurrences(of: " ", with: "-")
        let name = String(format: "%@-%02d-%@.png", prefix, screenshotIndex, slug)
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        do {
            try screenshot.pngRepresentation.write(to: outputDirectory.appendingPathComponent(name))
        } catch {
            XCTFail("could not write \(name): \(error)")
        }
    }
}
