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
        XCTAssertEqual(scheduling["addError"], "none", "add reports no error while denied")
        XCTAssertEqual(scheduling["pendingCountAfterAdd"], "0", "a request added while denied is not retained")
        XCTAssertEqual(scheduling["pendingContainsRequest"], "false")
    }
}
