import UIKit
import XCTest

/// What the seeding run did, handed to the verifying run and to `tools/release/simulator_upgrade_check.sh` as
/// `seed-expectations.json`. It holds identifiers, lengths and labels the app itself showed; the synthetic words typed
/// into the app stay in the test source.
struct UpgradeExpectations: Codable {
    struct Kept: Codable {
        let itemID: String
        let textLength: Int
    }
    struct Corrected: Codable {
        let itemID: String
        let originalTextLength: Int
        let correctedTextLength: Int
    }
    struct Deleted: Codable {
        let itemID: String
    }
    struct Reminder: Codable {
        let itemID: String
        let textLength: Int
        /// The stored time as the reminder section words it (`reminder.stored-time`).
        let storedTimeLabel: String
    }
    struct Provider: Codable {
        let profileName: String
        /// What the seeding run chose and the app then reported: the verifier requires exactly these after the upgrade.
        let providerProtocol: String
        let model: String
        let destinationClasses: [String]
    }

    let kept: Kept
    let corrected: Corrected
    let deleted: Deleted
    let reminder: Reminder
    let provider: Provider
}

/// Shared synthetic content and the helpers both upgrade runs use. The runs drive an installed app by bundle identifier,
/// so xcodebuild never installs or replaces it: what the app holds is whatever the installs before the run left.
class UpgradeUITestCase: XCTestCase {
    static let bundleIdentifier = "com.boldfield.ohand.app"

    let keptText = "Upgrade check alpha: pick up the blue folder"
    let correctedOriginalText = "Upgrade check bravo: water the ferns"
    let correctedFinalText = "Upgrade check bravo corrected: water the ferns on friday"
    let deletedText = "Upgrade check charlie: delete this one later"
    let reminderText = "Upgrade check delta: call the plumber"
    let providerName = "upgrade-check-provider"
    let providerModel = "upgrade-check-model"
    let providerSecret = "upgrade-check-synthetic-secret"

    let app = XCUIApplication(bundleIdentifier: UpgradeUITestCase.bundleIdentifier)
    let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    let screenTimeout: TimeInterval = 90
    var outputDirectory: URL!

    override func setUpWithError() throws {
        continueAfterFailure = false
        let path = try XCTUnwrap(
            ProcessInfo.processInfo.environment["UPGRADE_OUTPUT_DIR"], "UPGRADE_OUTPUT_DIR is not set")
        outputDirectory = URL(fileURLWithPath: path, isDirectory: true)
        try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
    }

    var expectationsURL: URL { outputDirectory.appendingPathComponent("seed-expectations.json") }

    // MARK: Waiting and tapping (the walkthrough's helpers, which survived loaded hosted runners)

    /// Writes what the screen showed, so a step that cannot continue leaves evidence of why.
    func recordFailureScreen(_ name: String) {
        try? XCUIScreen.main.screenshot().pngRepresentation.write(
            to: outputDirectory.appendingPathComponent("failure-\(name).png"))
        try? app.debugDescription.write(
            to: outputDirectory.appendingPathComponent("failure-\(name)-hierarchy.txt"), atomically: true, encoding: .utf8)
    }

    func appears(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let exists = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == true"), object: element)
        return XCTWaiter().wait(for: [exists], timeout: timeout) == .completed
    }

    func disappears(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let gone = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == false"), object: element)
        return XCTWaiter().wait(for: [gone], timeout: timeout) == .completed
    }

