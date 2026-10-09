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
        file: StaticString = #filePath,
        line: UInt = #line,
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
        await eventually("the call to reach the provider", file: file, line: line) { gate.arrivals >= 1 }
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

        // Cancel reads the pending list before removing, so only the removal may stay suspended.
        center.pendingGate = nil
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

    func testARetryThatSucceedsWhileARemovalUndoIsSuspendedSurvivesTheLateRemoval() async throws {
        let addGate = newGate()
        center.addGate = addGate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let failure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(failure, .timedOut)

        let removeGate = newGate()
        center.removeGate = removeGate
        center.addGate = nil
        addGate.release()
        await eventually("the undo to block in removal") { removeGate.arrivals >= 1 }

        let retried = try await quick.schedule(scheduleRequest)
        XCTAssertEqual(retried.identifier, try identifier())
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"])

        center.removeGate = nil
        removeGate.release()
        await eventually("the suspended removal to land late") { !self.inner.removedIdentifiers.isEmpty }
        await letLateWorkRun(quick)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"], "the retry's confirmed install survives")
        let survivingDue = try await pendingDueInstant()
        XCTAssertEqual(survivingDue, scheduleRequest.dueInstant)
        XCTAssertTrue(quick.unreconciledIdentifiers.isEmpty)
    }

    /// An abandoned schedule's undo removal times out and stays suspended. A retry then installs
    /// the identifier and reads the list back; the suspended removal lands after that read took
    /// its snapshot but before the retry uses it. The retry reports success, so the request must
    /// end up pending, not removed by work the retry had already superseded.
    func testALateRemovalLandingDuringTheRetrysConfirmingReadDoesNotLeaveTheRequestAbsent() async throws {
        let addGate = newGate()
        center.addGate = addGate
        let quick = makeBridge(effectTimeout: 0.05)
        let scheduleRequest = try request()

        let failure = await failureOfHungCall { _ = try await quick.schedule(scheduleRequest) }
        XCTAssertEqual(failure, .timedOut)

        let removeGate = newGate()
        center.removeGate = removeGate
        center.addGate = nil
        addGate.release()
        await eventually("the undo to block in removal") { removeGate.arrivals >= 1 }
        await quick.settleAbandonedWork()
        XCTAssertEqual(quick.unreconciledIdentifiers, ["reminder-1#1"], "the undo timed out and is remembered")

        // The first schedule read the list once; the retry reads it before its add and again to
        // confirm. The confirming read answers from a snapshot taken before the removal lands.
        let inner = self.inner
        center.pendingCallsBeforeGate = 2
        center.afterStaleSnapshot = {
            removeGate.release()
            while inner.removedIdentifiers.isEmpty {
                try? await Task.sleep(nanoseconds: 1_000_000)
            }
        }
        let retried = try await quick.schedule(scheduleRequest)
        XCTAssertEqual(retried.identifier, try identifier())
        XCTAssertEqual(inner.removedIdentifiers, ["reminder-1#1"], "the removal landed during the confirming read")
        XCTAssertNil(center.afterStaleSnapshot, "the stale read happened")

        await letLateWorkRun(quick)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"], "a reported install is pending")
        let survivingDue = try await pendingDueInstant()
        XCTAssertEqual(survivingDue, scheduleRequest.dueInstant)
        XCTAssertTrue(quick.unreconciledIdentifiers.isEmpty)
    }

    func testANewerScheduleThatSucceedsWhileARestoreUndoIsSuspendedSurvivesTheLateRestore() async throws {
        let quick = makeBridge(effectTimeout: 0.05)
        let original = try request()
        _ = try await quick.schedule(original)

        let abandoned = NotificationScheduleRequest.generic(
            identifier: original.identifier,
            dueInstant: original.dueInstant.addingTimeInterval(600),
            opaqueTargetID: original.opaqueTargetID
        )
        let abandonedAddGate = newGate()
        center.addGate = abandonedAddGate
        let failure = await failureOfHungCall { _ = try await quick.schedule(abandoned) }
        XCTAssertEqual(failure, .timedOut)

        let restoreGate = newGate()
        center.addGate = restoreGate
        abandonedAddGate.release()
        await eventually("the restore of the earlier request to block in add") { restoreGate.arrivals >= 1 }

        center.addGate = nil
        let newer = NotificationScheduleRequest.generic(
            identifier: original.identifier,
            dueInstant: original.dueInstant.addingTimeInterval(1200),
            opaqueTargetID: original.opaqueTargetID
        )
        _ = try await quick.schedule(newer)
        let dueAfterNewer = try await pendingDueInstant()
        XCTAssertEqual(dueAfterNewer, newer.dueInstant)

        restoreGate.release()
        // Adds so far: the original, the abandoned one landing late and the newer schedule.
        await eventually("the suspended restore to land late") { self.inner.addedRequests.count >= 4 }
        await letLateWorkRun(quick)
        let finalDue = try await pendingDueInstant()
        XCTAssertEqual(finalDue, newer.dueInstant, "the newer schedule's confirmed install survives")
        XCTAssertTrue(quick.unreconciledIdentifiers.isEmpty)
    }

    func testACancelCancelledWhileRemovalIsSuspendedLeavesTheRequestPending() async throws {
        let original = try request()
        _ = try await bridge.schedule(original)

        let removeGate = newGate()
        center.removeGate = removeGate
        let bridge = self.bridge!
        let target = try identifier()
        let failure = await failureAfterCancellingWhileSuspended(in: removeGate) {
            try await bridge.cancel(target)
        }
        XCTAssertEqual(failure, .cancelled)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"])

        center.removeGate = nil
        removeGate.release()
        await eventually("the suspended removal to land late") { !self.inner.removedIdentifiers.isEmpty }
        await letLateWorkRun(bridge)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"], "a cancelled cancel removes nothing")
        let survivingDue = try await pendingDueInstant()
        XCTAssertEqual(survivingDue, original.dueInstant)
        XCTAssertTrue(bridge.unreconciledIdentifiers.isEmpty)
    }

    func testACancelCancelledWhileConfirmingTheRemovalPutsTheRequestBack() async throws {
        let original = try request()
        _ = try await bridge.schedule(original)

        // Scheduling read the list twice; the cancel reads it once before removing, then polls.
        let pollGate = newGate()
        center.pendingCallsBeforeGate = 3
        center.pendingGate = pollGate
        let bridge = self.bridge!
        let target = try identifier()
        let failure = await failureAfterCancellingWhileSuspended(in: pollGate) {
            try await bridge.cancel(target)
        }
        XCTAssertEqual(failure, .cancelled)
        XCTAssertEqual(inner.removedIdentifiers, ["reminder-1#1"], "the removal had already happened")

        pollGate.release()
        await letLateWorkRun(bridge)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"], "a cancelled cancel leaves no removal behind")
        let restoredDue = try await pendingDueInstant()
        XCTAssertEqual(restoredDue, original.dueInstant)
        XCTAssertTrue(bridge.unreconciledIdentifiers.isEmpty)
    }

    func testACancelThatTimesOutInRemovalLeavesTheRequestPendingWhenTheRemovalLandsLate() async throws {
        let quick = makeBridge(effectTimeout: 0.05)
        let original = try request()
        _ = try await quick.schedule(original)

        let removeGate = newGate()
        center.removeGate = removeGate
        let target = try identifier()
        let failure = await failureOfHungCall { try await quick.cancel(target) }
        XCTAssertEqual(failure, .timedOut)

        center.removeGate = nil
        removeGate.release()
        await eventually("the suspended removal to land late") { !self.inner.removedIdentifiers.isEmpty }
        await letLateWorkRun(quick)
        XCTAssertEqual(inner.pendingIdentifiers, ["reminder-1#1"])
        let restoredDue = try await pendingDueInstant()
        XCTAssertEqual(restoredDue, original.dueInstant)
        XCTAssertTrue(quick.unreconciledIdentifiers.isEmpty)
    }

    /// A provider that finishes its removal as soon as the bridge cancels it lands the removal
    /// before the abandoned cancel records the earlier state; the late completion's own check can
    /// then run before or after that record. Several rounds exercise both orders, for a cancelled
    /// and a timed-out cancel, and the earlier request must survive every one.
    func testACancelWhoseRemovalLandsWhenAbandonedStillPutsTheRequestBack() async throws {
        let now = currentTime
        for round in 0..<10 {
            for timesOut in [false, true] {
                let roundInner = FakeNotificationCenter()
                let roundCenter = SuspendingNotificationCenter(inner: roundInner)
                let roundBridge = NotificationBridge(
                    center: roundCenter,
                    ingestor: NotificationEventIngestor(now: { now }, handler: { _ in }),
                    now: { now },
                    removalPollInterval: 0,
                    maximumRemovalPolls: 5,
                    effectTimeout: timesOut ? 0.05 : 30
                )
                let original = try request()
                _ = try await roundBridge.schedule(original)

                let removeGate = newGate()
                roundCenter.removeGate = removeGate
                roundCenter.removalLandsWhenCancelled = true
                let target = try identifier()
                let failure: NotificationBridgeError?
                if timesOut {
                    failure = await failureOfHungCall { try await roundBridge.cancel(target) }
                } else {
                    failure = await failureAfterCancellingWhileSuspended(in: removeGate) {
                        try await roundBridge.cancel(target)
                    }
                }
                let context = "round \(round), \(timesOut ? "timed out" : "cancelled")"
                XCTAssertEqual(failure, timesOut ? .timedOut : .cancelled, context)
                XCTAssertEqual(roundInner.removedIdentifiers, ["reminder-1#1"], "the removal landed: \(context)")

                roundCenter.removeGate = nil
                await eventually("the earlier request to be put back: \(context)") {
                    roundInner.pendingIdentifiers == ["reminder-1#1"]
                }
                await letLateWorkRun(roundBridge)
                XCTAssertEqual(roundInner.pendingIdentifiers, ["reminder-1#1"], context)
                let restoredDue = try await roundInner.pendingRequests()
                    .first(where: { $0.identifier == "reminder-1#1" })?.dueInstant
                XCTAssertEqual(restoredDue, original.dueInstant, context)
                XCTAssertTrue(roundBridge.unreconciledIdentifiers.isEmpty, context)
            }
        }
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
