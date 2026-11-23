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
        // iOS 27 and later report UNErrorDomain 2003 (notificationsNotAllowed) from add while denied; earlier runtimes report none.
        var expectedAddError = "none"
        if #available(iOS 27, *) { expectedAddError = "UNErrorDomain#2003" }
        XCTAssertEqual(scheduling["addError"], expectedAddError, "add while denied reports the platform's error for this runtime")
        XCTAssertEqual(scheduling["pendingCountAfterAdd"], "0", "a request added while denied is not retained")
        XCTAssertEqual(scheduling["pendingContainsRequest"], "false")
    }
}
