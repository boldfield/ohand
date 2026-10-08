import XCTest

/// Drives CaptureProbe on a simulator through plain launches and shortcut-URL handoffs and checks what the capture
/// screen renders. Each phase prints a `CAPTURE-PHASE <name> <captureId|idle> <launchKind|->` line;
/// smoke-capture-simulator.sh then checks the files the app persisted against those lines.
final class CaptureProbeHandoffUITests: XCTestCase {
    private let app = XCUIApplication()
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private let shortcutURL = URL(string: "ohand-captureprobe://capture")!

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
        openShortcutURL()
        let firstColdId = expectSavedEntry(kind: "cold", phase: "url-cold", previousIds: [])

        app.terminate()
        openShortcutURL()
        let secondColdId = expectSavedEntry(kind: "cold", phase: "url-cold-after-restart", previousIds: [firstColdId])

        sendAppToBackground()
        openShortcutURL()
        let warmId = expectSavedEntry(kind: "warm", phase: "url-warm", previousIds: [firstColdId, secondColdId])

        sendAppToBackground()
        app.activate()
        expectIdle(phase: "plain-warm")

        openShortcutURL()
        _ = expectSavedEntry(kind: "warm", phase: "url-foreground", previousIds: [firstColdId, secondColdId, warmId])
    }

    @available(iOS 16.4, *)
    private func openShortcutURL() {
        XCUIDevice.shared.system.open(shortcutURL)
        // The system may ask before opening a custom URL scheme; accept it when it does.
        let openButton = springboard.buttons["Open"]
        if openButton.waitForExistence(timeout: 5) {
            openButton.tap()
        }
    }

    private func sendAppToBackground() {
        XCUIDevice.shared.press(.home)
        let leftForeground = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "state != %d", XCUIApplication.State.runningForeground.rawValue),
            object: app
        )
        XCTAssertEqual(XCTWaiter().wait(for: [leftForeground], timeout: 15), .completed, "app never left the foreground")
    }

    private var statusLabel: XCUIElement { app.staticTexts["capture-status"] }
    private var detailLabel: XCUIElement { app.staticTexts["capture-detail"] }

    private func waitForLabel(_ element: XCUIElement, _ predicate: NSPredicate, _ message: String) {
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 20), "app not foreground: \(message)")
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
