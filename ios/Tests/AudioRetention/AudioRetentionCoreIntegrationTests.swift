import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives `AudioRetentionService` through the production `CoreAudioRetentionClient` into the real Rust core over a file
/// store, with real files in protected storage. The core decides from real `transcribed_at` times and the item's source
/// state; nothing here fakes a policy decision. "Reopening" closes the core and opens a new one over the same disk, with
/// a new client and service, so only the files survive, as after a kill.
final class AudioRetentionCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var worlds: [RealCoreRetentionWorld] = []

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

    private func makeWorld() throws -> RealCoreRetentionWorld {
        let world = try RealCoreRetentionWorld()
        worlds.append(world)
        return world
    }

    private func outcome(_ report: AudioRetentionSweepReport, _ itemID: String) -> AudioRemovalOutcome? {
        report.items.first { $0.item.itemID == itemID }?.removal
    }

    func testOnlyAudioThePolicyExpiresIsRemovedFromRealFiles() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let recent = try await world.seedVoice(captureID: "cap-recent", transcribedDaysAgo: 1)
        let untranscribed = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let session = try world.openCore()

        let report = await world.makeService(on: session).sweep()

        XCTAssertNil(report.listingFailure)
        XCTAssertEqual(outcome(report, expired.itemID), .removed)
        XCTAssertNil(outcome(report, recent.itemID))
        XCTAssertNil(outcome(report, untranscribed.itemID))
        XCTAssertTrue(report.converged)
        XCTAssertFalse(world.audioExists(expired.fileName))
        XCTAssertTrue(world.audioExists(recent.fileName))
        XCTAssertTrue(world.audioExists(untranscribed.fileName), "sole-source audio is never removed")
        XCTAssertEqual(try world.ledgerEntries().map(\.itemID), [expired.itemID])

        let recentReport = try XCTUnwrap(report.items.first { $0.item.itemID == recent.itemID })
        if case .retained = recentReport.item.status {} else { XCTFail("recent audio is retained: \(recentReport.item.status)") }
        let soleReport = try XCTUnwrap(report.items.first { $0.item.itemID == untranscribed.itemID })
        XCTAssertEqual(soleReport.item.status, .unresolved(.transcriptionPending))
        XCTAssertTrue(soleReport.item.options.contains(.deleteAudio))
        session.close()
    }

    func testAConfiguredPeriodMovesTheExpiry() async throws {
        let world = try makeWorld()
        let recent = try await world.seedVoice(captureID: "cap-recent", transcribedDaysAgo: 3)
        let session = try world.openCore()

        let defaultReport = await world.makeService(on: session).sweep()
        XCTAssertNil(outcome(defaultReport, recent.itemID))
        XCTAssertTrue(world.audioExists(recent.fileName))

        try await session.storeRetentionPeriod(seconds: 2 * 86_400)
        let shortened = await world.makeService(on: session).sweep()
        XCTAssertEqual(outcome(shortened, recent.itemID), .removed)
        XCTAssertFalse(world.audioExists(recent.fileName))
        session.close()
    }

    func testAStaleListingCannotRemoveAudioOfAnItemDeletedAfterTheSweep() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let session = try world.openCore()
        let service = world.makeService(
            on: session,
            core: HookedRetentionCore(wrapped: session.client, beforeRemoval: { itemID in
                world.markDeleted(itemID: itemID)
            }))

        let report = await service.sweep()
        XCTAssertEqual(outcome(report, expired.itemID), .denied(.deletionOwnsCleanup))
        XCTAssertTrue(world.audioExists(expired.fileName), "deletion cleanup owns this file, not retention")
        XCTAssertEqual(try world.ledgerEntries(), [])
        session.close()
    }

    func testAStaleListingCannotRemoveAudioWhoseReferenceChangedAfterTheSweep() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let session = try world.openCore()
        let replacement = RetentionFixtures.audioReference("cap-expired-replacement.m4a")
        let service = world.makeService(
            on: session,
            core: HookedRetentionCore(wrapped: session.client, beforeRemoval: { _ in
                world.setCaptureAudioReference(captureID: "cap-expired", to: replacement)
            }))

        let report = await service.sweep()
        XCTAssertEqual(outcome(report, expired.itemID), .denied(.audioReferenceMismatch))
        XCTAssertTrue(world.audioExists(expired.fileName))
        session.close()
    }

    func testAWriteThatWouldProtectApprovedAudioCannotCommitBeforeTheFileIsGone() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let session = try world.openCore()
        let attempts = RecordedValues<Int32>()
        let fileWasThere = RecordedValues<Bool>()
        let service = world.makeService(
            on: session,
            core: HookedRetentionCore(wrapped: session.client, duringRemoval: { itemID in
                fileWasThere.append(world.audioExists(expired.fileName))
                attempts.append(world.tryMarkDeleted(itemID: itemID))
            }))

        let report = await service.sweep()
        XCTAssertEqual(fileWasThere.all, [true], "the effect ran while the fence was held")
        XCTAssertEqual(attempts.all.count, 1)
        let status = try XCTUnwrap(attempts.all.first)
        XCTAssertTrue(
            status == SQLITE_BUSY || status == SQLITE_LOCKED,
            "a protecting write cannot commit between the approval and the file effect: \(status)")
        XCTAssertEqual(outcome(report, expired.itemID), .removed)
        XCTAssertFalse(world.audioExists(expired.fileName))
        XCTAssertTrue(report.converged)

        XCTAssertEqual(world.tryMarkDeleted(itemID: expired.itemID), SQLITE_OK, "the fence is released after the effect")
        let later = await world.makeService(on: session).sweep()
        XCTAssertNil(outcome(later, expired.itemID))
        session.close()
    }

    func testAnEffectThatStallsPastTheLeaseStillHoldsOffAProtectingWrite() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let session = try world.openCore()
        // Longer than the core's 3 second lease: an unpinned lease would expire and let the writer commit mid-effect.
        let stall: TimeInterval = 4.5
        let writerStatus = RecordedValues<Int32>()
        let fileWasThereAtCommit = RecordedValues<Bool>()
        let effectEnded = RecordedValues<Date>()
        let committedAt = RecordedValues<Date>()
        let service = world.makeService(
            on: session,
            core: HookedRetentionCore(wrapped: session.client, duringRemoval: { itemID in
                // A protecting write races the effect from a second connection, waiting longer than the stall.
                Thread.detachNewThread {
                    let status = world.tryMarkDeleted(itemID: itemID, busyMillis: 15_000)
                    committedAt.append(Date())
                    fileWasThereAtCommit.append(world.audioExists(expired.fileName))
                    writerStatus.append(status)
                }
                Thread.sleep(forTimeInterval: stall)
                effectEnded.append(Date())
            }))

        let report = await service.sweep()
        // Polled without blocking, so the test never waits on the lower-priority writer thread.
        let deadline = Date().addingTimeInterval(20)
        while writerStatus.all.isEmpty && Date() < deadline {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertEqual(writerStatus.all, [SQLITE_OK], "the writer was delayed, not refused")
        XCTAssertEqual(fileWasThereAtCommit.all, [false], "the protecting write committed only after the file was gone")
        let ended = try XCTUnwrap(effectEnded.all.first)
        let committed = try XCTUnwrap(committedAt.all.first)
        XCTAssertGreaterThanOrEqual(committed, ended, "the lease could not expire while the effect ran")
        XCTAssertEqual(outcome(report, expired.itemID), .removed, "the fence was held for the whole effect")
        XCTAssertTrue(report.converged)

        let later = await world.makeService(on: session).sweep()
        XCTAssertNil(outcome(later, expired.itemID), "the item is deleted now; its audio is deletion's")
        session.close()
    }

    func testInterruptionBeforeRemovalConvergesAfterReopeningTheCore() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)

        let firstSession = try world.openCore()
        let interrupting = HookedRetentionCore(wrapped: firstSession.client)
        interrupting.interruptNextRemoval()
        let interrupted = await world.makeService(on: firstSession, core: interrupting).sweep()
        XCTAssertEqual(outcome(interrupted, expired.itemID), .retryLater(.coreUnavailable))
        XCTAssertFalse(interrupted.converged)
        XCTAssertTrue(world.audioExists(expired.fileName))
        firstSession.close()

        let secondSession = try world.openCore()
        let converged = await world.makeService(on: secondSession).sweep()
        XCTAssertEqual(outcome(converged, expired.itemID), .removed)
        XCTAssertTrue(converged.converged)
        XCTAssertFalse(world.audioExists(expired.fileName))
        secondSession.close()
    }

    func testInterruptionAfterRemovalConvergesAfterReopeningTheCore() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let faults = FaultPlan()
        faults.arm("fs.replace")

        let firstSession = try world.openCore()
        let interrupted = await world.makeService(
            on: firstSession, fileSystem: FaultingRetentionFileSystem(faults: faults)).sweep()
        XCTAssertFalse(world.audioExists(expired.fileName), "the file went before the completion was recorded")
        XCTAssertEqual(outcome(interrupted, expired.itemID), .retryLater(.ledgerUnavailable))
        XCTAssertFalse(interrupted.converged)
        firstSession.close()

        let secondSession = try world.openCore()
        let converged = await world.makeService(on: secondSession).sweep()
        XCTAssertEqual(outcome(converged, expired.itemID), .alreadyRemoved, "the core found the file already gone")
        XCTAssertTrue(converged.converged)
        XCTAssertEqual(try world.ledgerEntries().map(\.state), [.removed])

        let repeated = await world.makeService(on: secondSession).sweep()
        XCTAssertEqual(outcome(repeated, expired.itemID), .alreadyRemoved)
        XCTAssertTrue(repeated.converged)
        secondSession.close()
    }

    func testLookupFailuresAroundTheUnlinkAreNeverReportedAsCleanupAndConverge() async throws {
        let world = try makeWorld()
        let beforeUnlink = try await world.seedVoice(captureID: "cap-before", transcribedDaysAgo: 10)
        let afterUnlink = try await world.seedVoice(captureID: "cap-after", transcribedDaysAgo: 10)
        let fileSystem = FaultingRetentionFileSystem(faults: FaultPlan())
        fileSystem.failPresenceLookups(of: beforeUnlink.fileName)
        fileSystem.failPresenceLookups(of: afterUnlink.fileName, afterPassing: 1)
        let session = try world.openCore()
        let service = world.makeService(on: session, fileSystem: fileSystem)

        let failed = await service.sweep()
        XCTAssertEqual(outcome(failed, beforeUnlink.itemID), .retryLater(.storageUnavailable))
        XCTAssertEqual(outcome(failed, afterUnlink.itemID), .retryLater(.storageUnavailable))
        XCTAssertFalse(failed.converged)
        XCTAssertTrue(world.audioExists(beforeUnlink.fileName), "an unknown file is not skipped as absent")
        XCTAssertFalse(world.audioExists(afterUnlink.fileName), "the unlink ran but could not be verified")
        XCTAssertEqual(try world.ledgerEntries().map(\.state), [.failing, .failing], "no removal is recorded")

        let converged = await service.sweep()
        XCTAssertEqual(outcome(converged, beforeUnlink.itemID), .removed)
        XCTAssertEqual(outcome(converged, afterUnlink.itemID), .alreadyRemoved)
        XCTAssertTrue(converged.converged)
        XCTAssertFalse(world.audioExists(beforeUnlink.fileName))
        XCTAssertEqual(try world.ledgerEntries().map(\.state), [.removed, .removed])
        session.close()
    }

    func testAShutDownCoreIsReportedAsUnknownAndRemovesNothing() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let session = try world.openCore()
        let service = world.makeService(on: session)
        session.client.shutDown()

        let report = await service.sweep()
        XCTAssertEqual(report.listingFailure, .cancelled)
        XCTAssertFalse(report.converged)
        XCTAssertTrue(world.audioExists(expired.fileName))
        session.close()
    }

    func testEveryCoreClientSharingAHandleStartsItsOperationsInADistinctRange() {
        let starts: [UInt64] = [
            TranscriptionCoreClient.firstOperationID, CoreDeletionCleanupClient.firstOperationID,
            CoreExportClient.firstOperationID, CoreReminderReconciler.firstOperationID,
            CoreAudioRetentionClient.firstOperationID, CoreNotificationActionClient.firstOperationID,
            ProcessingHealthService.firstOperationID, JobRunnerService.firstOperationID,
        ]
        XCTAssertEqual(Set(starts).count, starts.count, "no two clients start at the same operation identifier")
        let following = starts.filter { $0 > CoreAudioRetentionClient.firstOperationID }.min()
        XCTAssertEqual(
            following, CoreNotificationActionClient.firstOperationID, "retention's range ends where notification actions start")
        XCTAssertGreaterThan(ProcessingHealthService.firstOperationID, CoreNotificationActionClient.firstOperationID)
        XCTAssertGreaterThan(CoreAudioRetentionClient.firstOperationID, CoreReminderReconciler.firstOperationID)
    }

    /// One core handle with a single event handler that asks both the retention client and the reminder reconciler
    /// about every event, so an event both would take shows up as a double claim.
    private final class SharedCoreRouter: @unchecked Sendable {
        struct Claim: Equatable {
            let retention: Bool
            let reminders: Bool
        }

        private let lock = NSLock()
        private var claimsByOperation: [(operationID: UInt64, claim: Claim)] = []

        func route(_ event: CoreEvent, retention: CoreAudioRetentionClient, reminders: CoreReminderReconciler) {
            let claimedByRetention = retention.handle(event)
            let claimedByReminders = reminders.handle(event)
            lock.lock()
            claimsByOperation.append(
                (event.operationID, Claim(retention: claimedByRetention, reminders: claimedByReminders)))
            lock.unlock()
        }

        var claims: [(operationID: UInt64, claim: Claim)] {
            lock.lock()
            defer { lock.unlock() }
            return claimsByOperation
        }
    }

    private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 20) async throws -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition()
    }

    func testARetentionSweepOverlappingAReminderPassOnOneCoreReachesOnlyItsOwner() async throws {
        let world = try makeWorld()
        let expired = try await world.seedVoice(captureID: "cap-expired", transcribedDaysAgo: 10)
        let core = try CoreHandle(path: world.databasePath)
        defer { core.close() }
        let retention = CoreAudioRetentionClient(core: core)
        defer { retention.shutDown() }
        let center = FakeNotificationCenter()
        let gatedCenter = SuspendingNotificationCenter(inner: center)
        let gate = SuspensionGate()
        gatedCenter.pendingGate = gate
        let bridge = NotificationBridge(center: gatedCenter, ingestor: NotificationEventIngestor(handler: { _ in }))
        let reminders = try CoreReminderReconciler(core: core, bridge: bridge, center: gatedCenter)
        defer { reminders.invalidate() }
        let router = SharedCoreRouter()
        try core.setEventHandler { event in router.route(event, retention: retention, reminders: reminders) }
        let permission = NotificationPermissionReconciler(
            center: center, reconciliation: reminders,
            observations: InMemoryNotificationPermissionObservationStore(), promptCandidates: { [] })
        let service = AudioRetentionService(
            core: retention, layout: world.layout, fileSystem: FileManagerAudioRetentionFileSystem(),
            pressureProbe: FixedPressureProbe(pressure: .normal))

        // The reminder pass is inside the core, waiting on the OS pending list, when the sweep is submitted. The sweep
        // queues behind it on the core's single worker, so both first operations are outstanding at once.
        let pass = Task { try await permission.refresh(trigger: .launch) }
        let passHeld = try await waitUntil { gate.arrivals >= 1 }
        XCTAssertTrue(passHeld, "the pass asked for the pending list")
        let sweep = Task { await service.sweep() }
        let sweepSubmitted = try await waitUntil { retention.outstandingOperationCount == 1 }
        XCTAssertTrue(sweepSubmitted, "the sweep is waiting on the core")
        gate.release()

        let passReport = try await pass.value
        let report = await sweep.value
        XCTAssertNil(passReport.reconciliationFailure, "the pass got its own answer")
        XCTAssertTrue(passReport.converged)
        XCTAssertNil(report.listingFailure, "the sweep got its own answer")
        XCTAssertEqual(outcome(report, expired.itemID), .removed)
        XCTAssertTrue(report.converged)
        XCTAssertFalse(world.audioExists(expired.fileName))

        let claims = router.claims
        XCTAssertEqual(Set(claims.map(\.operationID)).count, claims.count, "no two operations shared an identifier")
        for (operationID, claim) in claims {
            XCTAssertNotEqual(claim.retention, claim.reminders, "exactly one client owns operation \(operationID)")
        }
        let retentionIDs = claims.filter(\.claim.retention).map(\.operationID)
        let reminderIDs = claims.filter(\.claim.reminders).map(\.operationID)
        XCTAssertEqual(retentionIDs.count, 2, "one sweep and one fenced removal")
        XCTAssertEqual(reminderIDs.count, 1)
        XCTAssertTrue(retentionIDs.allSatisfy { $0 >= CoreAudioRetentionClient.firstOperationID })
        XCTAssertTrue(reminderIDs.allSatisfy { $0 < CoreAudioRetentionClient.firstOperationID })
    }
}
