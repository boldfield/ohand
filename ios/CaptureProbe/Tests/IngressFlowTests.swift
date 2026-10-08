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

    private func makeStore(writer: FlakyWriter? = nil) -> IngressStore {
        guard let writer = writer else {
            return IngressStore(rootDirectory: rootDirectory)
        }
        return IngressStore(rootDirectory: rootDirectory, writeData: { data, url in
            try writer.write(data, to: url)
        })
    }

    private func makeFlow(writer: FlakyWriter? = nil) -> IngressFlow {
        IngressFlow(store: makeStore(writer: writer))
    }

    // MARK: IngressFlow and IngressStore

    func testHandoffIdIsStableAcrossRepeatedRegistrationsAndRelaunch() {
        let firstProcess = makeFlow()
        let handoffId = firstProcess.registerHandoff(source: .controlIntent)
        XCTAssertEqual(firstProcess.registerHandoff(source: .controlIntent), handoffId)

        let relaunchedProcess = makeFlow()
        XCTAssertEqual(relaunchedProcess.registerHandoff(source: .controlIntent), handoffId)
    }

    func testCommittingThePendingHandoffKeepsItsIdAndSource() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)

        let outcome = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))

        XCTAssertEqual(outcome.status, .saved)
        XCTAssertEqual(outcome.captureId, handoffId)
        let record = try XCTUnwrap(flow.store.loadRecord(captureId: handoffId))
        XCTAssertEqual(record.source, .controlIntent)
        XCTAssertEqual(record.launchKind, .cold)
        XCTAssertNil(flow.store.loadPending())
    }

    func testForegroundWithoutAHandoffCreatesNoEntry() {
        let flow = makeFlow()

        XCTAssertNil(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))

        XCTAssertTrue(flow.store.allRecords().isEmpty)
        XCTAssertNil(flow.store.loadPending())
    }

    func testSecondHandoffAfterACommitUsesANewId() throws {
        let flow = makeFlow()
        let firstId = flow.registerHandoff(source: .controlIntent)
        _ = flow.commitPending(launchKind: .cold, protectedDataAvailable: true)

        let secondId = flow.registerHandoff(source: .controlIntent)
        _ = flow.commitPending(launchKind: .warm, protectedDataAvailable: true)

        XCTAssertNotEqual(firstId, secondId)
        XCTAssertEqual(Set(flow.store.allRecords().map(\.captureId)), [firstId, secondId])
    }

    func testRetryAfterRecordWriteFailureReusesTheSameId() throws {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let flow = makeFlow(writer: writer)
        let handoffId = flow.registerHandoff(source: .controlIntent)

        let failed = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: false))
        XCTAssertTrue(failed.status.isFailure)
        XCTAssertEqual(failed.captureId, handoffId)
        XCTAssertEqual(flow.store.loadPending()?.captureId, handoffId)

        writer.failRecordWrites = false
        let retried = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))
        XCTAssertEqual(retried.status, .saved)
        XCTAssertEqual(retried.captureId, handoffId)
        XCTAssertEqual(flow.store.allRecords().map(\.captureId), [handoffId])
    }

    func testRetryAfterTotalWriteFailureKeepsTheIdInMemory() throws {
        let writer = FlakyWriter()
        writer.failAllWrites = true
        let flow = makeFlow(writer: writer)
        let handoffId = flow.registerHandoff(source: .shortcutURL)
        XCTAssertNil(flow.store.loadPending())

        let failed = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: false))
        XCTAssertTrue(failed.status.isFailure)

        writer.failAllWrites = false
        let retried = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))
        XCTAssertEqual(retried.captureId, handoffId)
        XCTAssertEqual(retried.status, .saved)
        XCTAssertEqual(flow.store.allRecords().count, 1)
    }

    func testPendingEntrySurvivesRelaunchAfterAFailedCommit() throws {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let firstProcess = makeFlow(writer: writer)
        let handoffId = firstProcess.registerHandoff(source: .controlIntent)
        _ = firstProcess.commitPending(launchKind: .cold, protectedDataAvailable: false)

        let relaunchedProcess = makeFlow()
        let outcome = try XCTUnwrap(relaunchedProcess.commitPending(launchKind: .cold, protectedDataAvailable: true))

        XCTAssertEqual(outcome.captureId, handoffId)
        XCTAssertEqual(outcome.status, .saved)
        XCTAssertEqual(relaunchedProcess.store.allRecords().map(\.captureId), [handoffId])
        XCTAssertNil(relaunchedProcess.store.loadPending())
    }

    func testCommitIsIdempotentAndNeverOverwritesTheFirstRecord() throws {
        let store = makeStore()
        let original = IngressRecord(
            captureId: "6A0C5C64-3E52-4F8B-9D0A-5B1E8F2C7D11", source: .controlIntent, launchKind: .cold,
            protectedDataAvailable: true, committedAt: "2026-03-01T09:30:00.000Z", syntheticText: "Synthetic probe capture"
        )
        let conflicting = IngressRecord(
            captureId: original.captureId, source: .shortcutURL, launchKind: .warm,
            protectedDataAvailable: false, committedAt: "2026-03-01T09:31:00.000Z", syntheticText: "Other"
        )

        XCTAssertEqual(try store.commit(original), .created)
        XCTAssertEqual(try store.commit(conflicting), .replayed)
        XCTAssertEqual(store.loadRecord(captureId: original.captureId), original)
    }

    func testRecordsSurviveReopeningTheStore() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)
        _ = flow.commitPending(launchKind: .warm, protectedDataAvailable: true)
        let written = try XCTUnwrap(flow.store.loadRecord(captureId: handoffId))

        let reopened = makeStore()
        XCTAssertEqual(reopened.loadRecord(captureId: handoffId), written)
        XCTAssertEqual(reopened.allRecords(), [written])
    }

    func testProtectedDataStateIsRecordedWithTheEntry() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)

        let outcome = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: false))

        XCTAssertEqual(outcome.status, .saved)
        XCTAssertEqual(flow.store.loadRecord(captureId: handoffId)?.protectedDataAvailable, false)
        XCTAssertTrue(outcome.displayLines.contains("Protected data: unavailable"))
    }

    func testCommittingAnAnnouncedIdThatIsAlreadySavedIsAReplay() throws {
        let flow = makeFlow()
        let handoffId = flow.registerHandoff(source: .controlIntent)
        _ = flow.commitPending(launchKind: .cold, protectedDataAvailable: true)
        let saved = try XCTUnwrap(flow.store.loadRecord(captureId: handoffId))

        let replay = flow.commitHandoff(
            captureId: handoffId, source: .controlIntent, launchKind: .warm, protectedDataAvailable: false
        )

        XCTAssertEqual(replay.status, .replayed)
        XCTAssertEqual(replay.captureId, handoffId)
        XCTAssertEqual(replay.launchKind, .cold)
        XCTAssertEqual(flow.store.allRecords(), [saved])
    }

    func testDisplayShowsOnlyTheCurrentEntry() throws {
        let flow = makeFlow()
        let earlierId = flow.registerHandoff(source: .controlIntent)
        _ = flow.commitPending(launchKind: .cold, protectedDataAvailable: true)
        _ = flow.registerHandoff(source: .controlIntent)

        let current = try XCTUnwrap(flow.commitPending(launchKind: .warm, protectedDataAvailable: true))

        XCTAssertTrue(current.displayLines.contains("Capture ID: \(current.captureId)"))
        XCTAssertFalse(current.displayLines.joined().contains(earlierId))
        XCTAssertFalse(IngressOutcome.idleDisplayLines.joined().contains(earlierId))
    }

    func testPresentationRecordMatchesTheOutcomeAndTheIdleScreen() throws {
        let flow = makeFlow()
        _ = flow.registerHandoff(source: .controlIntent)
        let outcome = try XCTUnwrap(flow.commitPending(launchKind: .cold, protectedDataAvailable: true))

        flow.recordPresentation(outcome)
        let presented = try JSONDecoder().decode(IngressPresented.self, from: Data(contentsOf: flow.store.presentedURL))
        XCTAssertEqual(presented, IngressPresented(captureId: outcome.captureId, statusText: "Saved", lines: outcome.displayLines))

        flow.recordPresentation(nil)
        let idle = try JSONDecoder().decode(IngressPresented.self, from: Data(contentsOf: flow.store.presentedURL))
        XCTAssertEqual(idle, IngressPresented(captureId: nil, statusText: "Ready", lines: IngressOutcome.idleDisplayLines))
    }

    func testControlIntentPerformRegistersTheHandoffAndAnnouncesItsId() async throws {
        let center = NotificationCenter()
        let flow = IngressFlow(store: makeStore(), notificationCenter: center)
        IngressFlow.live = flow
        var announcedId: String?
        let notified = expectation(forNotification: IngressFlow.handoffRegisteredNotification, object: nil, notificationCenter: center) { notification in
            announcedId = notification.userInfo?["captureId"] as? String
            return notification.userInfo?["source"] as? String == IngressSource.controlIntent.rawValue
        }

        _ = try await ProbeOpenCaptureIntent(target: .capture).perform()
        await fulfillment(of: [notified], timeout: 5)

        let pending = try XCTUnwrap(flow.store.loadPending())
        XCTAssertEqual(pending.source, .controlIntent)
        XCTAssertEqual(announcedId, pending.captureId)
        XCTAssertTrue(flow.store.allRecords().isEmpty, "the intent registers the entry; only the scene commits it")
    }

    // MARK: Scene/handoff ordering through IngressSession

    private final class RenderLog {
        private let lock = NSLock()
        private var rendered: [IngressOutcome?] = []
        var onRender: (() -> Void)?

        func append(_ outcome: IngressOutcome?) {
            lock.lock()
            rendered.append(outcome)
            lock.unlock()
            onRender?()
        }

        var outcomes: [IngressOutcome?] {
            lock.lock()
            defer { lock.unlock() }
            return rendered
        }

        var renderedIds: Set<String> { Set(outcomes.compactMap { $0?.captureId }) }
    }

    private final class TestClock {
        var current = Date(timeIntervalSince1970: 1_800_000_000)
        func advance(_ seconds: TimeInterval) { current = current.addingTimeInterval(seconds) }
    }

    private struct SceneHarness {
        let flow: IngressFlow
        let session: IngressSession
        let log: RenderLog
        let clock: TestClock
    }

    private func makeSceneHarness(writer: FlakyWriter? = nil) -> SceneHarness {
        let clock = TestClock()
        let flow = IngressFlow(store: makeStore(writer: writer), notificationCenter: NotificationCenter(), now: { clock.current })
        IngressFlow.live = flow
        let session = IngressSession(flow: flow)
        let log = RenderLog()
        session.observeHandoffs(protectedDataAvailable: { true }, onOutcome: { log.append($0) })
        return SceneHarness(flow: flow, session: session, log: log, clock: clock)
    }

    private func enterForeground(_ harness: SceneHarness, protectedDataAvailable: Bool = true) {
        harness.log.append(harness.session.willEnterForeground(protectedDataAvailable: protectedDataAvailable))
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

    /// The activation persisted exactly one new record, the screen last rendered that record's ID with success,
    /// every rendered ID is a persisted record, and nothing is left pending.
    @discardableResult
    private func assertActivationCommittedOneEntry(
        _ harness: SceneHarness, source: IngressSource, launchKind: IngressLaunchKind, expectedRecordCount: Int,
        file: StaticString = #filePath, line: UInt = #line
    ) throws -> IngressRecord {
        let records = harness.flow.store.allRecords()
        XCTAssertEqual(records.count, expectedRecordCount, file: file, line: line)
        let lastRendered = try XCTUnwrap(harness.log.outcomes.last ?? nil, "the screen must show the entry", file: file, line: line)
        let record = try XCTUnwrap(harness.flow.store.loadRecord(captureId: lastRendered.captureId), file: file, line: line)
        XCTAssertEqual(record.source, source, file: file, line: line)
        XCTAssertEqual(record.launchKind, launchKind, file: file, line: line)
        XCTAssertFalse(lastRendered.status.isFailure, file: file, line: line)
        XCTAssertTrue(lastRendered.displayLines.contains("Entry: \(source.rawValue), \(launchKind.rawValue) launch"), file: file, line: line)
        XCTAssertEqual(harness.log.renderedIds, Set(records.map(\.captureId)), "every rendered ID must be a persisted record", file: file, line: line)
        XCTAssertNil(harness.flow.store.loadPending(), file: file, line: line)
        return record
    }

    func testPlainColdAndWarmLaunchesRenderTheIdleScreenAndCreateNoEntry() {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()
        enterForeground(harness)

        XCTAssertEqual(harness.session.launchKind, .warm)
        XCTAssertEqual(harness.log.outcomes.count, 2)
        XCTAssertTrue(harness.log.outcomes.allSatisfy { $0 == nil })
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)
        XCTAssertNil(harness.flow.store.loadPending())
    }

    func testColdForegroundThenControlIntentYieldsOneRecordAndOneId() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)

        try await performControlAndWaitForRender(harness)

        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 1)
    }

    func testColdControlIntentThenForegroundYieldsOneRecordAndOneId() async throws {
        let harness = makeSceneHarness()
        try await performControlWhileBackgroundedOrBeforeScene()
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)
        XCTAssertTrue(harness.log.outcomes.isEmpty)

        enterForeground(harness)

        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 1)
    }

    func testWarmForegroundThenControlIntentYieldsOneNewRecord() async throws {
        let harness = makeSceneHarness()
        try await performControlWhileBackgroundedOrBeforeScene()
        enterForeground(harness)
        harness.session.didEnterBackground()

        enterForeground(harness)
        try await performControlAndWaitForRender(harness)

        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .warm, expectedRecordCount: 2)
    }

    func testWarmControlIntentWhileBackgroundedThenForegroundYieldsOneNewRecord() async throws {
        let harness = makeSceneHarness()
        try await performControlWhileBackgroundedOrBeforeScene()
        enterForeground(harness)
        harness.session.didEnterBackground()

        try await performControlWhileBackgroundedOrBeforeScene()
        XCTAssertEqual(harness.flow.store.allRecords().count, 1, "a backgrounded scene does not commit")
        enterForeground(harness)

        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .warm, expectedRecordCount: 2)
    }

    func testDelayedSameActivationIntentStillYieldsExactlyOneId() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.clock.advance(10 * 60)

        try await performControlAndWaitForRender(harness)

        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 1)
    }

    func testNearImmediateUnrelatedControlTapGetsItsOwnIdWithoutTouchingTheEarlierRecord() async throws {
        let harness = makeSceneHarness()
        try await performControlWhileBackgroundedOrBeforeScene()
        enterForeground(harness)
        let earlier = try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 1)

        try await performControlAndWaitForRender(harness)

        let later = try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 2)
        XCTAssertNotEqual(later.captureId, earlier.captureId)
        XCTAssertEqual(harness.flow.store.loadRecord(captureId: earlier.captureId), earlier)
    }

    func testAnnouncementRacingTheForegroundCommitRendersTheSameId() throws {
        let harness = makeSceneHarness()
        let handoffId = harness.flow.registerHandoff(source: .controlIntent)
        enterForeground(harness)

        let announced = harness.session.handoffRegistered(captureId: handoffId, source: .controlIntent, protectedDataAvailable: true)

        XCTAssertEqual(announced?.captureId, handoffId)
        XCTAssertEqual(announced?.status, .saved)
        try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .cold, expectedRecordCount: 1)
    }

    func testAnnouncementOfAnIdCommittedInAnEarlierSessionIsAReplay() throws {
        let harness = makeSceneHarness()
        let handoffId = harness.flow.registerHandoff(source: .controlIntent)
        enterForeground(harness)
        harness.session.didEnterBackground()
        enterForeground(harness)

        let announced = try XCTUnwrap(
            harness.session.handoffRegistered(captureId: handoffId, source: .controlIntent, protectedDataAvailable: true)
        )

        XCTAssertEqual(announced.captureId, handoffId)
        XCTAssertEqual(announced.status, .replayed)
        XCTAssertEqual(harness.flow.store.allRecords().map(\.captureId), [handoffId])
    }

    func testControlHandoffInTheBackgroundDoesNotRenderOrCommit() async throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()
        let renderedBefore = harness.log.outcomes.count

        try await performControlWhileBackgroundedOrBeforeScene()

        XCTAssertEqual(harness.log.outcomes.count, renderedBefore)
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)
        XCTAssertNotNil(harness.flow.store.loadPending())
    }

    func testColdShortcutURLIsCommittedByTheForegroundCallback() throws {
        let harness = makeSceneHarness()
        let url = try XCTUnwrap(URL(string: "ohand-captureprobe://capture"))
        XCTAssertNil(harness.session.receive(url: url, protectedDataAvailable: true))
        XCTAssertEqual(harness.flow.store.loadPending()?.source, .shortcutURL)

        enterForeground(harness)

        try assertActivationCommittedOneEntry(harness, source: .shortcutURL, launchKind: .cold, expectedRecordCount: 1)
    }

    func testWarmShortcutURLAfterForegroundCommitsOnce() throws {
        let harness = makeSceneHarness()
        enterForeground(harness)
        harness.session.didEnterBackground()
        enterForeground(harness)
        let url = try XCTUnwrap(URL(string: "ohand-captureprobe://capture"))

        harness.log.append(harness.session.receive(url: url, protectedDataAvailable: true))

        try assertActivationCommittedOneEntry(harness, source: .shortcutURL, launchKind: .warm, expectedRecordCount: 1)
    }

    func testUnrelatedURLsAreIgnored() throws {
        let harness = makeSceneHarness()
        enterForeground(harness)

        for text in ["https://example.com/capture", "ohand-captureprobe://history", "other://capture"] {
            XCTAssertNil(harness.session.receive(url: try XCTUnwrap(URL(string: text)), protectedDataAvailable: true))
        }

        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)
        XCTAssertNil(harness.flow.store.loadPending())
    }

    func testWriteFailureWhileProtectedDataIsUnavailableIsRetriedWithTheSameIdOnTheNextForeground() async throws {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let harness = makeSceneHarness(writer: writer)
        try await performControlWhileBackgroundedOrBeforeScene()
        let handoffId = try XCTUnwrap(harness.flow.store.loadPending()?.captureId)

        enterForeground(harness, protectedDataAvailable: false)
        let failed = try XCTUnwrap(harness.log.outcomes.last ?? nil)
        XCTAssertTrue(failed.status.isFailure)
        XCTAssertEqual(failed.captureId, handoffId)
        XCTAssertTrue(failed.displayLines.contains("Protected data: unavailable"))
        XCTAssertTrue(harness.flow.store.allRecords().isEmpty)

        writer.failRecordWrites = false
        harness.session.didEnterBackground()
        enterForeground(harness, protectedDataAvailable: true)

        let record = try assertActivationCommittedOneEntry(harness, source: .controlIntent, launchKind: .warm, expectedRecordCount: 1)
        XCTAssertEqual(record.captureId, handoffId)
        XCTAssertTrue(record.protectedDataAvailable)
    }

    func testControlIntentAdoptsAnEntryLeftPendingByAFailedCommit() async throws {
        let writer = FlakyWriter()
        writer.failRecordWrites = true
        let harness = makeSceneHarness(writer: writer)
        let url = try XCTUnwrap(URL(string: "ohand-captureprobe://capture"))
        _ = harness.session.receive(url: url, protectedDataAvailable: false)
        enterForeground(harness, protectedDataAvailable: false)
        let strandedId = try XCTUnwrap(harness.flow.store.loadPending()?.captureId)
        writer.failRecordWrites = false

        try await performControlAndWaitForRender(harness)

        let record = try assertActivationCommittedOneEntry(harness, source: .shortcutURL, launchKind: .cold, expectedRecordCount: 1)
        XCTAssertEqual(record.captureId, strandedId)
    }
}
