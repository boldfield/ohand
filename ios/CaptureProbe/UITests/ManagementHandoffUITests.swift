import XCTest

/// Drives CaptureProbe (the native entry) and the Tauri management shell (com.boldfield.ohand.tauri-probe, built and
/// installed by the tauri-probe CI job) through a real handoff: a saved entry's "Review in management app" button opens
/// the shell with only the capture identifier. Each phase prints `HANDOFF-PHASE <name> <captureId|-> <detail>`; the CI
/// step then checks the files both apps persisted against those lines (verify_handoff_evidence.py).
final class ManagementHandoffUITests: XCTestCase {
    private let captureApp = XCUIApplication()
    private let managementApp = XCUIApplication(bundleIdentifier: "com.boldfield.ohand.tauri-probe")
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private let shortcutURL = URL(string: "ohand-captureprobe://capture")!
    private let largeTextArguments = ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"]
    private let uuidPattern = "[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}"

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
    }

    func testColdAndWarmHandoffsKeepTheCaptureIdentifierAndHostileRoutesAreRejected() throws {
        guard #available(iOS 16.4, *) else {
            XCTFail("opening a URL from a UI test needs iOS 16.4 or later")
            return
        }

        // Cold: the management shell is not running when the handoff arrives.
        managementApp.terminate()
        captureApp.terminate()
        openThroughSystem(shortcutURL)
        let coldId = expectSavedEntry(phase: "cold", previousIds: [])
        XCTAssertEqual(managementApp.state, .notRunning, "cold phase needs the management shell stopped")
        tapManagementButton()
        expectManagementShell(shows: [coldId], phase: "cold")
        print("HANDOFF-PHASE cold \(coldId) notRunning")

        // Warm: the shell is alive in the background when the next handoff arrives.
        XCUIDevice.shared.press(.home)
        openThroughSystem(shortcutURL)
        let warmId = expectSavedEntry(phase: "warm", previousIds: [coldId])
        let shellState = managementApp.state
        XCTAssertTrue(
            shellState == .runningBackground || shellState == .runningBackgroundSuspended,
            "warm phase needs the management shell alive in the background, found state \(shellState.rawValue)"
        )
        tapManagementButton()
        expectManagementShell(shows: [coldId, warmId], phase: "warm")
        print("HANDOFF-PHASE warm \(warmId) background")

        // Hostile URLs sent straight to the shell: each is counted and none adds a list entry.
        let hostileURLs = [
            "ohand-tauri://capture?captureId=not-a-uuid",
            "ohand-tauri://capture/../../admin?captureId=\(warmId)",
        ]
        for (index, hostile) in hostileURLs.enumerated() {
            openThroughSystem(try XCTUnwrap(URL(string: hostile)))
            expectRejectedCount(index + 1)
        }
        expectManagementShell(shows: [coldId, warmId], phase: "rejected")
        XCTAssertEqual(managementApp.staticTexts.matching(NSPredicate(format: "label MATCHES %@", uuidPattern)).count, 2)
        print("HANDOFF-PHASE rejected - \(hostileURLs.count)")
        screenshot(managementApp, "management-rejected")
    }

    func testLargeTextKeepsTheHandoffControlAndTheShellListReachable() throws {
        guard #available(iOS 16.4, *) else {
            XCTFail("opening a URL from a UI test needs iOS 16.4 or later")
            return
        }

        captureApp.terminate()
        captureApp.launchArguments = largeTextArguments
        captureApp.launch()
        openThroughSystem(shortcutURL)
        let entryId = expectSavedEntry(phase: "large-text-capture", previousIds: [])
        let button = captureApp.buttons["open-management"]
        XCTAssertTrue(button.waitForExistence(timeout: 20), "handoff button missing at the largest text size")
        XCTAssertTrue(button.isEnabled, "handoff button disabled for a saved entry")
        var scrolls = 0
        while !button.isHittable && scrolls < 4 {
            captureApp.swipeUp()
            scrolls += 1
        }
        XCTAssertTrue(button.isHittable, "handoff button not reachable at the largest text size")
        screenshot(captureApp, "capture-large-text")
        print("HANDOFF-PHASE large-text-capture \(entryId) scrolls=\(scrolls)")

        managementApp.terminate()
        managementApp.launchArguments = largeTextArguments
        managementApp.launch()
        XCTAssertTrue(managementApp.wait(for: .runningForeground, timeout: 30))
        XCTAssertTrue(
            managementApp.staticTexts.matching(NSPredicate(format: "label MATCHES %@", uuidPattern)).firstMatch
                .waitForExistence(timeout: 30),
            "received identifiers not shown at the largest text size"
        )
        screenshot(managementApp, "management-large-text")
        print("HANDOFF-PHASE large-text-management - ok")
    }

    @available(iOS 16.4, *)
    private func openThroughSystem(_ url: URL) {
        XCUIDevice.shared.system.open(url)
        // The system may ask before opening a custom URL scheme; accept it when it does.
        let openButton = springboard.buttons["Open"]
        if openButton.waitForExistence(timeout: 5) {
            openButton.tap()
        }
    }

    private func tapManagementButton() {
        let button = captureApp.buttons["open-management"]
        XCTAssertTrue(button.waitForExistence(timeout: 20), "handoff button missing")
        XCTAssertTrue(button.isEnabled, "handoff button disabled for a saved entry")
        button.tap()
        // A system prompt is not expected for an app-to-app open; accept one if it appears.
        let openButton = springboard.buttons["Open"]
        if openButton.waitForExistence(timeout: 3) {
            openButton.tap()
        }
        XCTAssertTrue(managementApp.wait(for: .runningForeground, timeout: 30), "management shell never came to the foreground")
    }

    private func expectManagementShell(shows captureIds: [String], phase: String) {
        for captureId in captureIds {
            XCTAssertTrue(
                managementApp.staticTexts[captureId].waitForExistence(timeout: 30),
                "\(phase): management shell does not list \(captureId)"
            )
        }
        screenshot(managementApp, "management-\(phase)")
    }

    private func expectRejectedCount(_ count: Int) {
        XCTAssertTrue(managementApp.wait(for: .runningForeground, timeout: 30), "management shell not foreground")
        let summary = managementApp.staticTexts.matching(
            NSPredicate(format: "label CONTAINS %@", "Handoffs rejected: \(count).")
        ).firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 30), "shell never reported \(count) rejected handoff(s)")
    }

    private func screenshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Waits for a saved entry whose ID is not in `previousIds` and returns it.
    private func expectSavedEntry(phase: String, previousIds: [String]) -> String {
        XCTAssertTrue(captureApp.wait(for: .runningForeground, timeout: 30), "\(phase): capture app not foreground")
        let detail = captureApp.staticTexts["capture-detail"]
        var predicateText = "label CONTAINS %@"
        var arguments: [Any] = ["Entry: shortcutURL"]
        for previousId in previousIds {
            predicateText += " AND NOT (label CONTAINS %@)"
            arguments.append(previousId)
        }
        XCTAssertTrue(detail.waitForExistence(timeout: 20), "\(phase): detail label missing")
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: predicateText, argumentArray: arguments),
            object: detail
        )
        XCTAssertEqual(XCTWaiter().wait(for: [expectation], timeout: 20), .completed, "\(phase): no new entry; shows \(detail.label)")
        XCTAssertEqual(captureApp.staticTexts["capture-status"].label, "Saved", "\(phase): entry not saved")

        let idLine = detail.label.components(separatedBy: "\n").first { $0.hasPrefix("Capture ID: ") }
        let captureId = String(idLine?.dropFirst("Capture ID: ".count) ?? "")
        XCTAssertNotNil(UUID(uuidString: captureId), "\(phase): rendered capture ID \(captureId) is not a UUID")
        screenshot(captureApp, "capture-\(phase)")
        return captureId
    }
}
