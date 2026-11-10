import XCTest

final class NotificationProbeDeniedUITests: NotificationProbeUITestCase {
    func testDeniedPermissionBehavior() {
        let before = run("settings")
        XCTAssertEqual(before["authorizationStatus"], "notDetermined")

        let denied = run("requestAuthorization") { self.answerAuthorizationPrompt(allow: false) }
        XCTAssertEqual(denied["granted"], "false")
        XCTAssertEqual(denied["authorizationStatus"], "denied")
        XCTAssertEqual(denied["secondRequestGranted"], "false", "a repeated request returns the recorded denial without prompting")

        XCTAssertEqual(denied["alertSetting"], "disabled")
        XCTAssertEqual(denied["soundSetting"], "disabled")
        XCTAssertEqual(denied["badgeSetting"], "disabled")

        let scheduling = run("schedulingProbe")
        // The iOS 27 runtime reports UNErrorDomain 2003 (notificationsNotAllowed) from add while denied; earlier runtimes reported none.
        XCTAssertTrue(
            ["none", "UNErrorDomain#2003"].contains(scheduling["addError"] ?? ""),
            "add reports no error, or notificationsNotAllowed, while denied: \(scheduling["addError"] ?? "nil")"
        )
        XCTAssertEqual(scheduling["pendingCountAfterAdd"], "0", "a request added while denied is not retained")
        XCTAssertEqual(scheduling["pendingContainsRequest"], "false")
    }
}
