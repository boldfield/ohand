import XCTest

/// Drives CaptureProbe on a simulator through plain launches and shortcut-URL handoffs and checks what the capture
/// screen renders. Each phase prints a `CAPTURE-PHASE <name> <captureId|idle> <launchKind|->` line;
/// smoke-capture-simulator.sh then checks the files the app persisted against those lines.
final class CaptureProbeHandoffUITests: XCTestCase {
    private let app = XCUIApplication()
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private let shortcutURL = URL(string: "ohand-captureprobe://capture")!
    private let probeBundleIdentifier = "com.boldfield.ohand.probes.capture"

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    func testEachShortcutHandoffRendersOneNewEntryAndPlainLaunchesRenderNone() throws {
        guard #available(iOS 16.4, *) else {
            XCTFail("opening a URL from a UI test needs iOS 16.4 or later")
            return
        }

        app.launch()
        expectIdle(phase: "plain-cold")

        app.terminate()
        openShortcutURL(phase: "url-cold", previousIds: [])
        let firstColdId = expectSavedEntry(kind: "cold", phase: "url-cold", previousIds: [])

        app.terminate()
        openShortcutURL(phase: "url-cold-after-restart", previousIds: [firstColdId])
        let secondColdId = expectSavedEntry(kind: "cold", phase: "url-cold-after-restart", previousIds: [firstColdId])

        sendAppToBackground()
        openShortcutURL(phase: "url-warm", previousIds: [firstColdId, secondColdId])
        let warmId = expectSavedEntry(kind: "warm", phase: "url-warm", previousIds: [firstColdId, secondColdId])

        sendAppToBackground()
        app.activate()
        expectIdle(phase: "plain-warm")

