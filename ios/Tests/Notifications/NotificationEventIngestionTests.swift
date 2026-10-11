import XCTest
import UserNotifications
@testable import OhAndServices

final class NotificationEventIngestionTests: XCTestCase {
    private let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    private var recorder = EventRecorder()
    private var ingestor: NotificationEventIngestor!

    override func setUp() {
        super.setUp()
        recorder = EventRecorder()
        let now = currentTime
        let recorder = self.recorder
        ingestor = NotificationEventIngestor(now: { now }, handler: { recorder.record($0) })
    }

    private let targetInfo = ["ohand.target": "item-1", "ohand.kind": "generic"]

    func testTapIsAnOpenedEventKeyedByTheCoreIdentifier() throws {
        let forwarded = ingestor.ingestResponse(
            requestIdentifier: "reminder-1#4",
            actionIdentifier: UNNotificationDefaultActionIdentifier,
            userInfo: targetInfo
        )

        XCTAssertTrue(forwarded)
        let event = try XCTUnwrap(recorder.events.first)
        XCTAssertEqual(event.identifier.rawValue, "reminder-1#4")
        XCTAssertEqual(event.kind, .opened)
        XCTAssertEqual(event.opaqueTargetID?.rawValue, "item-1")
        XCTAssertEqual(event.occurredAt, currentTime)
    }

    func testNamedActionIsForwardedAsAnOpaqueActionIdentifier() throws {
        let forwarded = ingestor.ingestResponse(
            requestIdentifier: "reminder-1#4",
            actionIdentifier: "action.acknowledge",
            userInfo: targetInfo
        )

        XCTAssertTrue(forwarded)
        let action = try XCTUnwrap(OpaqueIdentifier("action.acknowledge"))
        XCTAssertEqual(recorder.events.first?.kind, .action(action))
    }

    func testForegroundDeliveryKeepsTheOSDeliveryTime() throws {
        let deliveredAt = currentTime.addingTimeInterval(-90)
        ingestor.ingestDelivered(requestIdentifier: "reminder-1#4", userInfo: targetInfo, deliveredAt: deliveredAt)

        let event = try XCTUnwrap(recorder.events.first)
        XCTAssertEqual(event.kind, .delivered)
        XCTAssertEqual(event.occurredAt, deliveredAt)
    }

    func testFactsThatCannotBeValidatedAreDroppedNotForwarded() {
        let notOurs = ingestor.ingestDelivered(requestIdentifier: "other-app-request", userInfo: [:])
        let badAction = ingestor.ingestResponse(
            requestIdentifier: "reminder-1#4", actionIdentifier: "not an opaque action!", userInfo: targetInfo)
        let emptyAction = ingestor.ingestResponse(
            requestIdentifier: "reminder-1#4", actionIdentifier: "", userInfo: targetInfo)

        XCTAssertFalse(notOurs)
        XCTAssertFalse(badAction)
        XCTAssertFalse(emptyAction)
        XCTAssertTrue(recorder.events.isEmpty)
    }

    func testAnInvalidTargetInUserInfoIsDroppedButTheEventStillCarriesTheIdentifier() throws {
        ingestor.ingestDelivered(
            requestIdentifier: "reminder-1#4",
            userInfo: ["ohand.target": "free text with spaces and a secret"]
        )

        let event = try XCTUnwrap(recorder.events.first)
        XCTAssertNil(event.opaqueTargetID)
        XCTAssertEqual(event.identifier.rawValue, "reminder-1#4")
    }
}
