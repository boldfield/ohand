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
        let center = NotificationCenter()
        let flow = IngressFlow(store: IngressStore(rootDirectory: rootDirectory), notificationCenter: center)
        IngressFlow.live = flow
        let notified = expectation(forNotification: IngressFlow.handoffRegisteredNotification, object: nil, notificationCenter: center) { notification in
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

    // MARK: Scene/intent ordering through IngressSession

    private final class RenderLog {
        private let lock = NSLock()
        private var rendered: [IngressOutcome] = []
        var onRender: (() -> Void)?

        func append(_ outcome: IngressOutcome) {
            lock.lock()
            rendered.append(outcome)
            lock.unlock()
            onRender?()
        }

        var outcomes: [IngressOutcome] {
            lock.lock()
            defer { lock.unlock() }
            return rendered
        }
    }

    private struct SceneHarness {
        let flow: IngressFlow
        let session: IngressSession
        let log: RenderLog
    }

    private final class TestClock {
        var current = Date(timeIntervalSince1970: 1_800_000_000)
        func advance(_ seconds: TimeInterval) { current = current.addingTimeInterval(seconds) }
    }

    private func makeSceneHarness(writer: FlakyWriter? = nil, clock: TestClock? = nil) -> SceneHarness {
        let store = writer.map { writer in
            IngressStore(rootDirectory: rootDirectory, writeData: { data, url in try writer.write(data, to: url) })
        } ?? IngressStore(rootDirectory: rootDirectory)
        let flow = clock.map { clock in
            IngressFlow(store: store, notificationCenter: NotificationCenter(), now: { clock.current })
        } ?? IngressFlow(store: store, notificationCenter: NotificationCenter())
        IngressFlow.live = flow
        let session = IngressSession(flow: flow)
        let log = RenderLog()
        session.observeHandoffs(protectedDataAvailable: { true }, onOutcome: { log.append($0) })
        return SceneHarness(flow: flow, session: session, log: log)
    }

    private func enterForeground(_ harness: SceneHarness) {
        harness.log.append(harness.session.willEnterForeground(protectedDataAvailable: true))
    }

    private func performControlAndWaitForRender(_ harness: SceneHarness) async throws {
        let rendered = expectation(description: "scene rendered the handoff")
        harness.log.onRender = { rendered.fulfill() }
        _ = try await ProbeOpenCaptureIntent(target: .capture).perform()
        await fulfillment(of: [rendered], timeout: 5)
        harness.log.onRender = nil
    }

    private func performControlWhileBackgroundedOrBeforeScene() async throws {
        _ = try await ProbeOpenCaptureIntent(target: .capture).perform()
        await MainActor.run {}
    }

    private func assertSingleControlEntry(
        _ harness: SceneHarness, launchKind: IngressLaunchKind, expectedRecordCount: Int,
        file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let records = harness.flow.store.allRecords()
        XCTAssertEqual(records.count, expectedRecordCount, file: file, line: line)
        let record = try XCTUnwrap(records.last, file: file, line: line)
        XCTAssertEqual(record.source, .controlIntent, file: file, line: line)
        XCTAssertEqual(record.launchKind, launchKind, file: file, line: line)

        let lastRendered = try XCTUnwrap(harness.log.outcomes.last, file: file, line: line)
        XCTAssertEqual(lastRendered.captureId, record.captureId, file: file, line: line)
        XCTAssertEqual(lastRendered.source, .controlIntent, file: file, line: line)
        XCTAssertEqual(lastRendered.status.isFailure, false, file: file, line: line)
        XCTAssertTrue(lastRendered.displayLines.contains("Entry: controlIntent, \(launchKind.rawValue) launch"), file: file, line: line)
        let activationIds = Set(harness.log.outcomes.map(\.captureId))
        XCTAssertEqual(activationIds.count, expectedRecordCount, "every rendered ID must be a persisted record", file: file, line: line)
        XCTAssertNil(harness.flow.store.loadPending(), file: file, line: line)
    }

    func testColdForegroundThenControlIntentYieldsOneRecordAndOneId() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        XCTAssertEqual(harness.flow.store.allRecords().count, 1)

        try await performControlAndWaitForRender(harness)

        try assertSingleControlEntry(harness, launchKind: .cold, expectedRecordCount: 1)
    }

    func testColdControlIntentThenForegroundYieldsOneRecordAndOneId() async throws {
        let harness = makeSceneHarness()
        try await performControlWhileBackgroundedOrBeforeScene()
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)
        XCTAssertTrue(harness.log.outcomes.isEmpty)

        enterForeground(harness)

        try assertSingleControlEntry(harness, launchKind: .cold, expectedRecordCount: 1)
    }

    func testWarmForegroundThenControlIntentYieldsOneNewRecord() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()

        enterForeground(harness)
        XCTAssertEqual(harness.flow.store.allRecords().count, 2)
        try await performControlAndWaitForRender(harness)

        try assertSingleControlEntry(harness, launchKind: .warm, expectedRecordCount: 2)
        XCTAssertEqual(harness.flow.store.allRecords().first?.source, .directLaunch)
    }

    func testWarmControlIntentWhileBackgroundedThenForegroundYieldsOneNewRecord() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()

        try await performControlWhileBackgroundedOrBeforeScene()
        XCTAssertEqual(harness.flow.store.allRecords().count, 1)
        enterForeground(harness)

        try assertSingleControlEntry(harness, launchKind: .warm, expectedRecordCount: 2)
    }

    func testControlActivationLongAfterAnUnrelatedDirectEntryGetsItsOwnRecord() async throws {
        let clock = TestClock()
        let harness = makeSceneHarness(clock: clock)
        enterForeground(harness)
        let directRecord = try XCTUnwrap(harness.flow.store.allRecords().first)
        XCTAssertEqual(directRecord.source, .directLaunch)

        clock.advance(120)
        try await performControlAndWaitForRender(harness)

        let records = harness.flow.store.allRecords()
        XCTAssertEqual(records.count, 2)
        XCTAssertEqual(harness.flow.store.loadRecord(captureId: directRecord.captureId), directRecord)
        let controlRecord = try XCTUnwrap(records.first { $0.captureId != directRecord.captureId })
        XCTAssertEqual(controlRecord.source, .controlIntent)
        let lastRendered = try XCTUnwrap(harness.log.outcomes.last)
        XCTAssertEqual(lastRendered.captureId, controlRecord.captureId)
        XCTAssertEqual(lastRendered.source, .controlIntent)
        XCTAssertEqual(lastRendered.status, .saved)
        XCTAssertNil(harness.flow.store.loadPending())
    }

    func testControlActivationWithinTheClaimWindowStillClaimsTheForegroundEntry() async throws {
        let clock = TestClock()
        let harness = makeSceneHarness(clock: clock)
        enterForeground(harness)
        clock.advance(1)

        try await performControlAndWaitForRender(harness)

        try assertSingleControlEntry(harness, launchKind: .cold, expectedRecordCount: 1)
    }

    func testSecondControlTapInTheSameForegroundSessionIsANewEntry() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        try await performControlAndWaitForRender(harness)
        let firstId = try XCTUnwrap(harness.log.outcomes.last?.captureId)

        try await performControlAndWaitForRender(harness)

        let records = harness.flow.store.allRecords()
        XCTAssertEqual(records.count, 2)
        XCTAssertEqual(records.map(\.source), [.controlIntent, .controlIntent])
        XCTAssertNotEqual(harness.log.outcomes.last?.captureId, firstId)
    }

    func testControlIntentAdoptsTheEntryLeftPendingByAFailedDirectLaunchWrite() async throws {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let harness = makeSceneHarness(writer: writer)
        harness.log.append(harness.session.willEnterForeground(protectedDataAvailable: false))
        let failedId = try XCTUnwrap(harness.log.outcomes.last?.captureId)
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)

        writer.failRecordWrites = false
        try await performControlAndWaitForRender(harness)

        let records = harness.flow.store.allRecords()
        XCTAssertEqual(records.map(\.captureId), [failedId])
        XCTAssertEqual(records.first?.source, .controlIntent)
        XCTAssertEqual(harness.log.outcomes.last?.status, .saved)
    }

    func testControlHandoffInTheBackgroundDoesNotRenderOrCommit() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()
        let renderedBefore = harness.log.outcomes.count

        try await performControlWhileBackgroundedOrBeforeScene()

        XCTAssertEqual(harness.log.outcomes.count, renderedBefore)
        XCTAssertEqual(harness.flow.store.allRecords().count, 1)
        XCTAssertNotNil(harness.flow.store.loadPending())
    }
}

private extension IngressOutcome.Status {
    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}
