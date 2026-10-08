import XCTest

final class NotificationProbeAuthorizedUITests: NotificationProbeUITestCase {
    func testAuthorizedSchedulingBehavior() {
        XCTContext.runActivity(named: "permission: notDetermined, then allowed") { _ in
            let before = run("settings")
            XCTAssertEqual(before["authorizationStatus"], "notDetermined")

            let granted = run("requestAuthorization") { self.answerAuthorizationPrompt(allow: true) }
            XCTAssertEqual(granted["granted"], "true")
            XCTAssertEqual(granted["authorizationStatus"], "authorized")
            XCTAssertEqual(granted["alertSetting"], "enabled")
            XCTAssertEqual(granted["soundSetting"], "enabled")
            XCTAssertEqual(granted["badgeSetting"], "enabled")
            XCTAssertEqual(granted["secondRequestGranted"], "true", "a repeated request returns the recorded decision")
        }

        XCTContext.runActivity(named: "single request and interval trigger") { _ in
            let single = run("schedulingProbe")
            XCTAssertEqual(single["addError"], "none")
            XCTAssertEqual(single["pendingContainsRequest"], "true")
            let intervalDelta = Int(single["intervalTriggerDeltaSeconds"] ?? "") ?? Int.max
            XCTAssertLessThanOrEqual(abs(intervalDelta), 5, "interval trigger must fire one interval after scheduling")
        }

        XCTContext.runActivity(named: "duplicate identifier replaces the pending request") { _ in
            let duplicate = run("duplicateIdentifier")
            XCTAssertEqual(duplicate["matchingCount"], "1")
            XCTAssertEqual(duplicate["matchingTitle"], "Second")
            XCTAssertEqual(duplicate["matchingIntervalSeconds"], "7200")
            XCTAssertEqual(duplicate["pendingTotal"], "1")
        }

        XCTContext.runActivity(named: "cancel by identifier") { _ in
            let cancel = run("cancelByIdentifier")
            XCTAssertEqual(cancel["pendingBeforeRemoval"], "3")
            XCTAssertEqual(cancel["removedIdentifierGone"], "true")
            XCTAssertEqual(cancel["remainingAfterOneRemoval"], "probe.cancel.b,probe.cancel.c")
            XCTAssertEqual(cancel["remainingAfterUnknownRemoval"], "2")
            XCTAssertEqual(cancel["pendingAfterRemoveAll"], "0")
        }

        XCTContext.runActivity(named: "calendar trigger due times") { _ in
            let calendar = run("calendarTrigger")
            for zone in ["auckland", "losAngeles"] {
                XCTAssertEqual(calendar["\(zone).addError"], "none")
                XCTAssertEqual(calendar["\(zone).deltaSeconds"], "0", "\(zone) trigger must fire at the requested wall-clock time in its zone")
            }
            XCTAssertEqual(calendar["floating.deltaSeconds"], "0")
            for key in ["springForwardGap.nextTriggerUTC", "fallBackOverlap.nextTriggerUTC", "past.addError", "past.nextTriggerUTC"] {
                XCTAssertNotNil(calendar[key], "\(key) was not recorded")
            }
        }

        XCTContext.runActivity(named: "pending request capacity") { _ in
            let capacity = run("capacity")
            XCTAssertEqual(capacity["requestsAdded"], "100")
            XCTAssertEqual(capacity["addErrorCount"], "0")
            XCTAssertNotNil(capacity["pendingCount"])
            XCTAssertNotNil(capacity["retainedAmongSoonest64"])
        }

        XCTContext.runActivity(named: "foreground delivery reaches the delegate") { _ in
            let foreground = run("foregroundDelivery")
            XCTAssertEqual(foreground["willPresentCalled"], "true")
        }

        XCTContext.runActivity(named: "delivery while the app is terminated") { _ in
            let clearedBefore = run("clearDelivered")
            XCTAssertEqual(clearedBefore["deliveredAfterRemoval"], "0")
            run("scheduleClosedAppDelivery")
            app.terminate()
            Thread.sleep(forTimeInterval: 15)
            app.launch()
            let report = run("reportDelivered")
            XCTAssertEqual(report["closedAppDeliveredPresent"], "true", "notification must be delivered while the app is not running")
            XCTAssertEqual(report["closedAppStillPending"], "false")
            XCTAssertEqual(report["willPresentCallbackCountSinceLaunch"], "0", "no foreground callback occurs for a delivery made while terminated")
            let cleared = run("clearDelivered")
            XCTAssertEqual(cleared["deliveredAfterRemoval"], "0")
        }
    }
}
