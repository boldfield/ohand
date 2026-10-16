import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider sender that refuses every request: retention controls never reach a provider.
private final class UnreachableRetentionProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

/// Drives `ServiceCompositionRoot.retentionControls` into the real Rust core over an on-disk store with real audio
/// files: the stored period, the unresolved sole-audio list, retry, the deletion that is refused for sole-source audio,
/// and the gate. The lifecycle coordinator is a module of the root, as in the app, so audio removal and the sweep run
/// through the same `AudioRetentionService` the app uses.
final class RetentionControlRootIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreRetentionWorld!
    private var center: FakeNotificationCenter!

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreRetentionWorld()
        center = FakeNotificationCenter()
    }

    override func tearDown() {
        world.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        super.tearDown()
    }

    // MARK: tests

    func testStatusReportsTheDefaultPolicyAndTheUnresolvedSoleAudioWithItsReason() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        _ = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let (root, _) = try makeConnectedRoot()
        defer { root.shutDown() }

        let status = try await root.retentionControls.status()

        XCTAssertEqual(status.policy, RetentionPolicySetting(periodSeconds: 7 * 86_400, isDefault: true))
        XCTAssertEqual(status.unresolved.count, 1, "a transcribed item's audio is not unresolved")
        let entry = try XCTUnwrap(status.unresolved.first)
        XCTAssertEqual(entry.itemID, sole.itemID)
        XCTAssertEqual(entry.capturedAt, "2026-10-01T08:00:00Z")
        XCTAssertEqual(entry.reason, .transcriptionPending)
        XCTAssertEqual(entry.options, [.deleteAudio], "retry is offered only when no transcription job is pending")
    }

    func testAStoredPeriodSurvivesAReopenedRootAndAnOutOfBoundsPeriodChangesNothing() async throws {
        let (first, _) = try makeConnectedRoot()
        let stored = try await first.retentionControls.setPeriod(seconds: 30 * 86_400)
        XCTAssertEqual(stored, RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))

        for outOfBounds in [Int64(0), -5, RetentionPolicySetting.maximumPeriodSeconds + 1] {
            do {
                _ = try await first.retentionControls.setPeriod(seconds: outOfBounds)
                XCTFail("\(outOfBounds) seconds is outside the bounds")
            } catch {
                XCTAssertEqual(error as? RetentionControlError, .invalidRequest)
            }
        }
        let unchanged = try await first.retentionControls.status()
        XCTAssertEqual(unchanged.policy.periodSeconds, 30 * 86_400)
        first.shutDown()

        let (second, _) = try makeConnectedRoot()
        defer { second.shutDown() }
        let reopened = try await second.retentionControls.status()
        XCTAssertEqual(reopened.policy, RetentionPolicySetting(periodSeconds: 30 * 86_400, isDefault: false))

        let longest = try await second.retentionControls.setPeriod(
            seconds: RetentionPolicySetting.maximumPeriodSeconds)
        XCTAssertEqual(longest.periodSeconds, RetentionPolicySetting.maximumPeriodSeconds)
        let shortest = try await second.retentionControls.setPeriod(seconds: 1)
        XCTAssertEqual(shortest.periodSeconds, 1)
    }

    func testTheSweepReadsTheStoredPeriodAndNeverRemovesSoleSourceAudioWhateverItIs() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 3)
        let (root, coordinator) = try makeConnectedRoot()
        defer { root.shutDown() }
        let retention = try XCTUnwrap(coordinator.audioRetention)

        let underDefault = await retention.sweep()
        XCTAssertNil(underDefault.listingFailure)
        XCTAssertTrue(world.audioExists(transcribed.fileName), "three days is inside the default seven")

        _ = try await root.retentionControls.setPeriod(seconds: 1)
        let afterShortening = await retention.sweep()
        XCTAssertNil(afterShortening.listingFailure)
        XCTAssertFalse(world.audioExists(transcribed.fileName), "the stored one-second period expired it")
        XCTAssertTrue(world.audioExists(sole.fileName), "sole-source audio never expires")
        XCTAssertEqual(try world.ledgerEntries().map(\.itemID), [transcribed.itemID])

        let status = try await root.retentionControls.status()
        XCTAssertEqual(status.unresolved.map(\.itemID), [sole.itemID])
    }

    func testRetryTranscriptionRequeuesThroughTheCoreAndReportsItsRefusals() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let (root, _) = try makeConnectedRoot()
        defer { root.shutDown() }
        let controls = root.retentionControls

        await assertThrows(.jobInFlight) { _ = try await controls.retryTranscription(itemID: sole.itemID) }

        world.clearJobs(itemID: sole.itemID)
        let jobID = try await controls.retryTranscription(itemID: sole.itemID)
        XCTAssertFalse(jobID.isEmpty)
        await assertThrows(.jobInFlight) { _ = try await controls.retryTranscription(itemID: sole.itemID) }

        await assertThrows(.notRetryable(transcriptionState: "transcribed")) {
            _ = try await controls.retryTranscription(itemID: transcribed.itemID)
        }
        await assertThrows(.itemNotFound) {
            _ = try await controls.retryTranscription(itemID: UUID().uuidString.lowercased())
        }
        await assertThrows(.invalidRequest) { _ = try await controls.retryTranscription(itemID: "") }
    }

    func testDeleteAudioRefusesSoleSourceAudioAndRemovesTheAudioOfATranscribedItem() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let (root, _) = try makeConnectedRoot()
        defer { root.shutDown() }
        let controls = root.retentionControls

        await assertThrows(.soleSourceUseItemDeletion) { _ = try await controls.deleteAudio(itemID: sole.itemID) }
        XCTAssertTrue(world.audioExists(sole.fileName), "sole-source audio is left in place")
        XCTAssertEqual(try world.ledgerEntries(), [])

        let receipt = try await controls.deleteAudio(itemID: transcribed.itemID)
        XCTAssertEqual(receipt, .removed)
        XCTAssertFalse(world.audioExists(transcribed.fileName))
        XCTAssertTrue(world.audioExists(sole.fileName))
        XCTAssertEqual(try world.ledgerEntries().map(\.itemID), [transcribed.itemID])

        let again = try await controls.deleteAudio(itemID: transcribed.itemID)
        XCTAssertEqual(again, .alreadyRemoved, "asking again finds the file gone and changes nothing")

        let status = try await controls.status()
        XCTAssertEqual(status.unresolved.map(\.itemID), [sole.itemID], "the item and its text remain readable")

        await assertThrows(.itemNotFound) { _ = try await controls.deleteAudio(itemID: "no-such-item") }
    }

    func testDeletingAudioOfAnItemDeletedMeanwhileLeavesTheFileToDeletionCleanup() async throws {
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let (root, _) = try makeConnectedRoot()
        defer { root.shutDown() }

        world.markDeleted(itemID: transcribed.itemID)

        do {
            _ = try await root.retentionControls.deleteAudio(itemID: transcribed.itemID)
            XCTFail("a deleted item's audio belongs to deletion cleanup")
        } catch {
            XCTAssertNotNil(error as? RetentionControlError)
        }
        XCTAssertTrue(world.audioExists(transcribed.fileName))
    }

    func testDeletingAudioIsUnavailableUntilTheCoordinatorIsConnected() async throws {
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let root = try makeRoot(session: authenticatedSession(), modules: [])
        defer { root.shutDown() }

        await assertThrows(.audioRemovalUnavailable) {
            _ = try await root.retentionControls.deleteAudio(itemID: transcribed.itemID)
        }
        XCTAssertTrue(world.audioExists(transcribed.fileName))
    }

    func testACaptureOnlySessionCannotReadOrWriteAndNothingChanges() async throws {
        let sole = try await world.seedVoice(captureID: "cap-sole", transcribedDaysAgo: nil)
        let transcribed = try await world.seedVoice(captureID: "cap-transcribed", transcribedDaysAgo: 1)
        let session = authenticatedSession()
        let (root, _) = try makeConnectedRoot(session: session)
        defer { root.shutDown() }
        let controls = root.retentionControls
        session.relock(.explicit)
        XCTAssertNotEqual(session.scope, .authenticated)

        await assertThrows(.notAuthenticated) { _ = try await controls.status() }
        await assertThrows(.notAuthenticated) { _ = try await controls.setPeriod(seconds: 86_400) }
        world.clearJobs(itemID: sole.itemID)
        await assertThrows(.notAuthenticated) { _ = try await controls.retryTranscription(itemID: sole.itemID) }
        await assertThrows(.notAuthenticated) { _ = try await controls.deleteAudio(itemID: transcribed.itemID) }
        XCTAssertTrue(world.audioExists(transcribed.fileName))
        XCTAssertEqual(try world.ledgerEntries(), [])

        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        let status = try await controls.status()
        XCTAssertTrue(status.policy.isDefault, "the refused write stored nothing")
        let retry = try await controls.retryTranscription(itemID: sole.itemID)
        XCTAssertFalse(retry.isEmpty, "the refused retry queued nothing, so a retry now is the first")
    }

    func testTheRetentionControlRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.retentionControlClientName })
        XCTAssertEqual(mine.start, CoreRetentionControlClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: helpers

    private func assertThrows(
        _ expected: RetentionControlError, file: StaticString = #filePath, line: UInt = #line,
        _ operation: () async throws -> Void
    ) async {
        do {
            try await operation()
            XCTFail("expected \(expected)", file: file, line: line)
        } catch {
            XCTAssertEqual(error as? RetentionControlError, expected, file: file, line: line)
        }
    }

    private func authenticatedSession() -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        return session
    }

    private func makeConnectedRoot() throws -> (ServiceCompositionRoot, LifecycleCoordinator) {
        try makeConnectedRoot(session: authenticatedSession())
    }

    private func makeConnectedRoot(session: ReadAuthenticationSession) throws -> (
        ServiceCompositionRoot, LifecycleCoordinator
    ) {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(session: session, modules: [coordinator])
        try coordinator.connect(to: root)
        return (root, coordinator)
    }

    private func makeRoot(session: ReadAuthenticationSession, modules: [CompositionModule]) throws
        -> ServiceCompositionRoot
    {
        try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: world.layout.rootDirectory.path, captureRouteID: "route-personal",
                jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableRetentionProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true)),
            modules: modules)
    }
}
