import XCTest
@testable import OhAndServices

/// The bridge installs, lists and cancels the core's `prompt:<local date>` requests as a kind of their own,
/// beside reminders and without loosening reminder identifier validation.
final class NotificationBridgePromptTests: XCTestCase {
    private let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    private var center = FakeNotificationCenter()
    private var bridge: NotificationBridge!

    override func setUp() {
        super.setUp()
        center = FakeNotificationCenter()
        let now = currentTime
        bridge = NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { _ in }),
            now: { now },
            removalPollInterval: 0,
            maximumRemovalPolls: 5
        )
    }

    private func promptRequest(date: String = "2026-10-15", dueIn seconds: TimeInterval = 3600) throws -> PromptScheduleRequest {
        PromptScheduleRequest(
            identifier: try XCTUnwrap(PromptIdentifier(rawValue: "prompt:\(date)")),
            dueInstant: currentTime.addingTimeInterval(seconds))
    }

    func testAPromptIsInstalledUnderItsCoreIdentifierWithFixedWordingAndNoActions() async throws {
        let installed = try await bridge.schedulePrompt(promptRequest())

        XCTAssertEqual(installed.identifier.rawValue, "prompt:2026-10-15")
        XCTAssertEqual(installed.dueInstant, currentTime.addingTimeInterval(3600))
        let sent = try XCTUnwrap(center.addedRequests.first)
        XCTAssertEqual(sent.identifier, "prompt:2026-10-15")
        XCTAssertEqual(sent.content.title, PromptNotificationWording.title)
        XCTAssertEqual(sent.content.body, PromptNotificationWording.body)
        XCTAssertEqual(sent.content.userInfo, ["ohand.kind": "prompt"])
        XCTAssertNil(sent.content.categoryIdentifier)
        XCTAssertEqual(center.pendingIdentifiers, ["prompt:2026-10-15"])
    }

    func testAPromptIsRefusedWithoutPermissionAndWhenAlreadyDue() async throws {
        center.authorizationStatus = .denied
        do {
            _ = try await bridge.schedulePrompt(promptRequest())
            XCTFail("expected permission_denied")
        } catch {
            XCTAssertEqual(error as? NotificationBridgeError, .permissionDenied)
        }
        center.authorizationStatus = .authorized
        do {
            _ = try await bridge.schedulePrompt(promptRequest(dueIn: 0))
            XCTFail("expected due_time_in_past")
        } catch {
            XCTAssertEqual(error as? NotificationBridgeError, .dueTimeInPast)
        }
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testSchedulingAPromptAgainReplacesItAndCancelRemovesIt() async throws {
        _ = try await bridge.schedulePrompt(promptRequest(dueIn: 3600))
        _ = try await bridge.schedulePrompt(promptRequest(dueIn: 7200))
        XCTAssertEqual(center.pendingIdentifiers, ["prompt:2026-10-15"])

        let identifier = try XCTUnwrap(PromptIdentifier(rawValue: "prompt:2026-10-15"))
        try await bridge.cancelPrompt(identifier)
        try await bridge.cancelPrompt(identifier)

        XCTAssertTrue(center.pendingIdentifiers.isEmpty)
    }

    func testPendingPromptsListsOnlyPromptsSoonestFirstAndPendingNotificationsStayReminderOnly() async throws {
        _ = try await bridge.schedulePrompt(promptRequest(date: "2026-10-16", dueIn: 7200))
        _ = try await bridge.schedulePrompt(promptRequest(date: "2026-10-15", dueIn: 3600))
        let reminder = NotificationScheduleRequest.generic(
            identifier: try XCTUnwrap(NotificationIdentifier(reminderID: "reminder-1", scheduleGeneration: 1)),
            dueInstant: currentTime.addingTimeInterval(1800),
            opaqueTargetID: try XCTUnwrap(OpaqueIdentifier("item-1")))
        _ = try await bridge.schedule(reminder)
        center.installForeignRequest(identifier: "stray", dueInstant: currentTime.addingTimeInterval(60))

        let prompts = try await bridge.pendingPrompts()
        let reminders = try await bridge.pendingNotifications()

        XCTAssertEqual(prompts.map(\.identifier.rawValue), ["prompt:2026-10-15", "prompt:2026-10-16"])
        XCTAssertEqual(reminders.map(\.identifier.rawValue), ["reminder-1#1"])
    }
}
