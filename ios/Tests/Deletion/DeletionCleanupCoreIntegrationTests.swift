import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives `DeletionCleanupService` through the production `CoreDeletionCleanupClient` into the real Rust core over a
/// file store, with real protected-storage files and the production `NotificationBridge` over a fake OS center. An
/// interruption is a fault at one step (or the core confirming a change that the caller never hears about); "reopening"
/// closes the core and opens a new one over the same disk, with a new client and service, so nothing but the files and the
/// notification center survives, as after a kill. The deletion intent itself is seeded (see `RealCoreDeletionWorld`).
final class DeletionCleanupCoreIntegrationTests: XCTestCase {
    private let captureID = "cap-1"
    private let reminderID = "rem-1"
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var worlds: [RealCoreDeletionWorld] = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDown() {
        worlds.forEach { $0.tearDown() }
        worlds = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: Helpers

    private struct Seeded {
        let world: RealCoreDeletionWorld
        let itemID: String
        let bystanderItemID: String
    }

    /// A deleted item with audio, a staged record, an interrupted record write and two notification generations, next to
    /// an untouched item with its own files and notification.
    private func seedDeletedItem() throws -> Seeded {
        let world = try RealCoreDeletionWorld()
        worlds.append(world)
        let itemID = try world.importVoiceCapture(captureID: captureID, interruptedRecordWrite: true)
        let bystanderItemID = try world.importVoiceCapture(captureID: "cap-2")
        try world.seedDeletedState(itemID: itemID, captureID: captureID, reminderID: reminderID)

        let setup = try world.openCore()
        defer { setup.close() }
        try world.installNotifications(itemID: itemID, reminderID: reminderID, generations: [1, 2], on: setup)
        try world.installNotifications(itemID: bystanderItemID, reminderID: "rem-2", generations: [1], on: setup)
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: reminderID), ["rem-1#1", "rem-1#2"])
        return Seeded(world: world, itemID: itemID, bystanderItemID: bystanderItemID)
    }

    private func reconcile(_ seeded: Seeded, on session: RealCoreSession) throws -> DeletionReconcileReport {
        let service = seeded.world.makeService(on: session)
        return try session.run { await service.reconcile() }
    }

    private func cleanUp(_ seeded: Seeded, on session: RealCoreSession) throws -> DeletionItemResult {
        let service = seeded.world.makeService(on: session)
        let itemID = seeded.itemID
        return try session.run { await service.cleanUp(itemID: itemID) }
    }

    private func assertConverged(_ seeded: Seeded, file: StaticString = #filePath, line: UInt = #line) throws {
        let world = seeded.world
        XCTAssertEqual(world.remnants(of: captureID), [], "no file of the deleted capture remains", file: file, line: line)
        XCTAssertEqual(
            world.pendingNotificationIdentifiers(ofReminder: reminderID), [],
            "no notification of the deleted item stays active", file: file, line: line)
        XCTAssertEqual(
            try world.workStatuses(itemID: seeded.itemID),
            ["remove_audio": "completed", "clear_ingress": "completed", "cancel_notifications": "completed"],
            "the core records every cleanup row as completed", file: file, line: line)
        XCTAssertNil(
            try world.audioReference(captureID: captureID), "the core released the audio reference", file: file, line: line)
        XCTAssertEqual(
            try world.cancelOperationStates(reminderID: reminderID), ["rem-1#1": "acknowledged", "rem-1#2": "acknowledged"],
            "every cancel the core issued is recorded", file: file, line: line)

        XCTAssertTrue(world.exists(.finalizedAudio, "cap-2.m4a"), "another item's audio is untouched", file: file, line: line)
        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-2.json"), file: file, line: line)
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: "rem-2"), ["rem-2#1"], file: file, line: line)
        XCTAssertEqual(try world.workStatuses(itemID: seeded.bystanderItemID), [:], file: file, line: line)
    }

    // MARK: Reads through the production client

    func testTheClientReadsTheQueueTheCoreHolds() throws {
        let seeded = try seedDeletedItem()
        let session = try seeded.world.openCore()
        let client = session.client

        XCTAssertEqual(try session.run { try await client.itemsWithPendingCleanup() }, [seeded.itemID])
        let work = try session.run { try await client.pendingCleanup(itemID: seeded.itemID) }
        XCTAssertEqual(Set(work.map { $0.kind }), Set(DeletionCleanupKind.allCases))
        XCTAssertTrue(work.allSatisfy { $0.itemID == seeded.itemID })
        XCTAssertEqual(
            try session.run { try await client.cleanupTarget(itemID: seeded.itemID) },
            DeletionCleanupTarget(captureID: captureID, audioReference: "FinalizedAudio/cap-1.m4a"))
        let identifiers = try session.run { try await client.reminderNotificationIdentifiers(itemID: seeded.itemID) }
        XCTAssertEqual(identifiers.map { $0.rawValue }.sorted(), ["rem-1#1", "rem-1#2"])
        XCTAssertEqual(try session.run { try await client.pendingCleanup(itemID: seeded.bystanderItemID) }, [])
        XCTAssertNil(try session.run { try await client.cleanupTarget(itemID: "item-unknown") })
        session.close()
    }

    func testTheCoreRefusesToCompleteTheNotificationRowWhileACancelIsUnrecorded() throws {
        let seeded = try seedDeletedItem()
        let session = try seeded.world.openCore()
        let client = session.client
        let work = try session.run { try await client.pendingCleanup(itemID: seeded.itemID) }
        let notificationRow = try XCTUnwrap(work.first { $0.kind == .cancelNotifications })

        XCTAssertThrowsError(try session.run { try await client.completeCleanup(workID: notificationRow.workID) }) { error in
            XCTAssertEqual((error as? CoreFailure)?.code, "deletion_work_refused")
        }
        XCTAssertEqual(try seeded.world.workStatuses(itemID: seeded.itemID)["cancel_notifications"], "pending")

        for identifier in try session.run({ try await client.reminderNotificationIdentifiers(itemID: seeded.itemID) }) {
            try session.run { try await client.recordNotificationCancelled(identifier) }
        }
        try session.run { try await client.completeCleanup(workID: notificationRow.workID) }
        XCTAssertEqual(try seeded.world.workStatuses(itemID: seeded.itemID)["cancel_notifications"], "completed")
        session.close()
    }

    func testAShutDownClientFailsCallsInsteadOfWaiting() throws {
        let seeded = try seedDeletedItem()
        let session = try seeded.world.openCore()
        let client = session.client
        client.shutDown()

        XCTAssertThrowsError(try session.run { try await client.itemsWithPendingCleanup() }) { error in
            XCTAssertEqual(error as? CoreFailure, CoreDeletionCleanupClient.shutDownFailure)
        }
        session.close()
    }

    // MARK: Cleanup and interruptions

    func testCleanupThroughTheRealCoreRemovesEveryEffectAndRecordsCompletion() throws {
        let seeded = try seedDeletedItem()
        let session = try seeded.world.openCore()

        let report = try reconcile(seeded, on: session)

        XCTAssertTrue(report.converged)
        XCTAssertEqual(report.items.map { $0.itemID }, [seeded.itemID])
        XCTAssertEqual(
            report.items.first?.steps,
            [
                DeletionStepResult(kind: .cancelNotifications, outcome: .completed),
                DeletionStepResult(kind: .clearIngress, outcome: .completed),
                DeletionStepResult(kind: .removeAudio, outcome: .completed),
            ])
        try assertConverged(seeded)

        let repeated = try reconcile(seeded, on: session)
        XCTAssertEqual(repeated, DeletionReconcileReport(listingFailure: nil, items: []), "a converged store has no work")
        session.close()
    }

    func testATerminalFailureStaysVisibleAcrossPassesAndReopens() throws {
        let seeded = try seedDeletedItem()
        try seeded.world.setAudioReference(captureID: captureID, to: "Elsewhere/cap-1.m4a")
        var session = try seeded.world.openCore()

        let first = try reconcile(seeded, on: session)

        XCTAssertFalse(first.converged)
        XCTAssertEqual(first.items.first?.steps.last, DeletionStepResult(kind: .removeAudio, outcome: .failedPermanently(.audioReferenceUnresolvable)))
        XCTAssertEqual(first.terminalFailures, [DeletionTerminalFailure(itemID: seeded.itemID, kind: .removeAudio)])
        XCTAssertEqual(try seeded.world.workStatuses(itemID: seeded.itemID)["remove_audio"], "failed_no_retry")

        for _ in 0..<2 {
            let again = try reconcile(seeded, on: session)
            XCTAssertEqual(again.items, [], "a terminal failure is not retried")
            XCTAssertEqual(again.terminalFailures, first.terminalFailures, "the durable failure stays visible")
            XCTAssertFalse(again.converged, "a durable failure never reports convergence")
            session.close()
            session = try seeded.world.openCore()
        }
        session.close()
    }

    func testRepeatedCleanUpAfterATerminalFailureIsNeverSettledAcrossReopens() throws {
        let seeded = try seedDeletedItem()
        try seeded.world.setAudioReference(captureID: captureID, to: "Elsewhere/cap-1.m4a")
        var session = try seeded.world.openCore()

        let first = try cleanUp(seeded, on: session)

        XCTAssertFalse(first.isSettled)
        XCTAssertEqual(first.terminalFailures, [.removeAudio])
        XCTAssertEqual(try seeded.world.workStatuses(itemID: seeded.itemID)["remove_audio"], "failed_no_retry")

        for _ in 0..<2 {
            let again = try cleanUp(seeded, on: session)
            XCTAssertEqual(again.steps, [], "a terminal failure is not retried")
            XCTAssertEqual(again.terminalFailures, [.removeAudio], "the durable failure stays visible")
            XCTAssertFalse(again.isSettled, "a durable failure is never settled")
            session.close()
            session = try seeded.world.openCore()
        }
        session.close()
    }

    private static let interruptionPoints = [
        "core.itemsWithPendingCleanup",
        "core.pendingCleanup",
        "core.reminderIdentifiers",
        "notifier.cancel",
        "notifier.cancel.afterEffect",
        "core.recordCancelled",
        "core.recordCancelled.afterEffect",
        "notifier.pending",
        "core.complete.cancel_notifications",
        "core.complete.cancel_notifications.afterEffect",
        "core.cleanupTarget",
        "fs.read:cap-1.json",
        "fs.remove:cap-1-recording.m4a",
        "fs.remove:cap-1.m4a",
        "fs.remove:cap-1.json.incomplete",
        "fs.remove:cap-1.json",
        "core.complete.clear_ingress",
        "core.complete.clear_ingress.afterEffect",
        "fs.entryNames",
        "core.complete.remove_audio",
        "core.complete.remove_audio.afterEffect",
        "core.terminalFailures",
    ]

    func testReopeningTheCoreAfterEachInterruptionConvergesWithoutRemnants() throws {
        for point in Self.interruptionPoints {
            let seeded = try seedDeletedItem()
            seeded.world.faults.arm(point)

            let interrupted = try seeded.world.openCore()
            let interruptedReport = try reconcile(seeded, on: interrupted)
            interrupted.close()

            XCTAssertEqual(seeded.world.faults.firedPoints, [point], "the interruption at \(point) happened")
            XCTAssertFalse(interruptedReport.converged, "an interrupted pass is not reported as converged: \(point)")

            let reopened = try seeded.world.openCore()
            let reopenedReport = try reconcile(seeded, on: reopened)
            reopened.close()

            XCTAssertTrue(reopenedReport.converged, "reopen converges after \(point)")
            try assertConverged(seeded)
            worlds.removeLast().tearDown()
        }
    }
}