        openShortcutURL(phase: "url-foreground", previousIds: [firstColdId, secondColdId, warmId])
        _ = expectSavedEntry(kind: "warm", phase: "url-foreground", previousIds: [firstColdId, secondColdId, warmId])
    }

    /// Shows what a not-in-the-foreground failure carries: with the app not running, the diagnosis says the app is not in
    /// front and that no prompt is showing, and the screen's screenshot is attached.
    func testTheNotInForegroundDiagnosisNamesWhatIsInFrontAndAttachesAScreenshot() {
        if app.state != .notRunning {
            app.terminate()
        }
        XCTAssertTrue(waitUntil(timeout: 15, { app.state == .notRunning }), "the app should not be running: \(describeWhatIsInFront())")
        let diagnosis = describeWhatIsInFront()
        print("CAPTURE-DIAGNOSIS \(diagnosis)")
        XCTAssertFalse(diagnosis.contains("foreground application: \(probeBundleIdentifier)"), diagnosis)
        XCTAssertTrue(diagnosis.contains("foreground application: "), diagnosis)
        XCTAssertTrue(diagnosis.contains("\"Open\" prompt: not showing"), diagnosis)
        XCTAssertTrue(diagnosis.contains("\(probeBundleIdentifier)=notRunning"), diagnosis)
        attachScreenshotOfTheScreen(named: "not-foreground-diagnosis-example")
    }

    /// The stale-state case: an application was sent Home, so SpringBoard's home screen is in front whatever that
    /// application's `state` still reports. The diagnosis must name SpringBoard and must not name the application. Settings
    /// stands in for the probe: launching the probe would rewrite the presented-state file that
    /// smoke-capture-simulator.sh checks against the `CAPTURE-PHASE` lines after the whole run.
    func testTheDiagnosisNamesSpringBoardWhenTheHomeScreenShowsAfterAnApplicationWasSentHome() {
        let settings = XCUIApplication(bundleIdentifier: "com.apple.Preferences")
        settings.launch()
        XCTAssertTrue(waitUntil(timeout: 20, { settings.state == .runningForeground }), "Settings should be in front: \(describeWhatIsInFront())")
        for _ in 1...4 {
            XCUIDevice.shared.press(.home)
            if waitUntil(timeout: 6, { homeScreenShowing }) { break }
        }
        XCTAssertTrue(homeScreenShowing, "the home screen should be showing")
        let diagnosis = describeWhatIsInFront()
        print("CAPTURE-DIAGNOSIS \(diagnosis)")
        XCTAssertTrue(diagnosis.contains("foreground application: com.apple.springboard"), diagnosis)
        XCTAssertTrue(diagnosis.contains("home screen: showing"), diagnosis)
        XCTAssertFalse(diagnosis.contains("foreground application: com.apple.Preferences"), diagnosis)
        attachScreenshotOfTheScreen(named: "home-screen-diagnosis-example")
    }

    /// The diagnosis the failure messages carry: which of the applications that can be in front is, whether SpringBoard's
    /// "Open" prompt is showing, and every candidate's state.
    private func describeWhatIsInFront() -> String {
        let candidates = [probeBundleIdentifier, "com.apple.springboard", "com.apple.mobilesafari", "com.apple.Preferences", "com.apple.shortcuts"]
        // A showing home screen outranks every reported state: `XCUIApplication.state` can keep saying
        // `runningForeground` for an app that Home has already sent away (see `homeScreenShowing`).
        let homeScreenIsShowing = homeScreenShowing
        var foregroundIdentifier: String? = homeScreenIsShowing ? "com.apple.springboard" : nil
        var states: [String] = []
        for identifier in candidates {
            let state = XCUIApplication(bundleIdentifier: identifier).state
            states.append("\(identifier)=\(stateName(state))")
            if !homeScreenIsShowing && state == .runningForeground && foregroundIdentifier == nil {
                foregroundIdentifier = identifier
            }
        }
        let promptShowing = springboard.buttons["Open"].exists
        return "foreground application: \(foregroundIdentifier ?? "none of the known applications"); "
            + "\"Open\" prompt: \(promptShowing ? "showing" : "not showing"); home screen: \(homeScreenIsShowing ? "showing" : "not showing"); states: \(states.joined(separator: ", "))"
    }

    private func stateName(_ state: XCUIApplication.State) -> String {
        switch state {
        case .unknown: return "unknown"
        case .notRunning: return "notRunning"
        case .runningBackgroundSuspended: return "backgroundSuspended"
        case .runningBackground: return "background"
        case .runningForeground: return "foreground"
        @unknown default: return "state-\(state.rawValue)"
        }
    }

    private func attachScreenshotOfTheScreen(named name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Spins the run loop until `condition` holds or `timeout` passes. XCTest delivers app state changes on the main
    /// queue, and a predicate wait can lose its deadline to an accessibility snapshot.
    @discardableResult
    private func waitUntil(timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        }
        return condition()
    }

    /// Fails with what is in front, and a screenshot of the screen, unless the app reaches the foreground within `timeout`.
    private func requireForeground(_ message: String, timeout: TimeInterval) {
        if waitUntil(timeout: timeout, { appInFront }) { return }
        attachScreenshotOfTheScreen(named: "not-foreground-\(message)")
        XCTFail("app not foreground: \(message); \(describeWhatIsInFront())")
    }

    /// Opens the shortcut URL and drives the handoff to a known state: the "Open" prompt accepted (or known absent) and
    /// the app in the foreground. It waits on those conditions, accepting the prompt whenever it appears, and does not
    /// open the URL a second time, which would be a second handoff. An app that was already in front is in the
    /// foreground before the system asks anything, so for it the handoff is complete only when the new entry (an ID not in
    /// `previousIds`) is rendered or the prompt has been accepted; no fixed window decides that a prompt is absent.
    @available(iOS 16.4, *)
    private func openShortcutURL(phase: String, previousIds: [String]) {
        let wasForeground = appInFront
        XCUIDevice.shared.system.open(shortcutURL)
        var promptAccepted = false
        let handedOff = waitUntil(timeout: 45) {
            let openButton = springboard.buttons["Open"]
            if openButton.exists {
                openButton.tap()
                promptAccepted = true
                return false
            }
            guard appInFront else { return false }
            if !wasForeground || promptAccepted { return true }
            let shown = detailLabel.exists ? detailLabel.label : ""
            return shown.contains("Capture ID: ") && !previousIds.contains { shown.contains($0) }
        }
        if !handedOff {
            attachScreenshotOfTheScreen(named: "not-foreground-\(phase)-handoff")
            XCTFail("app not foreground: \(phase) handoff did not complete (prompt accepted: \(promptAccepted)); \(describeWhatIsInFront())")
        }
    }

    /// True when SpringBoard's home screen is in front (its dock shows Safari). `app.state` can keep reporting
    /// `runningForeground` for a long time after Home was pressed (hosted run 44691024441 attempt 3: the screenshot showed
    /// the home screen while four polls of the state still said foreground), so the home screen is the evidence of record.
    private var homeScreenShowing: Bool { springboard.icons["Safari"].exists }

    private var appInFront: Bool { app.state == .runningForeground && !homeScreenShowing }

    /// Presses Home until the app has left the foreground; a press that is lost on a loaded runner is pressed again.
    private func sendAppToBackground() {
        for _ in 1...4 {
            XCUIDevice.shared.press(.home)
            if waitUntil(timeout: 6, { !appInFront }) { return }
        }
        attachScreenshotOfTheScreen(named: "never-left-the-foreground")
        XCTFail("app never left the foreground; \(describeWhatIsInFront())")
    }

    private var statusLabel: XCUIElement { app.staticTexts["capture-status"] }
    private var detailLabel: XCUIElement { app.staticTexts["capture-detail"] }

    private func waitForLabel(_ element: XCUIElement, _ predicate: NSPredicate, _ message: String) {
        requireForeground(message, timeout: 20)
        XCTAssertTrue(element.waitForExistence(timeout: 20), "label missing: \(message)")
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: element)
        XCTAssertEqual(XCTWaiter().wait(for: [expectation], timeout: 20), .completed, "\(message); shows \(element.label)")
    }

    private func screenshot(_ phase: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "capture-\(phase)"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func expectIdle(phase: String) {
        waitForLabel(statusLabel, NSPredicate(format: "label == %@", "Ready"), "\(phase) should render the idle screen")
        XCTAssertFalse(detailLabel.label.contains("Capture ID:"), "\(phase) shows an entry: \(detailLabel.label)")
        screenshot(phase)
        print("CAPTURE-PHASE \(phase) idle -")
    }

    /// Waits for a saved entry whose ID is not one of `previousIds`, checks that only that ID is shown, and returns it.
    private func expectSavedEntry(kind: String, phase: String, previousIds: [String]) -> String {
        var predicateText = "label CONTAINS %@"
        var arguments: [Any] = ["Entry: shortcutURL, \(kind) launch"]
        for previousId in previousIds {
            predicateText += " AND NOT (label CONTAINS %@)"
            arguments.append(previousId)
        }
        waitForLabel(detailLabel, NSPredicate(format: predicateText, argumentArray: arguments), "\(phase) should render a new entry")
        waitForLabel(statusLabel, NSPredicate(format: "label == %@", "Saved"), "\(phase) should report the entry as saved")

        let idLine = detailLabel.label.components(separatedBy: "\n").first { $0.hasPrefix("Capture ID: ") }
        let captureId = String(idLine?.dropFirst("Capture ID: ".count) ?? "")
        XCTAssertNotNil(UUID(uuidString: captureId), "\(phase) rendered capture ID \(captureId) is not a UUID")
        for previousId in previousIds {
            XCTAssertFalse(detailLabel.label.contains(previousId), "\(phase) shows an earlier entry")
        }
        screenshot(phase)
        print("CAPTURE-PHASE \(phase) \(captureId) \(kind)")
        return captureId
    }
}
