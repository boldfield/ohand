import XCTest

final class NotificationProbeDeniedUITests: NotificationProbeUITestCase {
    func testDeniedPermissionBehavior() {
        let before = run("settings")
        XCTAssertEqual(before["authorizationStatus"], "notDetermined")

        let denied = run("requestAuthorization") { self.answerAuthorizationPrompt(allow: false) }
        XCTAssertEqual(denied["granted"], "false")
        XCTAssertEqual(denied["authorizationStatus"], "denied")
        XCTAssertEqual(denied["secondRequestGranted"], "false", "a repeated request returns the recorded denial without prompting")

        let scheduling = run("schedulingProbe")
        XCTAssertNotNil(scheduling["addError"], "add outcome while denied must be recorded")
        XCTAssertNotNil(scheduling["pendingCountAfterAdd"], "pending count while denied must be recorded")
    }
}
