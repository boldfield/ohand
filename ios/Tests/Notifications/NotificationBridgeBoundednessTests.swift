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
        center.pendingCallsBeforeGate = 1
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

    // MARK: Abandoned work must not disturb newer or earlier state

    private func pendingDueInstant() async throws -> Date? {
        try await inner.pendingRequests().first(where: { $0.identifier == "reminder-1#1" })?.dueInstant
    }

    /// Lets a late completion's follow-up work, if any wrongly starts, run before asserting.
    private func letLateWorkRun(_ bridge: NotificationBridge) async {
        try? await Task.sleep(nanoseconds: 150_000_000)
        await bridge.settleAbandonedWork()
    }

    func testALateInstallFromATimedOutScheduleDoesNotRemoveTheRetrysInstall() async throws {
        let gate = newGate()
        center.addGate = gate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let firstFailure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(firstFailure, .timedOut)

        center.addGate = nil
        let retried = try await quick.schedule(scheduleRequest)
        XCTAssertEqual(retried.identifier, try identifier())

        gate.release()
        await eventually("the first add to finish late") { self.inner.addedRequests.count == 2 }
        await letLateWorkRun(quick)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"], "the retry's confirmed install survives")
        XCTAssertTrue(inner.removedIdentifiers.isEmpty)
    }

    func testAnAbandonedRetryOfAnAlreadyPendingIdentifierLeavesTheRequestPending() async throws {
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()
        _ = try await quick.schedule(scheduleRequest)

        let gate = newGate()
        center.addGate = gate
        let bridge = self.bridge!
        let failure = await failureAfterCancellingWhileSuspended(in: gate) {
            _ = try await bridge.schedule(scheduleRequest)
        }
        XCTAssertEqual(failure, .cancelled)

        gate.release()
        await eventually("the abandoned retry to finish late") { self.inner.addedRequests.count == 2 }
        await letLateWorkRun(bridge)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"])
        XCTAssertTrue(inner.removedIdentifiers.isEmpty, "the earlier desired request is never deleted")
    }

    func testAnAbandonedRetryThatReplacedAPendingRequestRestoresTheEarlierOne() async throws {
        let quick = makeBridge(effectTimeout: 0.05)
        let original = try request()
        _ = try await quick.schedule(original)

        let changed = NotificationScheduleRequest.generic(
            identifier: original.identifier,
            dueInstant: original.dueInstant.addingTimeInterval(600),
            opaqueTargetID: original.opaqueTargetID
        )
        let gate = newGate()
        center.addGate = gate
        let failure = await failureOfHungCall { _ = try await quick.schedule(changed) }
        XCTAssertEqual(failure, .timedOut)

        gate.release()
        await eventually("the abandoned retry to finish late and be undone") { self.inner.addedRequests.count == 3 }
        await quick.settleAbandonedWork()
        let restoredDue = try await pendingDueInstant()
        XCTAssertEqual(restoredDue, original.dueInstant)
        XCTAssertTrue(inner.removedIdentifiers.isEmpty)
    }

    func testALateInstallAfterANewerCancelIsRemoved() async throws {
        let gate = newGate()
        center.addGate = gate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let failure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(failure, .timedOut)
        try await quick.cancel(try identifier())

        gate.release()
        await eventually("the late install to be added") { !self.inner.addedRequests.isEmpty }
        await eventually("the cancelled identifier to be removed again") { self.inner.pendingIdentifiers.isEmpty }
    }

    func testAStuckUndoIsBoundedTrackedAndRetriedByReconcile() async throws {
        let addGate = newGate()
        center.addGate = addGate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let failure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(failure, .timedOut)

        let removeGate = newGate()
        center.removeGate = removeGate
        addGate.release()
        await eventually("the undo to reach the provider") { removeGate.arrivals >= 1 }
        await quick.settleAbandonedWork()
        XCTAssertEqual(quick.unreconciledIdentifiers, ["reminder-1#1"], "the undo timed out and is remembered")
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"])

        center.removeGate = nil
        try await quick.reconcile()
        XCTAssertTrue(inner.pendingIdentifiers.isEmpty)
        XCTAssertTrue(quick.unreconciledIdentifiers.isEmpty)
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
