import UIKit
import XCTest

/// Walks every main screen of the app in light and dark and writes one PNG per screen into the directory named by
/// `WALKTHROUGH_OUTPUT_DIR` (passed as `TEST_RUNNER_WALKTHROUGH_OUTPUT_DIR`), so a reviewer can see what a change looks
/// like. Captures are synthetic text typed into the home's composer. Opening Search or Settings asks for the device
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
    /// Each settings screen in the order of the Settings rows: the row's label, then the title the opened screen shows
    /// (as its header text, or in its navigation bar).
    private let settingsScreens: [(label: String, title: String)] = [
        ("Daily prompt", "Daily prompt"),
        ("Unfinished recordings", "Unfinished recordings"),
        ("Processing", "Processing"),
        ("Privacy", "Privacy"),
        ("Audio", "Audio"),
        ("Export", "Export"),
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
        XCTAssertTrue(app.buttons[homeSearch].waitForExistence(timeout: screenTimeout), "home screen did not appear")
        waitForLaunchAuthentication()
        saveCaptures()
        dismissKeyboard()
        capture(prefix, "home")

        // Search, with a query.
        // The restyled Search has no title; its query field is what shows the screen is open.
        let queryField = app.textFields["retrieval.search.query"].firstMatch
        openFromHome(homeSearch, name: "Search", shows: queryField)
        tapWhenHittable(queryField, step: "Search", elementName: "the query field")
        queryField.typeText(searchQuery)
        // The keyboard's Search key is the one way a person submits; it also puts the keyboard away.
        let searchKey = app.keyboards.buttons["Search"]
        XCTAssertTrue(searchKey.waitForExistence(timeout: screenTimeout), "keyboard Search key did not appear")
        tapWhenHittable(searchKey, step: "Search", elementName: "the keyboard Search key")
        // The saved captures must be found: "garage" matches at least one (the second appearance's run adds a second copy).
        // The count ("1 result", "2 results") is shown only when there are results; no match shows "No matches".
        let resultCount = app.staticTexts["retrieval.search.count"]
        XCTAssertTrue(resultCount.waitForExistence(timeout: screenTimeout), "search for \(searchQuery) found no synthetic capture")
        capture(prefix, "search")

        // Source detail for the first result: pushed above Search at compact width, beside the results at regular width.
        // The same width rule as the screenshot names: an iPad runs Search at regular width, with the item pane.
        let isRegularWidth = UIDevice.current.userInterfaceIdiom == .pad
        let firstResult = app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "retrieval.search.row.")).firstMatch
        XCTAssertTrue(firstResult.waitForExistence(timeout: screenTimeout), "no search result to open")
        tapWhenHittable(firstResult, step: "source detail", elementName: "the first search result")
        let detailText = app.descendants(matching: .any)["source-detail.current-text"]
        XCTAssertTrue(detailText.waitForExistence(timeout: screenTimeout), "source detail did not open")
        capture(prefix, "source detail")
        if !isRegularWidth {
            goBack(to: resultCount, "Search")
        }
        goBack(to: homeLink(homeSearch), "home")

        // Settings and each settings screen. The home focuses its composer again when it comes back, and its header shows
        // Done in place of Settings while the keyboard is up, so the keyboard goes away first.
        dismissKeyboard()
        openFromHome(homeSettings, name: "Settings", shows: settingsHeader)
        capture(prefix, "settings")
        for screen in settingsScreens {
            // A row reads its label, then its current value when one is known ("Processing, On this phone").
            let row = app.buttons.matching(
                NSPredicate(format: "label == %@ OR label BEGINSWITH %@", screen.label, "\(screen.label), ")
            ).firstMatch
            XCTAssertTrue(row.waitForExistence(timeout: screenTimeout), "settings row \(screen.label) not found")
            tapWhenHittable(row, step: "settings row \(screen.label)", elementName: "the \(screen.label) row")
            // The row query is re-resolved on every check and can match a control on the opened screen (Export's own
            // "Export" button), so the screen counts as open once the Settings header has gone.
            XCTAssertTrue(
                waitForScreen(screen.title, leaving: settingsHeader, timeout: screenTimeout), "\(screen.title) did not open")
            capture(prefix, screen.label)
            if screen.title == "Processing" {
                // The two screens one level down hold the dense controls, so each is captured too.
                for (identifier, title, slug) in [
                    ("provider-settings.add-provider", "Add provider", "processing-add-provider"),
                    ("provider-settings.advanced", "Advanced", "processing-advanced"),
                ] {
                    let link = app.buttons[identifier]
                    XCTAssertTrue(link.waitForExistence(timeout: screenTimeout), "\(identifier) not found on Processing")
                    tapWhenHittable(link, step: title, elementName: "the \(identifier) link")
                    XCTAssertTrue(
                        waitForScreen(title, leaving: link, timeout: screenTimeout), "\(title) did not open from Processing")
                    capture(prefix, slug)
                    goBack(to: app.staticTexts[screen.title], screen.title)
                }
            }
            goBack(to: settingsHeader, "Settings")
        }
        // The "Erase everything" card on the Settings root must open the reset screen, which is captured. Nothing is confirmed.
        let eraseCard = app.descendants(matching: .any)["management.settings.erase"].firstMatch
        XCTAssertTrue(eraseCard.waitForExistence(timeout: screenTimeout), "Erase everything card not found on Settings")
        if !eraseCard.isHittable { app.swipeUp() }
        tapWhenHittable(eraseCard, step: "erase everything", elementName: "the Erase everything card")
        let resetExplanation = app.descendants(matching: .any)["data-settings.reset.explanation"].firstMatch
        XCTAssertTrue(resetExplanation.waitForExistence(timeout: screenTimeout), "Erase everything did not open the reset screen")
        capture(prefix, "erase everything")
        goBack(to: settingsHeader, "Settings")
        goBack(to: homeLink(homeSearch), "home")
    }

    /// The home's glass header: Search is always there, so it is what shows the home is back; Settings becomes Done while
    /// the keyboard is up.
    private let homeSearch = "home.search"
    private let homeSettings = "home.settings"
    private let homeDone = "home.done"

    private var settingsHeader: XCUIElement { app.staticTexts["Settings"] }

    private func homeLink(_ identifier: String) -> XCUIElement { app.buttons[identifier] }

    /// True once the screen titled `title` is showing: the restyled settings screens draw their title as header text
    /// and hide the navigation bar, the others show it in the navigation bar. `leaving` is the control that opened the
    /// screen; it must be gone, so a row or link with the same text as the title does not count as the screen.
    private func waitForScreen(_ title: String, leaving origin: XCUIElement, timeout: TimeInterval) -> Bool {
        waitForScreen(shownBy: [app.navigationBars[title], app.staticTexts[title]], leaving: origin, timeout: timeout)
    }

    /// True once any of `markers` exists and `origin` has gone.
    private func waitForScreen(shownBy markers: [XCUIElement], leaving origin: XCUIElement, timeout: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        repeat {
            if markers.contains(where: { $0.exists }), !isShowing(origin) { return true }
            Thread.sleep(forTimeInterval: 0.5)
        } while Date() < deadline
        return false
    }

    /// True when `element` exists and its frame is on the screen. Unlike `isHittable`, it never asks for an activation
    /// point, which XCTest fails the test over for a static text mid-transition ("Activation point invalid").
    private func isShowing(_ element: XCUIElement) -> Bool {
        guard element.exists else { return false }
        let frame = element.frame
        return !frame.isEmpty && frame.intersects(app.frame)
    }

    /// Waits until `element` exists and is hittable, then taps it. A step that cannot act fails naming the step and the
    /// element. The timeout is `screenTimeout` like every other wait here: a hosted macOS simulator can take a minute to
    /// finish a transition under load.
    private func tapWhenHittable(_ element: XCUIElement, step: String, elementName: String) {
        waitUntilHittable(element, step: step, elementName: elementName)
        element.tap()
    }

    private func waitUntilHittable(_ element: XCUIElement, step: String, elementName: String) {
        let deadline = Date().addingTimeInterval(screenTimeout)
        while Date() < deadline {
            if element.exists, element.isHittable { return }
            Thread.sleep(forTimeInterval: 0.25)
        }
        XCTFail("step \"\(step)\": \(elementName) did not exist and become hittable within \(Int(screenTimeout)) s")
    }

    /// The app makes one quiet Face ID attempt at launch. Its system prompt takes focus, so typing while it is up loses
    /// text, and the runner's alert handler can race the matching face and fail the test. Wait until the driving script's
    /// matching face has answered it, as a person looks at the phone before typing.
    private func waitForLaunchAuthentication() {
        let promptIdentifier = "com.apple.localauthentication.ax.authentication.alert"
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let prompts = [springboard.alerts[promptIdentifier], app.alerts[promptIdentifier]]
        let deadline = Date().addingTimeInterval(15)
        var shown = false
        repeat {
            shown = prompts.contains { $0.exists }
            if !shown { Thread.sleep(forTimeInterval: 0.5) }
        } while !shown && Date() < deadline
        guard shown else { return }
        let answered = Date().addingTimeInterval(screenTimeout)
        while prompts.contains(where: { $0.exists }) && Date() < answered {
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTAssertFalse(prompts.contains { $0.exists }, "the launch Face ID prompt was not answered")
    }

    private func saveCaptures() {
        let field = composerField
        if !field.waitForExistence(timeout: screenTimeout) {
            // What the home exposed instead, so a wrong identifier or element kind can be read off the evidence.
            try? app.debugDescription.write(
                to: outputDirectory.appendingPathComponent("failure-home-hierarchy.txt"), atomically: true, encoding: .utf8)
            XCTFail("composer field did not appear")
        }
        let save = app.buttons["home.composer.save"]
        // The composer shows Save only while there is text, and enables it once that text has content and no earlier
        // save is still in flight.
        let saveIsReady = NSPredicate(format: "exists == true AND isEnabled == true")
        for text in syntheticCaptures {
            // The system can take focus (the launch Face ID check, keyboard tips) and drop typed text; a person types again.
            var typed = false
            for attempt in 1...3 where !typed {
                tapWhenHittable(field, step: "type capture", elementName: "the composer field")
                // Typing before the keyboard is up makes the UI-test runner fail while it looks for interrupting alerts.
                XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: screenTimeout), "keyboard did not appear")
                field.typeText(text)
                let enabled = XCTNSPredicateExpectation(predicate: saveIsReady, object: save)
                typed = XCTWaiter().wait(for: [enabled], timeout: 8) == .completed
                if !typed {
                    try? XCUIScreen.main.screenshot().pngRepresentation.write(
                        to: outputDirectory.appendingPathComponent("failure-capture-attempt\(attempt).png"))
                }
            }
            XCTAssertTrue(typed, "Save did not appear enabled after typing a capture")
            tapWhenHittable(save, step: "save capture", elementName: "the Save button")
            // A saved capture empties the field and keeps the keyboard up, so the composer is back at rest, which has no
            // Save. Typing before then would go into the text still being saved.
            let saveGone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: save)
            XCTAssertEqual(
                XCTWaiter().wait(for: [saveGone], timeout: 30), .completed,
                "step \"save capture\": the Save button was still showing 30 s after it was tapped")
        }
    }

    /// The composer's text field. It grows with its text, and a growing SwiftUI field is exposed as a text view rather
    /// than a text field, so it is found by its identifier alone.
    private var composerField: XCUIElement {
        app.descendants(matching: .any).matching(identifier: "home.composer.field").firstMatch
    }

    /// Puts the keyboard away with the header's Done button (after the slide-to-type tip, if the system shows it) so the
    /// home screenshot shows the app. The composer takes focus whenever the home appears, including on the way back from
    /// Search or Settings, so the keyboard is given a moment to come up before it is looked for. The walkthrough fails if
    /// the keyboard cannot be dismissed, rather than saving a covered home screen.
    private func dismissKeyboard() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where button.waitForExistence(timeout: 2) {
            button.tap()
        }
        if app.keyboards.firstMatch.waitForExistence(timeout: 5) {
            let done = app.buttons[homeDone]
            XCTAssertTrue(done.waitForExistence(timeout: screenTimeout), "Done did not replace Settings while the keyboard is up")
            done.tap()
        }
        let gone = NSPredicate(format: "count == 0")
        expectation(for: gone, evaluatedWith: app.keyboards)
        waitForExpectations(timeout: 10)
    }

    /// Taps a home link and waits for the destination. The first open per launch shows the system Face ID prompt,
    /// which the driving script answers with a matching face.
    private func openFromHome(_ identifier: String, name: String, shows marker: XCUIElement) {
        // A failed or cancelled Face ID attempt leaves the user on home with a notice; asking again is what a person does.
        for attempt in 1...3 {
            let link = app.buttons[identifier]
            XCTAssertTrue(link.waitForExistence(timeout: screenTimeout), "\(identifier) not on home")
            if link.isEnabled { link.tap() }
            if waitForScreen(shownBy: [marker], leaving: link, timeout: 40) { return }
            let evidence = XCUIScreen.main.screenshot()
            try? evidence.pngRepresentation.write(
                to: outputDirectory.appendingPathComponent("failure-\(name.lowercased())-attempt\(attempt).png"))
        }
        XCTFail("\(name) did not open (authentication was not matched or the screen is unreachable)")
    }

    /// Goes back one screen and waits for `destination` to show again. The restyled settings screens have a glass
    /// "Back" button in place of a navigation bar; the others go back from the navigation bar.
    private func goBack(to destination: XCUIElement, _ destinationName: String) {
        let glassBack = app.buttons["Back"].firstMatch
        let barBack = app.navigationBars.buttons.element(boundBy: 0)
        let deadline = Date().addingTimeInterval(10)
        var tapped = false
        repeat {
            if glassBack.exists, glassBack.isHittable {
                glassBack.tap()
                tapped = true
            } else if barBack.exists, barBack.isHittable {
                barBack.tap()
                tapped = true
            } else {
                Thread.sleep(forTimeInterval: 0.5)
            }
        } while !tapped && Date() < deadline
        XCTAssertTrue(tapped, "no back button")
        XCTAssertTrue(destination.waitForExistence(timeout: screenTimeout), "going back did not return to \(destinationName)")
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
