import XCTest
@testable import OhAndServices

/// Every OS call must be cancellable and time-bounded even when the provider ignores
/// cancellation or never answers. The gated center suspends without honoring cancellation.
final class NotificationBridgeBoundednessTests: XCTestCase {
    private let currentTime = Date(timeIntervalSince1970: 1_800_000_000)
    private var inner = FakeNotificationCenter()
    private var center: SuspendingNotificationCenter!
    private var bridge: NotificationBridge!
    private var gates: [SuspensionGate] = []

    override func setUp() {
        super.setUp()
        inner = FakeNotificationCenter()
        center = SuspendingNotificationCenter(inner: inner)
        gates = []
        bridge = makeBridge(effectTimeout: 30)
    }

    override func tearDown() {
        gates.forEach { $0.release() }
        super.tearDown()
    }

    private func makeBridge(effectTimeout: TimeInterval) -> NotificationBridge {
        let now = currentTime
        return NotificationBridge(
            center: center,
            ingestor: NotificationEventIngestor(now: { now }, handler: { _ in }),
            now: { now },
            removalPollInterval: 0,
            maximumRemovalPolls: 5,
            effectTimeout: effectTimeout
        )
    }

    private func newGate() -> SuspensionGate {
        let gate = SuspensionGate()
        gates.append(gate)
        return gate
    }

    private func identifier() throws -> NotificationIdentifier {
        try XCTUnwrap(NotificationIdentifier(reminderID: "reminder-1", scheduleGeneration: 1))
    }

    private func request() throws -> NotificationScheduleRequest {
        .generic(
            identifier: try identifier(),
            dueInstant: currentTime.addingTimeInterval(3600),
            opaqueTargetID: try XCTUnwrap(OpaqueIdentifier("item-1"))
        )
    }

    private func eventually(
        _ description: String,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ condition: () -> Bool
    ) async {
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for: \(description)", file: file, line: line)
    }

    /// Starts `operation`, waits until it is suspended inside the gate, cancels it and returns
    /// the normalized failure it reports while the provider is still suspended.
    private func failureAfterCancellingWhileSuspended(
        in gate: SuspensionGate,
        _ operation: @escaping @Sendable () async throws -> Void
    ) async -> NotificationBridgeError? {
        let task = Task { () -> NotificationBridgeError? in
            do {
                try await operation()
                return nil
            } catch {
                return error as? NotificationBridgeError
            }
        }
        await eventually("the call to reach the provider") { gate.arrivals >= 1 }
        task.cancel()
        return await task.value
    }

    private func failureOfHungCall(_ operation: @escaping @Sendable () async throws -> Void) async -> NotificationBridgeError? {
        do {
            try await operation()
            return nil
        } catch {
            return error as? NotificationBridgeError
        }
    }

    // MARK: Cancellation

    func testCancellationWhileAddIsSuspendedIsCancelledAndTheLateInstallIsRemoved() async throws {
        let gate = newGate()
        center.addGate = gate
        let scheduleRequest = try request()
        let bridge = self.bridge!

        let failure = await failureAfterCancellingWhileSuspended(in: gate) {
            _ = try await bridge.schedule(scheduleRequest)
        }
        XCTAssertEqual(failure, .cancelled, "a cancelled schedule never reports success")
        XCTAssertTrue(inner.pendingIdentifiers.isEmpty, "nothing was installed yet")

        gate.release()
        await eventually("the late install to be added") { !self.inner.addedRequests.isEmpty }
        await eventually("the late install to be removed again") { self.inner.pendingIdentifiers.isEmpty }
        XCTAssertEqual(inner.removedIdentifiers, ["reminder-1#1"])
    }

