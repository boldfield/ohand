import XCTest

private final class FlakyWriter {
    var failRecordWrites = false
    var failAllWrites = false

    func write(_ data: Data, to url: URL) throws {
        if failAllWrites || (failRecordWrites && url.path.contains("/records/")) {
            throw NSError(domain: "CaptureProbeTests", code: 7)
        }
        try IngressStore.protectedAtomicWrite(data, to: url)
    }
}

final class IngressFlowTests: XCTestCase {
    private var rootDirectory: URL!
    private var savedLiveFlow: IngressFlow!

    override func setUpWithError() throws {
        rootDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("CaptureProbeTests-\(UUID().uuidString)", isDirectory: true)
        savedLiveFlow = IngressFlow.live
    }

    override func tearDownWithError() throws {
        IngressFlow.live = savedLiveFlow
        try? FileManager.default.removeItem(at: rootDirectory)
    }

    private func makeFlow(writer: FlakyWriter? = nil) -> IngressFlow {
        guard let writer = writer else {
            return IngressFlow(store: IngressStore(rootDirectory: rootDirectory))
        }
        let store = IngressStore(rootDirectory: rootDirectory, writeData: { data, url in
            try writer.write(data, to: url)
        })
        return IngressFlow(store: store)
    }

    func testHandoffIdIsStableAcrossRepeatedRegistrationsAndRelaunch() {
        let firstProcess = makeFlow()
        let handoffId = firstProcess.registerHandoff(source: .controlIntent)
        XCTAssertEqual(firstProcess.registerHandoff(source: .controlIntent), handoffId)

        let relaunchedProcess = makeFlow()
        XCTAssertEqual(relaunchedProcess.registerHandoff(source: .controlIntent), handoffId)
    }

