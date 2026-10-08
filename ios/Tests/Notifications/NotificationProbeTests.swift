import XCTest
import UserNotifications

final class NotificationProbeTests: XCTestCase {
    private let notificationCenter = UNUserNotificationCenter.current()
    private let testTimeout: TimeInterval = 5.0

    override func setUp() {
        super.setUp()
        notificationCenter.removeAllPendingNotificationRequests()

        let authExpectation = XCTestExpectation(description: "Request authorization")
        notificationCenter.requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in
            authExpectation.fulfill()
        }
        wait(for: [authExpectation], timeout: testTimeout)
    }

    override func tearDown() {
        notificationCenter.removeAllPendingNotificationRequests()
        super.tearDown()
    }

    // MARK: - Permission Tests

    func testPermissionStateCanBeQueried() {
        let expectation = XCTestExpectation(description: "Permission state retrieved")

        notificationCenter.getNotificationSettings { settings in
            XCTAssertNotNil(settings)
            XCTAssertTrue([
                UNAuthorizationStatus.notDetermined,
                UNAuthorizationStatus.denied,
                UNAuthorizationStatus.authorized
            ].contains(settings.authorizationStatus))
            expectation.fulfill()
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - Duplicate Identifier Tests

    func testDuplicateIdReplacement() {
        let expectation = XCTestExpectation(description: "Duplicate ID replacement verified")
        let duplicateId = "duplicate-test-id"

        let content1 = UNMutableNotificationContent()
        content1.title = "First Request"
        content1.body = "This should be replaced"

        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60, repeats: false)
        let request1 = UNNotificationRequest(identifier: duplicateId, content: content1, trigger: trigger)

        notificationCenter.add(request1) { error in
            XCTAssertNil(error, "First request should succeed")

            let content2 = UNMutableNotificationContent()
            content2.title = "Second Request"
            content2.body = "This should replace the first"

            let request2 = UNNotificationRequest(identifier: duplicateId, content: content2, trigger: trigger)

            self.notificationCenter.add(request2) { error in
                XCTAssertNil(error, "Second request with duplicate ID should succeed")

                self.notificationCenter.getPendingNotificationRequests { requests in
                    let duplicate = requests.first(where: { $0.identifier == duplicateId })
                    XCTAssertNotNil(duplicate, "Request should exist")
                    XCTAssertEqual(duplicate?.content.title, "Second Request", "Content should be from second request")

                    let allWithDuplicateId = requests.filter { $0.identifier == duplicateId }
                    XCTAssertEqual(allWithDuplicateId.count, 1, "Only one request with this ID should exist")

                    expectation.fulfill()
                }
            }
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - Cancel by Identifier Tests

    func testCancelByIdentifier() {
        let expectation = XCTestExpectation(description: "Cancel by identifier verified")
        let testIds = ["cancel-1", "cancel-2", "cancel-3"]

        let group = DispatchGroup()

        for id in testIds {
            group.enter()
            let content = UNMutableNotificationContent()
            content.title = "Test \(id)"
            content.body = "To be canceled"

            let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60, repeats: false)
            let request = UNNotificationRequest(identifier: id, content: content, trigger: trigger)

            notificationCenter.add(request) { error in
                XCTAssertNil(error)
                group.leave()
            }
        }

        group.notify(queue: .main) {
            self.notificationCenter.getPendingNotificationRequests { requests in
                XCTAssertGreaterThanOrEqual(requests.count, 3, "Should have at least 3 pending requests")

                let beforeCount = requests.count
                self.notificationCenter.removePendingNotificationRequests(withIdentifiers: ["cancel-1", "cancel-2"])

                let removeGroup = DispatchGroup()
                removeGroup.enter()

                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                    removeGroup.leave()
                }

                removeGroup.notify(queue: .main) {
                    self.notificationCenter.getPendingNotificationRequests { requestsAfter in
                        let afterCount = requestsAfter.count
                        XCTAssertEqual(beforeCount - afterCount, 2, "Should have removed exactly 2 requests")

                        let remaining = requestsAfter.filter { testIds.contains($0.identifier) }
                        XCTAssertEqual(remaining.count, 1, "Only cancel-3 should remain")
                        XCTAssertEqual(remaining.first?.identifier, "cancel-3")

                        expectation.fulfill()
                    }
                }
            }
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - Capacity Tests

    func testCapacityLimit() {
        let expectation = XCTestExpectation(description: "Capacity limit verified")

        notificationCenter.removeAllPendingNotificationRequests()

        let group = DispatchGroup()

        for i in 0..<100 {
            group.enter()
            let content = UNMutableNotificationContent()
            content.title = "Capacity Test \(i)"
            content.body = "Testing pending request capacity"

            let trigger = UNTimeIntervalNotificationTrigger(
                timeInterval: TimeInterval(60 + i * 5),
                repeats: false
            )
            let request = UNNotificationRequest(
                identifier: "capacity-\(i)",
                content: content,
                trigger: trigger
            )

            notificationCenter.add(request) { error in
                group.leave()
            }
        }

        group.notify(queue: .main) {
            self.notificationCenter.getPendingNotificationRequests { requests in
                let actualLimit = requests.count
                XCTAssertGreaterThan(actualLimit, 0, "Some requests should be pending")
                XCTAssertLessThan(actualLimit, 100, "Not all 100 requests should be accepted")

                expectation.fulfill()
            }
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - Calendar Trigger Tests

    func testCalendarTriggerFireDate() {
        let expectation = XCTestExpectation(description: "Calendar trigger fire date computed")

        var components = DateComponents()
        components.minute = 1
        let futureDate = Calendar.current.date(byAdding: components, to: Date())!

        let dateComponents = Calendar.current.dateComponents(
            [.year, .month, .day, .hour, .minute],
            from: futureDate
        )

        let content = UNMutableNotificationContent()
        content.title = "Calendar Test"
        content.body = "Fire date should be computable"

        let trigger = UNCalendarNotificationTrigger(dateMatching: dateComponents, repeats: false)
        let request = UNNotificationRequest(identifier: "calendar-test", content: content, trigger: trigger)

        notificationCenter.add(request) { error in
            XCTAssertNil(error, "Request should be added successfully")

            self.notificationCenter.getPendingNotificationRequests { requests in
                let pending = requests.first(where: { $0.identifier == "calendar-test" })
                XCTAssertNotNil(pending, "Request should be pending")

                if let calendarTrigger = pending?.trigger as? UNCalendarNotificationTrigger {
                    let nextFire = calendarTrigger.nextTriggerDate()
                    XCTAssertNotNil(nextFire, "Calendar trigger should compute next fire date")
                    XCTAssertGreaterThan(nextFire ?? Date(), Date(), "Fire date should be in future")
                } else {
                    XCTFail("Trigger should be a calendar trigger")
                }

                expectation.fulfill()
            }
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - List Pending Requests Tests

    func testListPendingRequests() {
        let expectation = XCTestExpectation(description: "List pending requests verified")

        let testIds = ["list-1", "list-2", "list-3"]
        let group = DispatchGroup()

        for id in testIds {
            group.enter()
            let content = UNMutableNotificationContent()
            content.title = "Pending Test \(id)"
            content.body = "Should be listed"

            let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60, repeats: false)
            let request = UNNotificationRequest(identifier: id, content: content, trigger: trigger)

            notificationCenter.add(request) { error in
                XCTAssertNil(error)
                group.leave()
            }
        }

        group.notify(queue: .main) {
            self.notificationCenter.getPendingNotificationRequests { requests in
                let pending = requests.filter { testIds.contains($0.identifier) }
                XCTAssertEqual(pending.count, 3, "All test requests should be listed")

                let titles = Set(pending.map { $0.content.title })
                XCTAssertEqual(titles.count, 3, "All titles should be present and unique")

                expectation.fulfill()
            }
        }

        wait(for: [expectation], timeout: testTimeout)
    }

    // MARK: - Request Addition Error Handling

    func testAddRequestErrorHandling() {
        let expectation = XCTestExpectation(description: "Request addition error handling verified")

        let content = UNMutableNotificationContent()
        content.title = "Valid Request"

        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60, repeats: false)
        let request = UNNotificationRequest(identifier: "valid-request", content: content, trigger: trigger)

        notificationCenter.add(request) { error in
            XCTAssertNil(error, "Valid request should not error")
            expectation.fulfill()
        }

        wait(for: [expectation], timeout: testTimeout)
    }
}