    func testCancellationWhileReadBackIsSuspendedRemovesTheInstalledRequest() async throws {
        let gate = newGate()
        center.pendingGate = gate
        let scheduleRequest = try request()
        let bridge = self.bridge!

        let failure = await failureAfterCancellingWhileSuspended(in: gate) {
            _ = try await bridge.schedule(scheduleRequest)
        }
        XCTAssertEqual(failure, .cancelled)
        await eventually("the installed request to be rolled back") { self.inner.pendingIdentifiers.isEmpty }
        XCTAssertEqual(inner.removedIdentifiers, ["reminder-1#1"])
    }

    func testCancellationWhileOtherCallsAreSuspendedIsCancelled() async throws {
        let target = try identifier()
        let bridge = self.bridge!

        let authorizationGate = newGate()
        center.authorizationGate = authorizationGate
        let scheduleRequest = try request()
        let whileAuthorizing = await failureAfterCancellingWhileSuspended(in: authorizationGate) {
            _ = try await bridge.schedule(scheduleRequest)
        }
        XCTAssertEqual(whileAuthorizing, .cancelled)
        XCTAssertTrue(inner.addedRequests.isEmpty, "a cancelled schedule never reaches add")

        let pendingGate = newGate()
        center.pendingGate = pendingGate
        let whileListing = await failureAfterCancellingWhileSuspended(in: pendingGate) {
            _ = try await bridge.pendingNotifications()
        }
        XCTAssertEqual(whileListing, .cancelled)

        let removeGate = newGate()
        center.removeGate = removeGate
        let whileRemoving = await failureAfterCancellingWhileSuspended(in: removeGate) {
            try await bridge.cancel(target)
        }
        XCTAssertEqual(whileRemoving, .cancelled)

        let deliveredGate = newGate()
        center.deliveredGate = deliveredGate
        let whileReadingDelivered = await failureAfterCancellingWhileSuspended(in: deliveredGate) {
            _ = try await bridge.ingestDeliveredNotifications()
        }
        XCTAssertEqual(whileReadingDelivered, .cancelled)
    }

    // MARK: Deadlines

    func testAHungProviderTimesOutWithANormalizedTransientError() async throws {
        let target = try identifier()
        let scheduleRequest = try request()
        let quick = makeBridge(effectTimeout: 0.05)

        center.authorizationGate = newGate()
        let hungAuthorization = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(hungAuthorization, .timedOut)
        XCTAssertEqual(hungAuthorization?.errorClass, .transient)
        center.authorizationGate = nil

        center.addGate = newGate()
        let hungAdd = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(hungAdd, .timedOut)
        center.addGate = nil

        center.pendingGate = newGate()
        let hungListing = await failureOfHungCall { _ = try await quick.pendingNotifications() }
        XCTAssertEqual(hungListing, .timedOut)
        let hungCancelPoll = await failureOfHungCall { try await quick.cancel(target) }
        XCTAssertEqual(hungCancelPoll, .timedOut)
        center.pendingGate = nil

        center.removeGate = newGate()
        let hungRemoval = await failureOfHungCall { try await quick.cancel(target) }
        XCTAssertEqual(hungRemoval, .timedOut)
        center.removeGate = nil

        center.deliveredGate = newGate()
        let hungDelivered = await failureOfHungCall { _ = try await quick.ingestDeliveredNotifications() }
        XCTAssertEqual(hungDelivered, .timedOut)
    }

    func testAScheduleThatTimesOutInAddIsRemovedWhenTheOSLaterCompletesIt() async throws {
        let gate = newGate()
        center.addGate = gate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let failure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(failure, .timedOut)

        gate.release()
        await eventually("the late install to be added") { !self.inner.addedRequests.isEmpty }
        await eventually("the late install to be removed again") { self.inner.pendingIdentifiers.isEmpty }
    }

    func testAProviderThatAnswersInTimeIsNotAffectedByTheDeadline() async throws {
        let installed = try await bridge.schedule(request())
        XCTAssertEqual(installed.identifier, try identifier())
        try await bridge.cancel(try identifier())
        XCTAssertTrue(inner.pendingIdentifiers.isEmpty)
    }
}