    func testEnteringCommitsTheHandoffIdAndSource() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)

        let outcome = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)

        XCTAssertEqual(outcome.status, .saved)
        XCTAssertEqual(outcome.captureId, handoffId)
        let record = try XCTUnwrap(flow.store.loadRecord(captureId: handoffId))
        XCTAssertEqual(record.source, .controlIntent)
        XCTAssertEqual(record.launchKind, .cold)
        XCTAssertNil(flow.store.loadPending())
    }

    func testDirectEntryWithoutHandoffGetsItsOwnId() {
        let flow = makeFlow()
        let outcome = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: true)
        XCTAssertEqual(outcome.status, .saved)
        XCTAssertEqual(flow.store.loadRecord(captureId: outcome.captureId)?.source, .directLaunch)
        XCTAssertEqual(flow.store.loadRecord(captureId: outcome.captureId)?.launchKind, .warm)
    }

    func testSecondEntryAfterCommitUsesANewId() {
        let flow = makeFlow()
        let first = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        let second = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: true)
        XCTAssertNotEqual(first.captureId, second.captureId)
        XCTAssertEqual(flow.store.allRecords().count, 2)
    }

    func testRetryAfterRecordWriteFailureReusesTheSameId() {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let flow = makeFlow(writer: writer)

        let failed = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: false)
        guard case .failed = failed.status else { return XCTFail("expected a failed outcome, got \(failed.status)") }
        XCTAssertTrue(flow.store.allRecords().isEmpty)

        writer.failRecordWrites = false
        let retried = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        XCTAssertEqual(retried.status, .saved)
        XCTAssertEqual(retried.captureId, failed.captureId)
        XCTAssertEqual(flow.store.allRecords().map(\.captureId), [failed.captureId])
    }

    func testRetryAfterTotalWriteFailureKeepsTheIdInMemory() {
        let writer = FlakyWriter()
        writer.failAllWrites = true
        let flow = makeFlow(writer: writer)

        let failed = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: false)
        guard case .failed = failed.status else { return XCTFail("expected a failed outcome, got \(failed.status)") }
        XCTAssertNil(flow.store.loadPending())

        writer.failAllWrites = false
        let retried = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        XCTAssertEqual(retried.status, .saved)
        XCTAssertEqual(retried.captureId, failed.captureId)
    }

    func testPendingEntrySurvivesRelaunchAfterAFailedCommit() {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let firstProcess = makeFlow(writer: writer)
        let failed = firstProcess.enter(source: .controlIntent, launchKind: .cold, protectedDataAvailable: false)

        let relaunchedProcess = makeFlow()
        let retried = relaunchedProcess.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        XCTAssertEqual(retried.status, .saved)
        XCTAssertEqual(retried.captureId, failed.captureId)
        XCTAssertEqual(relaunchedProcess.store.loadRecord(captureId: failed.captureId)?.source, .controlIntent)
    }

    func testCommitIsIdempotentAndNeverOverwritesTheFirstRecord() throws {
        let store = IngressStore(rootDirectory: rootDirectory)
        let original = IngressRecord(
            captureId: "fixed-id", source: .directLaunch, launchKind: .cold,
            protectedDataAvailable: true, committedAt: "2026-03-01T09:30:00.000Z", syntheticText: "Synthetic probe capture"
        )
        let conflicting = IngressRecord(
            captureId: "fixed-id", source: .controlIntent, launchKind: .warm,
            protectedDataAvailable: false, committedAt: "2026-03-01T09:31:00.000Z", syntheticText: "Synthetic probe capture"
        )
        XCTAssertEqual(try store.commit(original), .created)
        XCTAssertEqual(try store.commit(conflicting), .replayed)
        XCTAssertEqual(store.loadRecord(captureId: "fixed-id"), original)
        XCTAssertEqual(store.allRecords().count, 1)
    }

    func testRecordsSurviveReopeningTheStore() throws {
        let firstFlow = makeFlow()
        let outcome = firstFlow.enter(source: .controlIntent, launchKind: .cold, protectedDataAvailable: true)
        let written = try XCTUnwrap(firstFlow.store.loadRecord(captureId: outcome.captureId))

        let reopened = IngressStore(rootDirectory: rootDirectory)
        XCTAssertEqual(reopened.loadRecord(captureId: outcome.captureId), written)
        XCTAssertEqual(reopened.allRecords(), [written])
    }

    func testProtectedDataStateIsRecordedWithTheEntry() throws {
        let flow = makeFlow()
        let locked = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: false)
        let unlocked = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: true)
        XCTAssertEqual(flow.store.loadRecord(captureId: locked.captureId)?.protectedDataAvailable, false)
        XCTAssertEqual(flow.store.loadRecord(captureId: unlocked.captureId)?.protectedDataAvailable, true)
        XCTAssertTrue(locked.displayLines.contains("Protected data: unavailable"))
    }

    func testDisplayShowsOnlyTheCurrentEntry() {
        let flow = makeFlow()
        let earlier = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        let current = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: true)
        let shown = current.displayLines.joined(separator: "\n")
        XCTAssertTrue(shown.contains(current.captureId))
        XCTAssertFalse(shown.contains(earlier.captureId))
    }

    func testPresentationRecordMatchesTheOutcome() throws {
        let flow = makeFlow()
        let outcome = flow.enter(source: .directLaunch, launchKind: .cold, protectedDataAvailable: true)
        flow.recordPresentation(outcome)
        let data = try Data(contentsOf: flow.store.presentedURL)
        let presented = try JSONDecoder().decode(IngressPresented.self, from: data)
        XCTAssertEqual(presented.captureId, outcome.captureId)
        XCTAssertEqual(presented.statusText, "Saved")
    }

    func testControlIntentPerformRegistersTheHandoffAndNotifiesTheScene() async throws {
        let flow = makeFlow()
        IngressFlow.live = flow
        let notified = expectation(forNotification: IngressFlow.handoffRegisteredNotification, object: nil) { notification in
            notification.userInfo?["captureId"] is String
        }

        _ = try await ProbeOpenCaptureIntent(target: .capture).perform()
        await fulfillment(of: [notified], timeout: 5)

        let pending = try XCTUnwrap(flow.store.loadPending())
        XCTAssertEqual(pending.source, .controlIntent)
        let outcome = flow.enter(source: .directLaunch, launchKind: .warm, protectedDataAvailable: true)
        XCTAssertEqual(outcome.captureId, pending.captureId)
        XCTAssertEqual(flow.store.loadRecord(captureId: pending.captureId)?.source, .controlIntent)
    }
}
