import UIKit
import XCTest

/// Runs `performAccessibilityAudit()` with the default audit types on every screen a person reaches in M1, using the
/// synthetic fixtures, and asserts what the simulator can prove about VoiceOver: labels, state and announcements.
///
/// The driving script (`ios/scripts/accessibility-simulator.sh`) runs this class once at the default content size and once
/// at the largest accessibility size (`ACCESSIBILITY_CONTENT_SIZE=default|xxxl`, passed as `TEST_RUNNER_ACCESSIBILITY_CONTENT_SIZE`)
/// and names the directory for the audit log and screenshots (`TEST_RUNNER_ACCESSIBILITY_OUTPUT_DIR`). Any audit issue fails
/// the test; the only exemptions are in `isExempt`, each for one named element and listed in docs/validation/accessibility.md.
final class AccessibilityAuditUITests: XCTestCase {
    private var app: XCUIApplication!
    private var sizeName = "default"
    private var outputDirectory: URL!
    private var screenshotIndex = 0
    private var auditLines: [String] = []
    private let screenTimeout: TimeInterval = 90

    private static let largestContentSize = "UICTContentSizeCategoryAccessibilityXXXL"
    /// The launch argument the app's debug announcement log reads (`AccessibilityAnnouncer.logLaunchArgument`).
    private static let announcementLogArgument = "-OhAndAnnouncementLog"
    private static let announcementLogIdentifier = "accessibility.announcement-log"
    private static let fixtureStatusIdentifier = "ui-fixture.status"

    override func setUpWithError() throws {
        continueAfterFailure = true
        let environment = ProcessInfo.processInfo.environment
        sizeName = environment["ACCESSIBILITY_CONTENT_SIZE"] ?? "default"
        XCTAssertTrue(["default", "xxxl"].contains(sizeName), "unknown content size \(sizeName)")
        let path = environment["ACCESSIBILITY_OUTPUT_DIR"] ?? NSTemporaryDirectory() + "accessibility"
        outputDirectory = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
        app = XCUIApplication()
        app.launchArguments += [Self.announcementLogArgument]
        if sizeName == "xxxl" {
            app.launchArguments += ["-UIPreferredContentSizeCategoryName", Self.largestContentSize]
        }
    }

    override func tearDownWithError() throws {
        let log = outputDirectory.appendingPathComponent("audit-\(sizeName).log")
        let text = auditLines.joined(separator: "\n") + "\n"
        if let handle = try? FileHandle(forWritingTo: log) {
            handle.seekToEndOfFile()
            handle.write(Data(text.utf8))
            try? handle.close()
        } else {
            try text.write(to: log, atomically: true, encoding: .utf8)
        }
        app.terminate()
    }

    // MARK: Home and capture