    func isShowing(_ element: XCUIElement, within seconds: TimeInterval = 3) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            if let snapshot = try? element.snapshot(), snapshot.isEnabled, !snapshot.frame.isEmpty,
                let screen = try? app.snapshot().frame, !screen.isEmpty,
                screen.contains(CGPoint(x: snapshot.frame.midX, y: snapshot.frame.midY))
            {
                return true
            }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return false
    }

    func isHittableNow(within seconds: TimeInterval, _ element: XCUIElement) -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            if isShowing(element, within: 1), element.isHittable { return true }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        return false
    }

    /// Waits until `element` is hittable (scrolling the screen up while it is below the fold or under the keyboard),
    /// lets it settle, then taps it. A step that cannot act fails naming the step.
    func tap(_ element: XCUIElement, step: String) {
        let deadline = Date().addingTimeInterval(screenTimeout)
        var ready = false
        while !ready && Date() < deadline {
            if isHittableNow(within: 2, element) {
                ready = true
            } else if element.exists {
                scrollToward(element)
            }
        }
        if !ready { recordFailureScreen(slug(step)) }
        XCTAssertTrue(ready, "step \"\(step)\": the element did not become hittable within \(Int(screenTimeout)) s")
        Thread.sleep(forTimeInterval: 0.5)
        XCTAssertTrue(isHittableNow(within: screenTimeout, element), "step \"\(step)\": the element moved away")
        element.tap()
    }

    /// Scrolls so `element` sits in the upper middle of the screen, above any keyboard: small drags when they move the
    /// content, a full swipe when they do not (a drag that starts on a control can be taken by it).
    func scrollToward(_ element: XCUIElement) {
        guard let before = try? element.snapshot().frame else { return }
        let height = app.frame.height
        let delta = height * 0.35 - before.midY  // positive: the content has to move down
        let distance = min(abs(delta), height * 0.45)
        let startFraction = delta > 0 ? 0.2 : 0.55
        let endFraction = delta > 0 ? 0.2 + distance / height : 0.55 - distance / height
        let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: startFraction))
        let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.9, dy: endFraction))
        start.press(forDuration: 0.05, thenDragTo: end)
        Thread.sleep(forTimeInterval: 0.7)
        if let after = try? element.snapshot().frame, abs(after.midY - before.midY) < 1 {
            if delta > 0 { app.swipeDown() } else { app.swipeUp() }
            Thread.sleep(forTimeInterval: 0.7)
        }
    }

    func slug(_ text: String) -> String {
        text.lowercased().map { $0.isLetter || $0.isNumber ? String($0) : "-" }.joined()
    }

    func element(_ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    func elements(withIdentifierPrefix prefix: String) -> XCUIElementQuery {
        app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", prefix))
    }

    /// The reminder section's own controls all read as `reminder.section` (the container's identifier covers them), so they
    /// are found by their words.
    var saveReminderTimeButton: XCUIElement { app.buttons["Save time"].firstMatch }

    var storedReminderTime: XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'Stored time:'")).firstMatch
    }

    func card(containing phrase: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'home.recents.card.' AND label CONTAINS %@", phrase))
            .firstMatch
    }

    func itemID(ofCard card: XCUIElement) -> String {
        String(card.identifier.dropFirst("home.recents.card.".count))
    }

    /// The app makes one quiet Face ID attempt at launch; the driving script's matching face answers it. Typing while the
    /// prompt is up loses text, so wait until it has gone.
    func waitForLaunchAuthentication() {
        let promptIdentifier = "com.apple.localauthentication.ax.authentication.alert"
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

    /// Launches the installed app (never installs it) and waits for the home and for the launch authentication.
    func launchApp() {
        app.launch()
        XCTAssertTrue(appears(element("home.search"), timeout: screenTimeout), "the home screen did not appear")
        waitForLaunchAuthentication()
    }

    /// Puts the keyboard away with the header's Done button.
    func dismissKeyboard() {
        for button in [app.buttons["Continue"], springboard.buttons["Continue"]] where appears(button, timeout: 2) {
            tap(button, step: "dismiss the system tip")
        }
        if appears(app.keyboards.firstMatch, timeout: 3) {
            let done = app.buttons["home.done"]
            if done.exists {
                tap(done, step: "put the keyboard away")
            } else {
                app.swipeDown()
            }
        }
        _ = disappears(app.keyboards.firstMatch, timeout: 30)
    }

    /// Opens a home link (Search, Settings). The first open per launch can show the system Face ID prompt, which the
    /// driving script answers; a failed attempt is asked again, as a person would.
    func openFromHome(_ identifier: String, name: String, shows marker: XCUIElement) {
        for _ in 1...3 {
            // The home focuses its composer when it appears, and its header shows Done in place of Settings while the
            // keyboard is up, so the keyboard goes away first.
            if !app.buttons[identifier].exists { dismissKeyboard() }
            let link = app.buttons[identifier]
            XCTAssertTrue(appears(link, timeout: screenTimeout), "\(identifier) is not on the home")
            tap(link, step: "open \(name)")
            if appears(marker, timeout: 40) { return }
            recordFailureScreen("open-\(slug(name))")
        }
        XCTFail("\(name) did not open")
    }

    func goBack(step: String) {
        let glassBack = app.buttons["Back"].firstMatch
        let barBack = app.navigationBars.buttons.element(boundBy: 0)
        let deadline = Date().addingTimeInterval(screenTimeout)
        while Date() < deadline {
            if isHittableNow(within: 1, glassBack) {
                glassBack.tap()
                return
            }
            if isHittableNow(within: 1, barBack) {
                barBack.tap()
                return
            }
            Thread.sleep(forTimeInterval: 0.5)
        }
        recordFailureScreen("back-\(slug(step))")
        XCTFail("going back (\(step)): no back button was hittable")
    }

    /// The home's recents read only after the device owner is authenticated. The launch attempt can miss the matching face, so
    /// a locked list is unlocked with its own button, as a person would, and the list is given time to settle: a card that
    /// shows for an instant while the list reloads does not count.
    @discardableResult
    func requireStableCard(containing phrase: String, timeout: TimeInterval? = nil) -> XCUIElement {
        let cardElement = card(containing: phrase)
        let unlock = element("home.recents.unlock")
        let deadline = Date().addingTimeInterval(timeout ?? screenTimeout)
        var lastUnlockTap = Date.distantPast
        while Date() < deadline {
            if cardElement.exists {
                Thread.sleep(forTimeInterval: 2)
                if cardElement.exists { return cardElement }
            } else if unlock.exists, Date().timeIntervalSince(lastUnlockTap) > 20 {
                lastUnlockTap = Date()
                if isHittableNow(within: 2, unlock) { unlock.tap() }
                waitForLaunchAuthentication()
            } else {
                Thread.sleep(forTimeInterval: 0.5)
            }
        }
        recordFailureScreen("card-\(slug(phrase))")
        XCTFail("no stable home card containing \"\(phrase)\"")
        return cardElement
    }

    /// True once `element` exists and still exists `stableFor` seconds later, within `timeout`. A screen that shows for an
    /// instant and is dismissed again (a relock, a failed Face ID match) does not count.
    func appearsStably(_ element: XCUIElement, timeout: TimeInterval, stableFor: TimeInterval = 2) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if element.exists {
                Thread.sleep(forTimeInterval: stableFor)
                if element.exists { return true }
            } else {
                Thread.sleep(forTimeInterval: 0.5)
            }
        }
        return false
    }

    /// Opens the item behind a home card and waits for the source detail. The first read of stored items asks the device
    /// owner for Face ID, which the driving script answers; a failed or cancelled attempt leaves the person on the home,
    /// and asking again is what a person does.
    func openItem(card cardElement: XCUIElement) {
        let detail = element("source-detail.current-text")
        for attempt in 1...3 {
            tap(cardElement, step: "open the item")
            waitForLaunchAuthentication()
            if appearsStably(detail, timeout: 40) { return }
            recordFailureScreen("source-detail-attempt\(attempt)")
            if !element("home.search").exists { goBack(step: "after the source detail did not stay open") }
            _ = appears(cardElement, timeout: 30)
        }
        XCTFail("the source detail did not open")
    }

    /// Allows the system notification prompt if it shows (the reminder boundary asks once).
    func allowNotificationsIfAsked(within seconds: TimeInterval = 15) {
        let alert = springboard.alerts.firstMatch
        if appears(alert, timeout: seconds) {
            let allow = alert.buttons["Allow"]
            if allow.exists { allow.tap() }
        }
    }

    func write(_ expectations: UpgradeExpectations) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(expectations).write(to: expectationsURL, options: .atomic)
    }

    func readExpectations() throws -> UpgradeExpectations {
        try JSONDecoder().decode(UpgradeExpectations.self, from: Data(contentsOf: expectationsURL))
    }
}
