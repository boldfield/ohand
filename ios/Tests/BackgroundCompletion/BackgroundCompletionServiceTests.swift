import Foundation
import XCTest
@testable import OhAndServices

/// The lease logic against scripted fakes. The real-core behavior is in `BackgroundCompletionCoreIntegrationTests`.
final class BackgroundCompletionServiceTests: XCTestCase {
    private var grant: FakeBackgroundGrant!
    private var drain: FakeCompletionDrain!
    private var reader: FakePendingReader!
    private var alert: FakePendingAlert!

    override func setUp() {
        super.setUp()
        grant = FakeBackgroundGrant()
        drain = FakeCompletionDrain()
        reader = FakePendingReader(.none)
        alert = FakePendingAlert()
    }

    @MainActor private func makeService(
        maximumWindow: TimeInterval = 5, settleTimeout: TimeInterval = 0.2, withAlert: Bool = true
    ) -> BackgroundCompletionService {
        BackgroundCompletionService(
            drain: drain, grant: grant, pending: reader, alert: withAlert ? alert : nil,
            maximumWindow: maximumWindow, settleTimeout: settleTimeout, pollInterval: 0.005)
    }

    @MainActor private func startLease(_ service: BackgroundCompletionService) async -> Task<BackgroundCompletionOutcome, Never> {
        let lease = Task { await service.completeAfterCapture() }
        let isOpen = await waitUntilTrue { self.grant.beginCount == 1 && self.drain.windowBeginCount == 1 }
        XCTAssertTrue(isOpen)
        return lease
    }

    @MainActor func testRefusedGrantOpensNoWindowAndArmsNoAlert() async {
        grant.tokenToReturn = 0
        let outcome = await makeService().completeAfterCapture()

        XCTAssertEqual(outcome, .refused)
        XCTAssertEqual(drain.windowBeginCount, 0)
        XCTAssertEqual(alert.armCount, 0)
        XCTAssertEqual(grant.endedTokenList, [], "an invalid token is never ended")
    }

    @MainActor func testTheSystemRefusingByCallingTheHandlerAtOnceIsRefused() async {
        grant.tokenToReturn = 0
        grant.expiresDuringBegin = true
        let outcome = await makeService().completeAfterCapture()

        XCTAssertEqual(outcome, .refused)
        XCTAssertEqual(drain.windowBeginCount, 0)
        XCTAssertEqual(alert.armCount, 0)
    }

    @MainActor func testExpirationBeforeBeginReturnsEndsTheTokenOnceAndOpensNoWindow() async {
        grant.expiresDuringBegin = true
        let outcome = await makeService().completeAfterCapture()

        XCTAssertEqual(outcome, .expired(.systemExpiration))
        XCTAssertEqual(grant.endedTokenList, [7])
        XCTAssertEqual(drain.windowBeginCount, 0)
        XCTAssertEqual(alert.armCount, 0)
    }

    @MainActor func testCompletedWhenTheDrainGoesIdleAndNothingIsPendingRemovesTheAlert() async {
        let service = makeService()
        let lease = await startLease(service)
        drain.finishDrain()
        let outcome = await lease.value

        XCTAssertEqual(outcome, .completed)
        XCTAssertEqual(alert.armCount, 1)
        XCTAssertEqual(alert.disarmCount, 1)
        XCTAssertEqual(drain.windowEndCount, 1)
        XCTAssertEqual(grant.endedTokenList, [7])
    }

    @MainActor func testPendingWorkAfterTheDrainKeepsTheAlertAndEndsTheGrant() async {
        reader.set(.pending(jobs: 2))
        let service = makeService()
        let lease = await startLease(service)
        drain.finishDrain()
        let outcome = await lease.value

        XCTAssertEqual(outcome, .pendingRemains(pendingJobs: 2))
        XCTAssertEqual(alert.armCount, 1)
        XCTAssertEqual(alert.disarmCount, 0, "unfinished work keeps its one pending alert")
        XCTAssertEqual(drain.windowEndCount, 1)
        XCTAssertEqual(grant.endedTokenList, [7])
    }

