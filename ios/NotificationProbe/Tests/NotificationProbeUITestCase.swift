import XCTest

/// Drives the NotificationProbe app through its on-screen scenario buttons and parses the JSON each one publishes.
class NotificationProbeUITestCase: XCTestCase {
    let app = XCUIApplication()

    override func setUp() {
        super.setUp()
        continueAfterFailure = false
        app.launch()
    }

    /// Taps a scenario button, optionally interacts with the system while it runs, and returns its observations.
    @discardableResult
    func run(
        _ scenario: String,
        timeout: TimeInterval = 90,
        whileRunning: (() -> Void)? = nil
    ) -> [String: String] {
        let resultLabel = app.staticTexts["probe.lastResult"]
        XCTAssertTrue(resultLabel.waitForExistence(timeout: 15), "result label missing")
        let previousResult = resultLabel.label
        let button = app.buttons["probe.run.\(scenario)"]
        XCTAssertTrue(button.waitForExistence(timeout: 15), "button for \(scenario) missing")
        button.tap()
        whileRunning?()
        let published = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label != %@ AND label CONTAINS %@", previousResult, "\"scenario\":\"\(scenario)\""),
            object: resultLabel
        )
        XCTAssertEqual(XCTWaiter().wait(for: [published], timeout: timeout), .completed, "\(scenario) never published")
        let json = resultLabel.label
        print("PROBE-RESULT \(json)")
        add(XCTAttachment(string: json))
        guard
            let data = json.data(using: .utf8),
            let parsed = try? JSONSerialization.jsonObject(with: data) as? [String: String]
        else {
            XCTFail("unparseable result: \(json)")
            return [:]
        }
        XCTAssertNil(parsed["error"], "\(scenario) reported an error: \(parsed["error"] ?? "")")
        return parsed
    }

    func answerAuthorizationPrompt(allow: Bool) {
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let alert = springboard.alerts.firstMatch
        XCTAssertTrue(alert.waitForExistence(timeout: 30), "authorization prompt never appeared")
        let button = allow
            ? alert.buttons["Allow"]
            : alert.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Don")).firstMatch
        XCTAssertTrue(button.waitForExistence(timeout: 5), "prompt button missing")
        button.tap()
    }
}
