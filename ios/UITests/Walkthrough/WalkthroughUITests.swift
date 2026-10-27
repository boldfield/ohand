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

    /// Status with one failed note, opened through F45's `failed-sorting` fixture (unlocked, one rejected note). The row
    /// assertion is what makes the picture evidence: a Status screen without the failed note would fail here.
    func testWalkthroughStatusWithOneFailedNote() throws {
        let environment = ProcessInfo.processInfo.environment
        let appearanceName = try XCTUnwrap(
            environment["WALKTHROUGH_APPEARANCE"], "WALKTHROUGH_APPEARANCE (light or dark) is not set")
        XCTAssertTrue(["light", "dark"].contains(appearanceName), "unknown appearance \(appearanceName)")
        expectsDark = appearanceName == "dark"
        let widthName = UIDevice.current.userInterfaceIdiom == .pad ? "regular" : "compact"
        // The fixture app must not be running when the full walkthrough launches the real app next.
        addTeardownBlock { [app] in app.terminate() }
        XCTAssertTrue(app.launch(fixture: .failedSorting), "the failed-sorting fixture did not reach home")
        XCTAssertTrue(app.waitForFixtureReady(), "the failed-sorting fixture did not finish seeding")
        dismissKeyboard()
        openFromHome(homeSettings, name: "Settings", shows: settingsHeader)
        let row = app.buttons.matching(
            NSPredicate(format: "label == %@ OR label BEGINSWITH %@", "Status", "Status, ")
        ).firstMatch
        XCTAssertTrue(appears(row, timeout: screenTimeout), "settings row Status not found")
        tapWhenHittable(row, step: "settings row Status", elementName: "the Status row")
        let failedNote = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "status.failed-note.")).firstMatch
        XCTAssertTrue(appears(failedNote, timeout: screenTimeout), "Status shows no failed note")
        capture("\(widthName)-\(appearanceName)", "status failed note")
        // One row can expose several accessibility elements under its identifier; count the distinct identifiers.
        let failedNoteIdentifiers = Set(
            app.descendants(matching: .any).matching(
                NSPredicate(format: "identifier BEGINSWITH %@", "status.failed-note.")
            ).allElementsBoundByIndex.map(\.identifier))
        let failedNoteLabels = app.descendants(matching: .any).matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "status.failed-note.")
        ).allElementsBoundByIndex.map { "\($0.identifier)=\($0.label)" }
        // The failed-sorting fixture (F45, outside this task's paths) can seed its one synthetic note twice, so a count of
        // exactly one is not asserted here; the labels are reported so a change in that is visible.
        XCTAssertFalse(failedNoteIdentifiers.isEmpty, "Status shows a failed note: \(failedNoteLabels)")
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
        XCTAssertTrue(appears(app.buttons[homeSearch], timeout: screenTimeout), "home screen did not appear")
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
        XCTAssertTrue(appears(searchKey, timeout: screenTimeout), "keyboard Search key did not appear")
        tapWhenHittable(searchKey, step: "Search", elementName: "the keyboard Search key")
        // The saved captures must be found: "garage" matches at least one (the second appearance's run adds a second copy).
        // The count ("1 result", "2 results") is shown only when there are results; no match shows "No matches".
        let resultCount = app.staticTexts["retrieval.search.count"]
        XCTAssertTrue(appears(resultCount, timeout: screenTimeout), "search for \(searchQuery) found no synthetic capture")
        capture(prefix, "search")

        // Source detail for the first result: pushed above Search at compact width, beside the results at regular width.
        // The same width rule as the screenshot names: an iPad runs Search at regular width, with the item pane.
        let isRegularWidth = UIDevice.current.userInterfaceIdiom == .pad
        let firstResult = app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH %@", "retrieval.search.row.")).firstMatch
        XCTAssertTrue(appears(firstResult, timeout: screenTimeout), "no search result to open")
        tapWhenHittable(firstResult, step: "source detail", elementName: "the first search result")
        let detailText = app.descendants(matching: .any)["source-detail.current-text"]
        XCTAssertTrue(appears(detailText, timeout: screenTimeout), "source detail did not open")
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
            XCTAssertTrue(appears(row, timeout: screenTimeout), "settings row \(screen.label) not found")
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
                    XCTAssertTrue(appears(link, timeout: screenTimeout), "\(identifier) not found on Processing")
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
        XCTAssertTrue(appears(eraseCard, timeout: screenTimeout), "Erase everything card not found on Settings")
        if !isHittableNow(within: 3, eraseCard) { app.swipeUp() }
        tapWhenHittable(eraseCard, step: "erase everything", elementName: "the Erase everything card")
        let resetExplanation = app.descendants(matching: .any)["data-settings.reset.explanation"].firstMatch
        XCTAssertTrue(appears(resetExplanation, timeout: screenTimeout), "Erase everything did not open the reset screen")
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
        let opened = waitForScreen(shownBy: [app.navigationBars[title].firstMatch, app.staticTexts[title].firstMatch], leaving: origin, timeout: timeout)
        if !opened {
            // What was on screen instead, so the evidence says whether the screen never opened or its marker was not found.
            let slug = title.lowercased().replacingOccurrences(of: " ", with: "-")
            try? XCUIScreen.main.screenshot().pngRepresentation.write(
                to: outputDirectory.appendingPathComponent("failure-screen-\(slug).png"))
            try? app.debugDescription.write(
                to: outputDirectory.appendingPathComponent("failure-screen-\(slug)-hierarchy.txt"), atomically: true, encoding: .utf8)
        }
        return opened
    }

    /// True once any of `markers` exists and `origin` has gone.
    private func waitForScreen(shownBy markers: [XCUIElement], leaving origin: XCUIElement, timeout: TimeInterval) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        repeat {
            // Hittable, not merely present: a screen that flashes up and is dismissed again (a failed Face ID match) must not count.
            if markers.contains(where: { isShowing($0) }), !isShowing(origin) { return true }
            Thread.sleep(forTimeInterval: 0.5)
        } while Date() < deadline
        return false
    }

    /// True when `element` exists, is enabled and the centre of its non-empty frame is on the screen (the app's own frame; not below the fold, not slid off any edge): what "hittable" asks,
    /// read from a snapshot. `element.isHittable`, `frame` and a `hittable` predicate all record a test failure of their own
    /// ("Failed to determine hittability ... Activation point invalid") when the element is sliding in or out, even when the
    /// caller is polling; a snapshot throws instead, and a throw simply reads as "not there yet".
    private func isShowing(_ element: XCUIElement, within seconds: TimeInterval = 3) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            if let snapshot = try? element.snapshot(), snapshot.isEnabled, !snapshot.frame.isEmpty,
                let screen = try? app.snapshot().frame, !screen.isEmpty,
                // A tap lands on the frame's centre, so the centre has to be on the screen: a card below the fold, or a
                // screen not yet slid in from the side, has a frame but no activation point a person could reach.
                screen.contains(CGPoint(x: snapshot.frame.midX, y: snapshot.frame.midY))
            {
                return true
            }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return false
    }

    /// True once `element` exists, within `timeout`. It polls through XCTest's predicate expectation rather than
    /// `waitForExistence`, because a query that times out while the app is busy fails the test outright when asked
    /// directly ("Timed out while evaluating UI query") but is simply asked again by the predicate polling.
    private func appears(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let exists = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: element)
        return XCTWaiter().wait(for: [exists], timeout: timeout) == .completed
    }

    /// Waits until `element` exists and is hittable, then taps it. A step that cannot act fails naming the step and the
    /// element. The timeout is `screenTimeout` like every other wait here: a hosted macOS simulator can take a minute to
    /// finish a transition under load.
    private func tapWhenHittable(_ element: XCUIElement, step: String, elementName: String) {
        waitUntilHittable(element, step: step, elementName: elementName)
        // A screen still sliding in answers "hittable" and then has no activation point by the time the tap resolves it
        // ("Activation point invalid"). Hittable again half a second later means the element has stopped moving.
        Thread.sleep(forTimeInterval: 0.5)
        waitUntilHittable(element, step: step, elementName: elementName)
        element.tap()
    }

    private func waitUntilHittable(_ element: XCUIElement, step: String, elementName: String) {
        XCTAssertTrue(
            isHittableNow(within: screenTimeout, element),
            "step \"\(step)\": \(elementName) did not exist and become hittable within \(Int(screenTimeout)) s")
    }

    /// True once `element` is showing (snapshot, which cannot record a failure) and XCTest's own `isHittable` agrees, so
    /// an element that is covered or otherwise cannot take a tap does not pass on its frame alone. The snapshot gates
    /// the `isHittable` read: asked of an element that is still sliding in or out, `isHittable` records its own failure.
    private func isHittableNow(within seconds: TimeInterval, _ element: XCUIElement) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            if isShowing(element, within: 1), element.isHittable { return true }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return false
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
        if !appears(field, timeout: screenTimeout) {
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
            for _ in 1...3 where !typed {
                tapWhenHittable(field, step: "type capture", elementName: "the composer field")
                // Typing before the keyboard is up makes the UI-test runner fail while it looks for interrupting alerts.
                XCTAssertTrue(appears(app.keyboards.firstMatch, timeout: screenTimeout), "keyboard did not appear")
                field.typeText(text)
                let enabled = XCTNSPredicateExpectation(predicate: saveIsReady, object: save)
                // 30 s, not less: run 40206045500 attempt 2 spent 60 s inside the typing event waiting for the app to idle on a
                // loaded runner, so the field's text reached the composer later than a short wait allows.
                typed = XCTWaiter().wait(for: [enabled], timeout: 30) == .completed
            }
            if !typed {
                // Taken once, after the last attempt: asking a stalled simulator for a screenshot between attempts can
                // itself time out and records a failure of its own.
                try? XCUIScreen.main.screenshot().pngRepresentation.write(
                    to: outputDirectory.appendingPathComponent("failure-capture-attempts.png"))
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
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where appears(button, timeout: 2) {
            waitUntilHittable(button, step: "dismiss the system tip", elementName: "the Continue button")
            button.tap()
        }
        if appears(app.keyboards.firstMatch, timeout: 5) {
            let done = app.buttons[homeDone]
            XCTAssertTrue(appears(done, timeout: screenTimeout), "Done did not replace Settings while the keyboard is up")
            tapWhenHittable(done, step: "put the keyboard away", elementName: "the Done button")
        }
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 0"), object: app.keyboards)
        // screenTimeout like the other waits: run 40219792886 (host load average ~700) still showed the keyboard 20 s after
        // Done was tapped, because a loaded simulator slows the slide-away animation down to a crawl.
        XCTAssertEqual(
            XCTWaiter().wait(for: [gone], timeout: screenTimeout), .completed,
            "step \"put the keyboard away\": the keyboard was still showing \(Int(screenTimeout)) s after Done was tapped")
    }

    /// Taps a home link and waits for the destination. The first open per launch shows the system Face ID prompt,
    /// which the driving script answers with a matching face.
    private func openFromHome(_ identifier: String, name: String, shows marker: XCUIElement) {
        // A failed or cancelled Face ID attempt leaves the user on home with a notice; asking again is what a person does.
        for attempt in 1...3 {
            let link = app.buttons[identifier]
            let linkAppeared = appears(link, timeout: screenTimeout)
            if !linkAppeared {
                // What the home showed instead (a lock screen, a prompt, a notice).
                try? XCUIScreen.main.screenshot().pngRepresentation.write(
                    to: outputDirectory.appendingPathComponent("failure-home-\(identifier)-attempt\(attempt).png"))
                try? app.debugDescription.write(
                    to: outputDirectory.appendingPathComponent("failure-home-\(identifier)-hierarchy.txt"), atomically: true, encoding: .utf8)
            }
            XCTAssertTrue(linkAppeared, "\(identifier) not on home")
            waitUntilHittable(link, step: "open \(name) from home", elementName: "the \(identifier) link")
            link.tap()
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
        // screenTimeout, not 10 s: the back button appears when the pushed screen finishes its transition, which a loaded
        // simulator can stretch for a long time.
        let deadline = Date().addingTimeInterval(screenTimeout)
        var tapped = false
        repeat {
            if isHittableNow(within: 1, glassBack) {
                glassBack.tap()
                tapped = true
            } else if isHittableNow(within: 1, barBack) {
                barBack.tap()
                tapped = true
            } else {
                Thread.sleep(forTimeInterval: 0.5)
            }
        } while !tapped && Date() < deadline
        if !tapped {
            // What was on screen instead of a back button.
            let slug = destinationName.lowercased().replacingOccurrences(of: " ", with: "-")
            try? XCUIScreen.main.screenshot().pngRepresentation.write(
                to: outputDirectory.appendingPathComponent("failure-back-to-\(slug).png"))
            try? app.debugDescription.write(
                to: outputDirectory.appendingPathComponent("failure-back-to-\(slug)-hierarchy.txt"), atomically: true, encoding: .utf8)
        }
        XCTAssertTrue(tapped, "going back to \(destinationName): no back button was hittable within \(Int(screenTimeout)) s")
        XCTAssertTrue(appears(destination, timeout: screenTimeout), "going back did not return to \(destinationName)")
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