    @MainActor func testAnUnreadableCountIsNeverReportedAsCompleted() async {
        reader.set(.unknown)
        let service = makeService()
        let lease = await startLease(service)
        drain.finishDrain()
        let outcome = await lease.value

        XCTAssertEqual(outcome, .pendingRemains(pendingJobs: nil))
        XCTAssertEqual(alert.disarmCount, 0)
    }

    @MainActor func testSystemExpirationCheckpointsAndEndsTheGrantOnceWithoutRemovingTheAlert() async {
        reader.set(.pending(jobs: 1))
        let service = makeService()
        let lease = await startLease(service)
        grant.fireExpiration()
        grant.fireExpiration()
        let outcome = await lease.value

        XCTAssertEqual(outcome, .expired(.systemExpiration))
        XCTAssertGreaterThanOrEqual(drain.windowEndCount, 1, "the running drain was asked to stop at a checkpoint")
        XCTAssertFalse(drain.isDraining)
        XCTAssertEqual(grant.endedTokenList, [7], "the background task is ended exactly once")
        XCTAssertEqual(alert.armCount, 1)
        XCTAssertEqual(alert.disarmCount, 0)
    }

    @MainActor func testTheWindowCapEndsAnUnfinishedDrainWithoutWaitingForTheSystem() async {
        let service = makeService(maximumWindow: 0.05)
        let outcome = await service.completeAfterCapture()

        XCTAssertEqual(outcome, .expired(.windowElapsed))
        XCTAssertFalse(drain.isDraining)
        XCTAssertEqual(grant.endedTokenList, [7])
        XCTAssertEqual(alert.disarmCount, 0)
    }

    @MainActor func testADrainThatWillNotStopDoesNotHoldTheGrantPastTheSettleTimeout() async {
        drain.stopsWhenWindowEnds = false
        let service = makeService(maximumWindow: 0.05, settleTimeout: 0.05)
        let started = Date()
        let outcome = await service.completeAfterCapture()

        XCTAssertEqual(outcome, .expired(.windowElapsed))
        XCTAssertTrue(drain.isDraining)
        XCTAssertEqual(grant.endedTokenList, [7], "the grant is ended even though the drain never reported idle")
        XCTAssertLessThan(Date().timeIntervalSince(started), 2)
    }

    @MainActor func testShutDownDuringALeaseEndsItAsShutDown() async {
        let service = makeService()
        let lease = await startLease(service)
        service.shutDown()
        let outcome = await lease.value

        XCTAssertEqual(outcome, .expired(.shutDown))
        XCTAssertEqual(grant.endedTokenList, [7])

        let after = await service.completeAfterCapture()
        XCTAssertEqual(after, .expired(.shutDown))
        XCTAssertEqual(grant.beginCount, 1, "a shut-down service asks for no more time")
    }

    @MainActor func testASecondCaptureEndingDuringALeaseJoinsItAndExtendsNothing() async {
        let service = makeService()
        let lease = await startLease(service)
        let joined = Task { await service.completeAfterCapture() }
        let didRequest = await waitUntilTrue { self.drain.drainRequestCount == 1 }
        XCTAssertTrue(didRequest)
        drain.finishDrain()

        let first = await lease.value
        let second = await joined.value
        XCTAssertEqual(first, .completed)
        XCTAssertEqual(second, .completed)
        XCTAssertEqual(grant.beginCount, 1)
        XCTAssertEqual(drain.windowBeginCount, 1)
    }

    @MainActor func testEachLeaseAfterTheLastOneAsksForItsOwnTime() async {
        let service = makeService()
        drain.startsDrainWhenWindowOpens = false
        let first = await service.completeAfterCapture()
        let second = await service.completeAfterCapture()

        XCTAssertEqual([first, second], [.completed, .completed])
        XCTAssertEqual(grant.beginCount, 2)
        XCTAssertEqual(grant.endedTokenList, [7, 7])
    }

    @MainActor func testBecomingActiveRemovesTheAlertAndWithoutOneDoesNothing() async {
        await makeService().appBecameActive()
        XCTAssertEqual(alert.disarmCount, 1)
        await makeService(withAlert: false).appBecameActive()
        XCTAssertEqual(alert.disarmCount, 1)
    }
}
