import XCTest
@testable import OhAndServices

/// The fake stands in for an OS whose late completions land on other threads while a test polls
/// what was recorded, so its recorded state must be safe to read during writes.
final class FakeNotificationCenterConcurrencyTests: XCTestCase {
    func testPollingRecordedAddsWhileAddsLandOnOtherThreadsIsSafe() async throws {
        let center = FakeNotificationCenter()
        let totalAdds = 400
        let due = Date(timeIntervalSince1970: 1_800_000_000)
        let writer = Task.detached {
            for index in 0..<totalAdds {
                let request = NotificationCenterRequest(
                    identifier: "reminder-\(index)#1",
                    dueInstant: due,
                    content: NotificationContent(title: "Title", body: "Body", userInfo: [:])
                )
                try await center.add(request)
                await center.removePending(identifiers: ["reminder-\(index)#1"])
            }
        }
        let deadline = Date().addingTimeInterval(30)
        var observed = 0
        while observed < totalAdds, Date() < deadline {
            observed = center.addedRequests.count
            _ = center.removedIdentifiers.count
            await Task.yield()
        }
        try await writer.value
        XCTAssertEqual(center.addedRequests.count, totalAdds)
        XCTAssertEqual(center.removedIdentifiers.count, totalAdds)
    }
}
