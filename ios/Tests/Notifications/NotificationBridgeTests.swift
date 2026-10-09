import XCTest
@testable import OhAndServices

/// Behavior of `NotificationBridge` against a fake OS center that reproduces the observed
/// platform behavior. The real Rust core and the real center are covered by
/// `NotificationCoreIntegrationTests`.
final class NotificationBridgeTests: XCTestCase {
    private let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    private var center = FakeNotificationCenter()
    private var recorder = EventRecorder()
    private var bridge: NotificationBridge!

    override func setUp() {
        super.setUp()
        center = FakeNotificationCenter()
        recorder = EventRecorder()
        bridge = makeBridge()
    }

    private func makeBridge(maximumRemovalPolls: Int = 5) -> NotificationBridge {
        let now = currentTime
        let recorder = self.recorder
        return NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { recorder.record($0) }),
            now: { now },
            removalPollInterval: 0,
            maximumRemovalPolls: maximumRemovalPolls
        )
    }

    private func identifier(_ reminderID: String = "reminder-1", generation: Int64 = 1) throws -> NotificationIdentifier {
        try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: generation))
    }

    private func target(_ value: String = "item-1") throws -> OpaqueIdentifier {
        try XCTUnwrap(OpaqueIdentifier(value))
    }

    private func genericRequest(
        reminderID: String = "reminder-1",
        generation: Int64 = 1,
        item: String = "item-1",
        dueIn seconds: TimeInterval = 3600
    ) throws -> NotificationScheduleRequest {
        .generic(
            identifier: try identifier(reminderID, generation: generation),
            dueInstant: currentTime.addingTimeInterval(seconds),
            opaqueTargetID: try target(item)
        )
    }

    // MARK: Schedule

    func testGenericNotificationHoldsOnlyFixedWordingAndTheOpaqueTarget() async throws {
        let installed = try await bridge.schedule(genericRequest(item: "7d1c5a1e-0000-4000-8000-000000000001"))

        XCTAssertEqual(installed.identifier.rawValue, "reminder-1#1")
        XCTAssertEqual(installed.dueInstant, currentTime.addingTimeInterval(3600))
        let sent = try XCTUnwrap(center.addedRequests.first)
        XCTAssertEqual(sent.identifier, "reminder-1#1", "the core identifier is the OS request identifier")
        XCTAssertEqual(sent.content.title, GenericNotificationWording.title)
        XCTAssertEqual(sent.content.body, GenericNotificationWording.body)
        XCTAssertEqual(sent.content.userInfo, [
            "ohand.target": "7d1c5a1e-0000-4000-8000-000000000001",
            "ohand.kind": "generic",
        ])
    }

    func testPayloadNeverContainsTextFromTheCaptureOrCredentialLikeStrings() async throws {
        let canaryText = "synthetic-canary roof deposit"
        let canaryCredential = "sk-synthetic-credential-0000"
        _ = try await bridge.schedule(genericRequest())

        let sent = try XCTUnwrap(center.addedRequests.first)
        var everyString: [String] = [sent.identifier, sent.content.title, sent.content.body]
        everyString.append(contentsOf: Array(sent.content.userInfo.keys))
        everyString.append(contentsOf: Array(sent.content.userInfo.values))
        for text in everyString {
            XCTAssertFalse(text.contains(canaryText))
            XCTAssertFalse(text.contains(canaryCredential))
        }
        XCTAssertNil(OpaqueIdentifier(canaryText), "free text cannot be made into an identifier")
        XCTAssertNil(OpaqueIdentifier("Bearer \(canaryCredential)"), "an authorization header cannot be made into an identifier")
    }

    func testSchedulingTheSameIdentifierTwiceReplacesInsteadOfDuplicating() async throws {
        _ = try await bridge.schedule(genericRequest(dueIn: 3600))
        let second = try await bridge.schedule(genericRequest(dueIn: 7200))

        XCTAssertEqual(center.pendingIdentifiers, ["reminder-1#1"])
        XCTAssertEqual(second.dueInstant, currentTime.addingTimeInterval(7200))
        let pending = try await bridge.pendingNotifications()
        XCTAssertEqual(pending.count, 1)
        XCTAssertEqual(pending.first?.dueInstant, currentTime.addingTimeInterval(7200))
    }

    func testANewScheduleGenerationIsAnotherIdentifier() async throws {
        _ = try await bridge.schedule(genericRequest(generation: 1))
        _ = try await bridge.schedule(genericRequest(generation: 2, dueIn: 7200))
        XCTAssertEqual(center.pendingIdentifiers, ["reminder-1#1", "reminder-1#2"])
    }

    func testDueTimeThatIsNotInTheFutureIsRejectedWithoutCallingTheCenter() async throws {
        for offset in [-3600.0, 0, 0.5] {
            let request = try genericRequest(dueIn: offset)
            await assertFailure(.dueTimeInPast, .permanent) { _ = try await self.bridge.schedule(request) }
        }
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testDeniedAndUndeterminedPermissionAreUnauthorizedAndNothingIsScheduled() async throws {
        for status in [NotificationAuthorization.denied, .notDetermined] {
            center.authorizationStatus = status
            let request = try genericRequest()
            await assertFailure(.permissionDenied, .unauthorized) { _ = try await self.bridge.schedule(request) }
        }
        XCTAssertTrue(center.addedRequests.isEmpty)
        XCTAssertTrue(center.pendingIdentifiers.isEmpty)
    }

    func testProvisionalAndEphemeralPermissionCanSchedule() async throws {
        for status in [NotificationAuthorization.provisional, .ephemeral] {
            center.authorizationStatus = status
            _ = try await bridge.schedule(genericRequest())
        }
        XCTAssertEqual(center.pendingIdentifiers, ["reminder-1#1"])
    }

    func testARequestTheCenterAcceptsButDoesNotKeepIsNotReportedInstalled() async throws {
        center.retainsAddedRequests = false
        let request = try genericRequest()
        await assertFailure(.installNotConfirmed, .transient) { _ = try await self.bridge.schedule(request) }
    }

    func testCenterFailuresAreNormalizedWithoutTheirText() async throws {
        struct LeakyError: Error, CustomStringConvertible {
            var description: String { "token=sk-synthetic-credential-0000 body=synthetic-canary roof deposit" }
        }
        center.addError = LeakyError()
        let request = try genericRequest()

        do {
            _ = try await bridge.schedule(request)
            XCTFail("expected a failure")
        } catch let error as NotificationBridgeError {
            XCTAssertEqual(error, .centerFailed)
            XCTAssertEqual(error.errorClass, .transient)
            XCTAssertFalse(error.description.contains("sk-synthetic"))
            XCTAssertFalse(error.message.contains("canary"))
        }

        center.addError = nil
        center.pendingReadError = LeakyError()
        await assertFailure(.centerFailed, .transient) { _ = try await self.bridge.pendingNotifications() }
    }

    func testCancelledTaskIsReportedAsCancelled() async throws {
        let request = try genericRequest()
        let task = Task { () -> NotificationBridgeError? in
            while !Task.isCancelled { await Task.yield() }
            do {
                _ = try await self.bridge.schedule(request)
                return nil
            } catch {
                return error as? NotificationBridgeError
            }
        }
        task.cancel()
        let failure = await task.value
        XCTAssertEqual(failure, .cancelled)
        XCTAssertEqual(failure?.errorClass, .cancelled)
    }

    // MARK: Payload kinds

    func testPreviewPayloadWithoutAValidApprovalIsRejected() async throws {
        let route = try XCTUnwrap(OpaqueIdentifier("route-preview-safe"))
        let missingApproval = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .previewApproved, opaqueTargetID: try target(), previewText: "Call the dentist")
        let missingText = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .previewApproved, opaqueTargetID: try target(),
            previewApproval: PreviewApprovalReference(routeID: route, policyVersion: 1))
        let invalidVersion = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .previewApproved, opaqueTargetID: try target(), previewText: "Call the dentist",
            previewApproval: PreviewApprovalReference(routeID: route, policyVersion: 0))

        for request in [missingApproval, missingText, invalidVersion] {
            await assertFailure(.previewApprovalRequired, .permanent) { _ = try await self.bridge.schedule(request) }
        }
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testApprovedPreviewIsNotEnabledSoNoItemTextReachesTheOS() async throws {
        let route = try XCTUnwrap(OpaqueIdentifier("route-preview-safe"))
        let request = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .previewApproved, opaqueTargetID: try target(), previewText: "Call the dentist",
            previewApproval: PreviewApprovalReference(routeID: route, policyVersion: 1))

        await assertFailure(.previewNotEnabled, .unsupported) { _ = try await self.bridge.schedule(request) }
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    func testGenericPayloadCarryingPreviewTextIsRejectedNotStripped() async throws {
        let route = try XCTUnwrap(OpaqueIdentifier("route-preview-safe"))
        let withText = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .generic, opaqueTargetID: try target(), previewText: "Call the dentist")
        let withApproval = NotificationScheduleRequest(
            identifier: try identifier(), dueInstant: currentTime.addingTimeInterval(3600),
            payloadKind: .generic, opaqueTargetID: try target(),
            previewApproval: PreviewApprovalReference(routeID: route, policyVersion: 1))

        for request in [withText, withApproval] {
            await assertFailure(.invalidPayload, .permanent) { _ = try await self.bridge.schedule(request) }
        }
        XCTAssertTrue(center.addedRequests.isEmpty)
    }

    // MARK: Cancel

    func testCancelRemovesThePendingRequestAndIsIdempotent() async throws {
        let first = try identifier("reminder-1")
        let second = try identifier("reminder-2")
        _ = try await bridge.schedule(genericRequest(reminderID: "reminder-1"))
        _ = try await bridge.schedule(genericRequest(reminderID: "reminder-2"))

        try await bridge.cancel(first)
        XCTAssertEqual(center.pendingIdentifiers, [second.rawValue])
        try await bridge.cancel(first)
        try await bridge.cancel(try identifier("never-scheduled"))
        XCTAssertEqual(center.pendingIdentifiers, [second.rawValue])
    }

    func testCancelWaitsForAsynchronousRemoval() async throws {
        center.removalDelayPolls = 2
        let target = try identifier()
        _ = try await bridge.schedule(genericRequest())

        try await bridge.cancel(target)
        XCTAssertTrue(center.pendingIdentifiers.isEmpty)
    }

    func testCancelThatTheOSNeverCompletesIsATransientFailure() async throws {
        center.removalDelayPolls = 100
        let target = try identifier()
        _ = try await bridge.schedule(genericRequest())

        await assertFailure(.cancelNotConfirmed, .transient) { try await self.bridge.cancel(target) }
    }

    // MARK: List

    func testPendingListIsSoonestFirstAndIgnoresRequestsWithoutCoreIdentifiers() async throws {
        _ = try await bridge.schedule(genericRequest(reminderID: "reminder-late", item: "item-late", dueIn: 7200))
        _ = try await bridge.schedule(genericRequest(reminderID: "reminder-soon", item: "item-soon", dueIn: 600))
        center.installForeignRequest(identifier: "not a core identifier", dueInstant: currentTime.addingTimeInterval(60))
        center.installForeignRequest(identifier: "reminder-3#007", dueInstant: currentTime.addingTimeInterval(60))

        let pending = try await bridge.pendingNotifications()

        XCTAssertEqual(pending.map(\.identifier.rawValue), ["reminder-soon#1", "reminder-late#1"])
        XCTAssertEqual(pending.map { $0.opaqueTargetID?.rawValue }, ["item-soon", "item-late"])
        XCTAssertEqual(pending.map(\.dueInstant), [currentTime.addingTimeInterval(600), currentTime.addingTimeInterval(7200)])
    }

    // MARK: Delivered evidence

    func testDeliveredListIsForwardedAsDeliveredEventsKeyedByCoreIdentifier() async throws {
        center.delivered = [
            NotificationCenterDeliveredNotification(
                identifier: "reminder-1#2", deliveredAt: currentTime.addingTimeInterval(-30),
                userInfo: ["ohand.target": "item-1", "ohand.kind": "generic"]),
            NotificationCenterDeliveredNotification(
                identifier: "someone-elses-notification", deliveredAt: currentTime, userInfo: [:]),
        ]

        let forwarded = await bridge.ingestDeliveredNotifications()

        XCTAssertEqual(forwarded, 1)
        let event = try XCTUnwrap(recorder.events.first)
        XCTAssertEqual(recorder.events.count, 1)
        XCTAssertEqual(event.identifier, try identifier(generation: 2))
        XCTAssertEqual(event.kind, .delivered)
        XCTAssertEqual(event.opaqueTargetID?.rawValue, "item-1")
        XCTAssertEqual(event.occurredAt, currentTime.addingTimeInterval(-30))
    }

    // MARK: Helpers

    private func assertFailure(
        _ expected: NotificationBridgeError,
        _ expectedClass: NotificationErrorClass,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("expected \(expected.code)", file: file, line: line)
        } catch let error as NotificationBridgeError {
            XCTAssertEqual(error, expected, file: file, line: line)
            XCTAssertEqual(error.errorClass, expectedClass, file: file, line: line)
        } catch {
            XCTFail("the failure was not normalized: \(type(of: error))", file: file, line: line)
        }
    }
}
