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

        let shortened = await world.makeService(on: session, retentionPeriodSeconds: 2 * 86_400).sweep()
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

    func testTheEffectStartsOnlyInTheFirstHalfOfTheLease() {
        XCTAssertTrue(CoreAudioRetentionClient.leaseAllowsEffect(elapsed: 0, leaseMillis: 3_000))
        XCTAssertTrue(CoreAudioRetentionClient.leaseAllowsEffect(elapsed: 1.4, leaseMillis: 3_000))
        XCTAssertFalse(CoreAudioRetentionClient.leaseAllowsEffect(elapsed: 1.5, leaseMillis: 3_000))
        XCTAssertFalse(CoreAudioRetentionClient.leaseAllowsEffect(elapsed: 4, leaseMillis: 3_000))
        XCTAssertFalse(CoreAudioRetentionClient.leaseAllowsEffect(elapsed: -1, leaseMillis: 3_000))
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
}