    func testHomeEmpty() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        audit("home-empty")
    }

    func testHomePopulated() {
        guard launch(.populated) else { return }
        XCTAssertTrue(app.homeCard(containing: "Synthetic typed note about the garden hose").waitForExistence(timeout: screenTimeout))
        dismissKeyboardIfShown()
        audit("home-populated")
    }

    func testCaptureVoiceIdle() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        let record = app.buttons["home.composer.record"]
        XCTAssertTrue(record.waitForExistence(timeout: screenTimeout), "the Record button is missing")
        XCTAssertEqual(record.label, "Record")
        audit("capture-voice-idle")
    }

    func testCaptureVoiceRecording() {
        guard launch(.emptyFirstRun) else { return }
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.composer.record"], "Record") else { return }
        let stop = app.buttons["home.composer.stop"]
        // The first recording asks for the microphone; the driving script grants it ahead, and a prompt that still
        // appears is answered as a person would.
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let deadline = Date().addingTimeInterval(screenTimeout)
        while !stop.exists, Date() < deadline {
            for alert in [springboard.alerts.firstMatch, app.alerts.firstMatch] where alert.exists {
                if alert.buttons["Allow"].exists { alert.buttons["Allow"].tap() } else if alert.buttons["OK"].exists { alert.buttons["OK"].tap() }
            }
            Thread.sleep(forTimeInterval: 0.5)
        }
        guard stop.exists else {
            return XCTFail("recording did not start: the Stop button never appeared")
        }
        XCTAssertEqual(stop.label, "Stop recording")
        audit("capture-voice-recording")
        if tapWhenReady(app.buttons["home.composer.discard"], "Discard") {
            let record = app.buttons["home.composer.record"]
            XCTAssertTrue(record.waitForExistence(timeout: screenTimeout), "Discard did not end the recording")
        }
    }

    func testCaptureSilentTextEntry() {
        guard launch(.emptyFirstRun) else { return }
        guard typeCapture("Synthetic note for the accessibility audit") else { return }
        audit("capture-text-entry")
    }

    /// Saving a capture shows its outcome in a status line, which the app also announces (`CaptureEntryView`).
    func testSavingACaptureShowsTheOutcomeLine() {
        guard launch(.emptyFirstRun) else { return }
        guard typeCapture("Synthetic note saved during the accessibility check") else { return }
        let save = app.buttons["home.composer.save"]
        XCTAssertFalse(save.label.isEmpty, "Save has no label")
        guard tapWhenReady(save, "Save") else { return }
        let message = app.element(identifier: "capture-entry.message")
        XCTAssertTrue(message.waitForExistence(timeout: screenTimeout), "saving a capture showed no outcome line to announce")
        XCTAssertFalse(message.label.isEmpty, "the outcome line has no text")
    }

    // MARK: Search

    func testSearchWithResults() {
        guard launch(.populated), openSearch(), search("garden") else { return }
        XCTAssertTrue(app.staticTexts["retrieval.search.count"].waitForExistence(timeout: screenTimeout), "no results for garden")
        audit("search-results")
    }

    func testSearchWithNoResults() {
        guard launch(.populated), openSearch(), search("zzzxqv") else { return }
        XCTAssertTrue(app.staticTexts["No matches"].waitForExistence(timeout: screenTimeout) || app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'No matches'")).firstMatch.waitForExistence(timeout: 5), "search showed no empty state")
        audit("search-no-results")
    }

    // MARK: Source detail

    func testSourceDetailWithReminderAndControls() {
        guard launch(.needsCorrection) else { return }
        let card = app.homeCard(containing: "call the plumber")
        guard card.waitForExistence(timeout: screenTimeout) else { return XCTFail("no card for the reminder note") }
        dismissKeyboardIfShown()
        guard tapWhenReady(card, "the reminder card") else { return }
        guard app.element(identifier: "source-detail.current-text").waitForExistence(timeout: screenTimeout) else {
            return XCTFail("source detail did not open")
        }
        audit("source-detail-top")
        app.swipeUp()
        audit("source-detail-controls")
        app.swipeUp()
        audit("source-detail-end")
    }

    // MARK: Settings

    /// Each Settings screen the person reaches, one launch per size: the row's label, then the screen's own marker.
    func testSettingsScreens() {
        guard launch(.populated), openSettings() else { return }
        audit("settings-root")
        let screens: [(row: String, slug: String)] = [
            ("Daily prompt", "prompt-settings"),
            ("Unfinished recordings", "recovery"),
            ("Processing", "processing"),
            ("Privacy", "privacy"),
            ("Status", "status"),
            ("Audio", "data-audio"),
            ("Export", "data-export"),
        ]
        for screen in screens {
            guard openSettingsRow(screen.row) else { return }
            audit(screen.slug)
            if screen.slug == "prompt-settings" { assertPromptToggleExposesState() }
            if screen.slug == "processing" {
                for (identifier, slug) in [("provider-settings.add-provider", "processing-add-provider"), ("provider-settings.advanced", "processing-advanced")] {
                    let link = app.buttons[identifier]
                    guard scrollUntilHittable(link), tapWhenReady(link, identifier) else { return }
                    audit(slug)
                    guard goBack() else { return }
                }
            }
            guard goBack() else { return }
        }
    }

    func testSettingsPermissionsOnFirstRun() {
        guard launch(.emptyFirstRun), openSettings(), openSettingsRow("Permissions") else { return }
        audit("permissions-first-run")
    }

    func testDataSettingsEraseConfirmation() {
        guard launch(.populated), openSettings() else { return }
        let erase = app.element(identifier: "management.settings.erase")
        guard scrollUntilHittable(erase), tapWhenReady(erase, "Erase everything") else { return }
        XCTAssertTrue(app.element(identifier: "data-settings.reset.explanation").waitForExistence(timeout: screenTimeout), "the reset screen did not open")
        audit("data-erase")
        let keep = app.element(identifier: "data-settings.reset.credentials.keep")
        if scrollUntilHittable(keep), tapWhenReady(keep, "Keep credentials") {
            XCTAssertTrue(keep.isSelected, "the chosen credential option does not report that it is selected")
        }
        let proceed = app.element(identifier: "data-settings.reset.continue")
        guard scrollUntilHittable(proceed), tapWhenReady(proceed, "Erase everything (continue)") else { return }
        let confirm = app.element(identifier: "data-settings.reset.confirm")
        XCTAssertTrue(confirm.waitForExistence(timeout: screenTimeout), "the erase confirmation did not appear")
        audit("data-erase-confirmation")
        // Nothing is confirmed: the confirmation is left as it is.
    }

    // MARK: Status

    func testStatusWithFailedNote() {
        guard launch(.failedSorting), openSettings(), openSettingsRow("Status") else { return }
        XCTAssertTrue(
            app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'status.failed-note.'")).firstMatch
                .waitForExistence(timeout: screenTimeout), "Status shows no failed note")
        audit("status-failed-note")
        // A processing failure is announced when Status learns of it.
        assertAnnouncement(containing: "failed to sort")
    }

    func testStatusWithReminderNeedingATime() {
        guard launch(.needsCorrection), openSettings(), openSettingsRow("Status") else { return }
        XCTAssertTrue(
            app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'status.needs-time-note.'")).firstMatch
                .waitForExistence(timeout: screenTimeout), "Status shows no reminder that needs a time")
        audit("status-needs-time")
    }

    // MARK: Locked

    func testLockedScreen() {
        guard launch(.locked) else { return }
        let unlock = app.buttons["home.recents.unlock"]
        XCTAssertTrue(app.element(identifier: "home.recents.locked").waitForExistence(timeout: screenTimeout), "the locked card did not appear")
        XCTAssertTrue(unlock.waitForExistence(timeout: screenTimeout), "the locked screen has no unlock control")
        XCTAssertEqual(unlock.label, "Unlock")
        XCTAssertTrue(unlock.isEnabled, "the unlock control is disabled")
        dismissKeyboardIfShown()
        scrollUntilHittable(unlock)
        XCTAssertTrue(unlock.isHittable, "the unlock control cannot be reached, so authentication has a dead end")
        audit("locked")
        assertAnnouncement(containing: "Your captures are locked")
    }

    // MARK: Announcements

    func testReminderNeedingACorrectionIsAnnounced() {
        guard launch(.needsCorrection) else { return }
        XCTAssertTrue(app.homeCard(containing: "call the plumber").waitForExistence(timeout: screenTimeout))
        assertAnnouncement(containing: "A reminder needs a time")
    }

    // MARK: Audit

    /// Screenshot, then the audit with every default type. An issue not exempted by `isExempt` fails the test.
    private func audit(_ screen: String) {
        capture(screen)
        if app.keyboards.count == 0 { assertInteractiveElementsAreLabelled(screen) }
        var issueCount = 0
        do {
            try app.performAccessibilityAudit { [self] issue in
                let element = issue.element
                let description = "\(screen): \(issue.auditType) \(issue.compactDescription) | \(issue.detailedDescription) | element: \(element?.identifier ?? "?") '\(element?.label ?? "?")'"
                if isExempt(issue) {
                    auditLines.append("EXEMPT \(description)")
                    return true
                }
                issueCount += 1
                auditLines.append("ISSUE \(description)")
                return false
            }
        } catch {
            auditLines.append("ERROR \(screen): \(error)")
        }
        auditLines.append("\(issueCount == 0 ? "PASS" : "FAIL") \(screen) (\(sizeName)), \(issueCount) issue(s)")
    }

    /// The exemptions. Each one is a single named issue on a single element and is listed with its reason in
    /// docs/validation/accessibility.md. No whole audit type is ever excluded.
    private func isExempt(_ issue: XCUIAccessibilityAuditIssue) -> Bool {
        guard let identifier = issue.element?.identifier else { return false }
        // The debug-only 4-point markers that tell a test how far a fixture is and what was announced. They are not
        // part of the app a person uses (debug builds only, near-invisible by design), so their size and contrast are
        // not defects. They are exempt by identifier, for any issue.
        return identifier == Self.fixtureStatusIdentifier || identifier == Self.announcementLogIdentifier
    }

    /// Every button, switch and field a person can act on has a label that names it.
    private func assertInteractiveElementsAreLabelled(_ screen: String) {
        let kinds: [XCUIElement.ElementType] = [.button, .switch, .textField, .textView, .slider, .stepper, .link, .segmentedControl]
        for kind in kinds {
            for element in app.descendants(matching: kind).allElementsBoundByIndex where element.exists {
                let identifier = element.identifier
                if identifier == Self.fixtureStatusIdentifier || identifier == Self.announcementLogIdentifier { continue }
                let hasName = !element.label.isEmpty || !element.title.isEmpty || !((element.placeholderValue ?? "").isEmpty)
                XCTAssertTrue(hasName, "\(screen): a \(kind) with identifier '\(identifier)' has no label")
            }
        }
    }

    private func assertPromptToggleExposesState() {
        let toggle = app.element(identifier: "prompt-settings.toggle")
        guard toggle.waitForExistence(timeout: screenTimeout) else { return XCTFail("the daily prompt toggle is missing") }
        XCTAssertFalse(toggle.label.isEmpty, "the daily prompt toggle has no label")
        let value = (toggle.value as? String) ?? ""
        XCTAssertTrue(["0", "1", "On", "Off"].contains(value), "the daily prompt toggle exposes no state (value '\(value)')")
    }

    /// The app's debug announcement log holds every announcement made since launch.
    private func assertAnnouncement(containing phrase: String, file: StaticString = #filePath, line: UInt = #line) {
        let log = app.element(identifier: Self.announcementLogIdentifier)
        let predicate = NSPredicate(format: "label CONTAINS %@", phrase)
        let matched = XCTNSPredicateExpectation(predicate: predicate, object: log)
        let result = XCTWaiter().wait(for: [matched], timeout: screenTimeout)
        XCTAssertEqual(
            result, .completed, "no announcement containing '\(phrase)' (log: \(log.exists ? log.label : "no log element"))",
            file: file, line: line)
    }

    // MARK: Driving the app

    private func launch(_ fixture: OhAndFixture) -> Bool {
        let reachedHome = app.launch(fixture: fixture)
        XCTAssertTrue(reachedHome, "the \(fixture.rawValue) fixture did not reach home")
        guard reachedHome else { return false }
        if fixture == .emptyFirstRun { return true }
        return app.waitForFixtureReady()
    }

    private func typeCapture(_ text: String) -> Bool {
        let field = app.element(identifier: "home.composer.field")
        guard tapWhenReady(field, "the composer field") else { return false }
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: screenTimeout), "the keyboard did not appear")
        field.typeText(text)
        let save = app.buttons["home.composer.save"]
        let enabled = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true AND isEnabled == true"), object: save)
        let typed = XCTWaiter().wait(for: [enabled], timeout: 30) == .completed
        XCTAssertTrue(typed, "Save did not appear after typing")
        return typed
    }

    private func openSearch() -> Bool {
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.search"], "Search") else { return false }
        let query = app.textFields["retrieval.search.query"].firstMatch
        let opened = query.waitForExistence(timeout: screenTimeout)
        XCTAssertTrue(opened, "Search did not open")
        return opened
    }

    private func search(_ text: String) -> Bool {
        let query = app.textFields["retrieval.search.query"].firstMatch
        guard tapWhenReady(query, "the query field") else { return false }
        query.typeText(text)
        let key = app.keyboards.buttons["Search"]
        guard tapWhenReady(key, "the keyboard Search key") else { return false }
        return true
    }

    private func openSettings() -> Bool {
        dismissKeyboardIfShown()
        guard tapWhenReady(app.buttons["home.settings"], "Settings") else { return false }
        let header = app.staticTexts["Settings"]
        let opened = header.waitForExistence(timeout: screenTimeout)
        XCTAssertTrue(opened, "Settings did not open")
        return opened
    }

    /// A row reads its label, then its current value when one is known ("Processing, On this phone").
    private func openSettingsRow(_ label: String) -> Bool {
        let row = app.buttons.matching(NSPredicate(format: "label == %@ OR label BEGINSWITH %@", label, "\(label), ")).firstMatch
        guard row.waitForExistence(timeout: screenTimeout) else {
            XCTFail("settings row \(label) not found")
            return false
        }
        guard scrollUntilHittable(row), tapWhenReady(row, "settings row \(label)") else { return false }
        // The row is gone once the screen has opened (the Settings header leaves with it).
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: app.staticTexts["Settings"])
        _ = XCTWaiter().wait(for: [gone], timeout: 20)
        Thread.sleep(forTimeInterval: 1)
        return true
    }

    private func goBack() -> Bool {
        let glassBack = app.buttons["Back"].firstMatch
        let barBack = app.navigationBars.buttons.element(boundBy: 0)
        let deadline = Date().addingTimeInterval(screenTimeout)
        repeat {
            if glassBack.exists, glassBack.isHittable {
                glassBack.tap()
                Thread.sleep(forTimeInterval: 1)
                return true
            }
            if barBack.exists, barBack.isHittable {
                barBack.tap()
                Thread.sleep(forTimeInterval: 1)
                return true
            }
            Thread.sleep(forTimeInterval: 0.5)
        } while Date() < deadline
        XCTFail("no back button was hittable")
        return false
    }

    private func dismissKeyboardIfShown() {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where button.waitForExistence(timeout: 1) {
            button.tap()
        }
        if app.keyboards.firstMatch.waitForExistence(timeout: 3) {
            let done = app.buttons["home.done"]
            if done.waitForExistence(timeout: 10) { done.tap() }
        }
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "count == 0"), object: app.keyboards)
        _ = XCTWaiter().wait(for: [gone], timeout: 30)
    }

    /// Waits for `element` to exist and be hittable, then taps it.
    private func tapWhenReady(_ element: XCUIElement, _ name: String) -> Bool {
        guard element.waitForExistence(timeout: screenTimeout) else {
            XCTFail("\(name) did not appear")
            return false
        }
        let hittable = XCTNSPredicateExpectation(predicate: NSPredicate(format: "isHittable == true"), object: element)
        guard XCTWaiter().wait(for: [hittable], timeout: screenTimeout) == .completed else {
            XCTFail("\(name) never became hittable")
            return false
        }
        // A screen still sliding in answers "hittable" and has no activation point a moment later.
        Thread.sleep(forTimeInterval: 0.5)
        element.tap()
        return true
    }

    /// Swipes the screen up until `element` can take a tap, up to a few screens.
    @discardableResult
    private func scrollUntilHittable(_ element: XCUIElement) -> Bool {
        guard element.waitForExistence(timeout: screenTimeout) else { return false }
        var swipesLeft = 8
        while !element.isHittable, swipesLeft > 0 {
            app.swipeUp()
            swipesLeft -= 1
        }
        return element.isHittable
    }

    private func capture(_ screen: String) {
        screenshotIndex += 1
        let name = String(format: "%@-%@-%02d.png", sizeName, screen, screenshotIndex)
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
