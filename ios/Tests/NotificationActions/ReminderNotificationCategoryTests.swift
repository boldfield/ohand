import UserNotifications
import XCTest
@testable import OhAndServices

final class ReminderNotificationCategoryTests: XCTestCase {
    private func registeredCategories(_ center: UNUserNotificationCenter) async -> Set<UNNotificationCategory> {
        await withCheckedContinuation { continuation in
            center.getNotificationCategories { continuation.resume(returning: $0) }
        }
    }

    func testReminderCategoryOffersAcknowledgeAndCompleteWithCompleteBehindUnlock() throws {
        let category = ReminderNotificationCategory.make()
        XCTAssertEqual(category.identifier, NotificationActionIdentifier.reminderCategory)
        XCTAssertEqual(
            category.actions.map(\.identifier),
            [NotificationActionIdentifier.acknowledge, NotificationActionIdentifier.complete])
        let acknowledge = try XCTUnwrap(category.actions.first { $0.identifier == NotificationActionIdentifier.acknowledge })
        let complete = try XCTUnwrap(category.actions.first { $0.identifier == NotificationActionIdentifier.complete })
        XCTAssertFalse(acknowledge.options.contains(.authenticationRequired))
        XCTAssertTrue(complete.options.contains(.authenticationRequired))
        XCTAssertFalse(complete.options.contains(.foreground))
    }

    func testScheduledReminderRequestNamesTheRegisteredCategory() throws {
        let content = NotificationContent.generic(opaqueTargetID: try XCTUnwrap(OpaqueIdentifier("item-1")))
        let request = SystemNotificationCenter.makeRequest(
            NotificationCenterRequest(
                identifier: "rem-1#1", dueInstant: Date(timeIntervalSince1970: 1_900_000_000), content: content))
        XCTAssertEqual(request.content.categoryIdentifier, NotificationActionIdentifier.reminderCategory)
        XCTAssertEqual(request.content.userInfo as? [String: String], content.userInfo)
    }

    func testContentWithoutACategoryOffersNoActions() {
        let request = SystemNotificationCenter.makeRequest(
            NotificationCenterRequest(
                identifier: "alert",
                dueInstant: Date(timeIntervalSince1970: 1_900_000_000),
                content: NotificationContent(title: "t", body: "b", userInfo: [:])))
        XCTAssertEqual(request.content.categoryIdentifier, "")
    }

    func testSystemCenterRegistersTheReminderCategoryWithTheOS() async throws {
        let center = UNUserNotificationCenter.current()
        _ = SystemNotificationCenter(center: center)
        let categories = await registeredCategories(center)
        let registered = try XCTUnwrap(
            categories.first { $0.identifier == NotificationActionIdentifier.reminderCategory })
        XCTAssertEqual(
            registered.actions.map(\.identifier),
            [NotificationActionIdentifier.acknowledge, NotificationActionIdentifier.complete])
    }
}
