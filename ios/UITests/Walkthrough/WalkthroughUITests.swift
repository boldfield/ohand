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

    func testWalkthrough() throws {
        let environment = ProcessInfo.processInfo.environment
        let appearanceName = try XCTUnwrap(
            environment["WALKTHROUGH_APPEARANCE"], "WALKTHROUGH_APPEARANCE (light or dark) is not set")
        XCTAssertTrue(["light", "dark"].contains(appearanceName), "unknown appearance \(appearanceName)")
        expectsDark = appearanceName == "dark"
        let widthName = UIDevice.current.userInterfaceIdiom == .pad ? "regular" : "compact"
        app.launch()
        try walk(prefix: "\(widthName)-\(appearanceName)")
    }

    /// True when the driving script put the simulator in dark appearance. The screenshots are checked against it, so a
    /// run in which the appearance did not take effect fails instead of producing mislabelled pictures.
    private var expectsDark = false

    private func assertAppearanceApplied(_ screenshot: XCUIScreenshot) {
        guard let image = screenshot.image.cgImage, let data = image.dataProvider?.data,
            let bytes = CFDataGetBytePtr(data)
        else { return XCTFail("cannot read the screenshot pixels") }
        // A point in the empty lower part of the screen, which shows the screen's own background.
        let bytesPerPixel = image.bitsPerPixel / 8
        let pixelOffset = (image.height * 3 / 4) * image.bytesPerRow + 8 * bytesPerPixel
        let luminance = (Int(bytes[pixelOffset]) + Int(bytes[pixelOffset + 1]) + Int(bytes[pixelOffset + 2])) / 3
        if expectsDark {
            XCTAssertLessThan(luminance, 100, "dark appearance was not applied")
        } else {
            XCTAssertGreaterThan(luminance, 150, "light appearance was not applied")
        }
    }

    private func walk(prefix: String) throws {
        XCTAssertTrue(app.buttons["management.open.search"].waitForExistence(timeout: screenTimeout), "home screen did not appear")
        saveCaptures()
        dismissKeyboard()
        capture(prefix, "home")

        // Search, with a query.
        openFromHome("management.open.search", title: "Search")
        let queryField = app.textFields["retrieval.search.query"].firstMatch
        XCTAssertTrue(queryField.waitForExistence(timeout: screenTimeout), "search query field did not appear")
        queryField.tap()
        queryField.typeText(searchQuery)
        // The keyboard's Search key is the one way a person submits; it also puts the keyboard away.
        let searchKey = app.keyboards.buttons["Search"]
        XCTAssertTrue(searchKey.waitForExistence(timeout: screenTimeout), "keyboard Search key did not appear")
        searchKey.tap()
        // The saved captures must be found: "garage" matches at least one (the second appearance's run adds a second copy).
        let resultCount = app.staticTexts.matching(
            NSPredicate(format: "label BEGINSWITH 'Showing ' AND NOT label BEGINSWITH 'Showing 0 '")
        ).firstMatch
        XCTAssertTrue(resultCount.waitForExistence(timeout: screenTimeout), "search for \(searchQuery) found no synthetic capture")
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
                XCTAssertTrue(exportLink.waitForExistence(timeout: screenTimeout), "Export link not found")
                exportLink.tap()
                XCTAssertTrue(app.navigationBars["Export"].waitForExistence(timeout: screenTimeout), "Export did not open")
                capture(prefix, "Export")
                goBack()
            }
            goBack()
        }
        goBack()
    }

    private func saveCaptures() {
        let field = app.textViews["capture-entry.field"]
        XCTAssertTrue(field.waitForExistence(timeout: screenTimeout), "capture field did not appear")
        for text in syntheticCaptures {
            // The field is disabled while the previous capture is being saved; typing then would be lost.
            expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: field)
            waitForExpectations(timeout: 30)
            let save = app.buttons["capture-entry.save"]
            XCTAssertTrue(save.waitForExistence(timeout: screenTimeout), "Save button did not appear")
            // The system can take focus (the launch Face ID check, keyboard tips) and drop typed text; a person types again.
            var typed = false
            for attempt in 1...3 where !typed {
                field.tap()
                // Typing before the keyboard is up makes the UI-test runner fail while it looks for interrupting alerts.
                XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: screenTimeout), "keyboard did not appear")
                field.typeText(text)
                let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isEnabled == true"), object: save)
                typed = XCTWaiter().wait(for: [enabled], timeout: 8) == .completed
                if !typed {
                    try? XCUIScreen.main.screenshot().pngRepresentation.write(
                        to: outputDirectory.appendingPathComponent("failure-capture-attempt\(attempt).png"))
                }
            }
            XCTAssertTrue(typed, "Save stayed disabled after typing a capture")
            save.tap()
            // A saved capture empties the field, which disables Save again.
            expectation(for: NSPredicate(format: "isEnabled == false"), evaluatedWith: save)
            waitForExpectations(timeout: 30)
        }
    }

    /// Puts the keyboard away by showing the Voice capture mode (after the slide-to-type tip, if the system shows it) so the home screenshot shows the
    /// app. The walkthrough fails if the keyboard cannot be dismissed, rather than saving a covered home screen.
    private func dismissKeyboard() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where button.waitForExistence(timeout: 2) {
            button.tap()
        }
        // The silent capture field takes focus whenever it appears and the screen has no control that puts the keyboard
        // away, so a person gets the whole home screen by choosing Voice capture, which has no text field.
        if app.keyboards.count > 0 {
            let voice = app.buttons["Voice"]
            XCTAssertTrue(voice.waitForExistence(timeout: screenTimeout), "capture mode picker did not appear")
            voice.tap()
        }
        let gone = NSPredicate(format: "count == 0")
        expectation(for: gone, evaluatedWith: app.keyboards)
        waitForExpectations(timeout: 10)
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
        if screen == "settings" { assertAppearanceApplied(screenshot) }
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
