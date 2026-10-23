import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A real session over an immediately completing boundary, so tests exercise the epoch-bound authorization.
final class FakeActionAuthorization: ActionAuthorizing {
    let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())

    var isAuthenticated: Bool {
        get { session.scope == .authenticated }
        set {
            if newValue {
                session.authenticate(reason: "test") { _ in }
            } else {
                session.relock(.explicit)
            }
        }
    }

    func currentAuthorization() -> ReadAuthorization? { session.currentAuthorization() }
    func isCurrent(_ authorization: ReadAuthorization) -> Bool { session.isCurrent(authorization) }
}

final class FakeActionCore: NotificationActionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var recordedCalls: [String] = []
    var evidenceAnswer = NotificationActionAnswer(outcome: "recorded")
    var acknowledgeAnswer = NotificationActionAnswer(outcome: "recorded")
    var completeAnswer = NotificationActionAnswer(outcome: "completed")
    var resolveAnswer = NotificationActionAnswer(
        outcome: "item", itemID: "item-from-core", lifecycle: "active", currentGeneration: true)
    var failure: CoreFailure?
    var onResolve: (() -> Void)?

    var calls: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recordedCalls
    }

    private func record(_ call: String) throws {
        lock.lock()
        recordedCalls.append(call)
        let pendingFailure = failure
        lock.unlock()
        if let pendingFailure { throw pendingFailure }
    }

    func recordEvidence(
        notificationID: String, kind: NotificationActionCommand.Evidence, observedAt: Date, itemID: String?
    ) async throws -> NotificationActionAnswer {
        try record("evidence:\(kind.rawValue):\(notificationID)")
        return evidenceAnswer
    }

    func acknowledge(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        try record("acknowledge:\(notificationID)")
        return acknowledgeAnswer
    }

    func complete(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        try record("complete:\(notificationID)")
        return completeAnswer
    }

    func resolve(notificationID: String) async throws -> NotificationActionAnswer {
        try record("resolve:\(notificationID)")
        onResolve?()
        return resolveAnswer
    }
}

final class InMemoryActionJournal: NotificationActionJournal, @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [String: NotificationActionEntry] = [:]
    var failsAppends = false
    var failsRemovals = false
    var failsReads = false

    var stored: [NotificationActionEntry] {
        lock.lock()
        defer { lock.unlock() }
        return entries.values.sorted(by: NotificationActionEntry.replayOrder)
    }

    func append(_ entry: NotificationActionEntry) throws {
        lock.lock()
        defer { lock.unlock() }
        if failsAppends { throw CocoaError(.fileWriteOutOfSpace) }
        entries[entry.entryID] = entries[entry.entryID] ?? entry
    }

    func pending() throws -> [NotificationActionEntry] {
        if failsReads { throw CocoaError(.fileReadNoPermission) }
        return stored
    }

    func remove(_ entry: NotificationActionEntry) throws {
        lock.lock()
        defer { lock.unlock() }
        if failsRemovals { throw CocoaError(.fileWriteNoPermission) }
        entries.removeValue(forKey: entry.entryID)
    }
}

private final class OpenedItems: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [String] = []

    func add(_ item: String) {
        lock.lock()
        items.append(item)
        lock.unlock()
    }

    var all: [String] {
        lock.lock()
        defer { lock.unlock() }
        return items
    }
}

final class ActionHandlerTests: XCTestCase {
    private let current = NotificationIdentifier(reminderID: "rem-1", scheduleGeneration: 2)!
    private let target = OpaqueIdentifier("item-1")!

    private struct Rig {
        let handler: ActionHandler
        let core: FakeActionCore
        let journal: InMemoryActionJournal
        let authorization: FakeActionAuthorization
        fileprivate let opened: OpenedItems
        var openedItems: [String] { opened.all }
    }

    private func makeRig(
        journal: InMemoryActionJournal = InMemoryActionJournal(),
        requestUnlock: @escaping @Sendable () -> Void = {}
    ) -> Rig {
        let core = FakeActionCore()
        let authorization = FakeActionAuthorization()
        let opened = OpenedItems()
        let handler = ActionHandler(
            core: core, journal: journal, authorization: authorization, openItem: { opened.add($0) },
            requestUnlock: requestUnlock, startsDeliveryOnIngest: false)
        return Rig(handler: handler, core: core, journal: journal, authorization: authorization, opened: opened)
    }

    private func event(
        _ kind: NotificationEventKind,
        identifier: NotificationIdentifier? = nil,
        at seconds: TimeInterval = 1_790_000_000
    ) -> NotificationEvent {
        NotificationEvent(
            identifier: identifier ?? current, kind: kind, opaqueTargetID: target,
            occurredAt: Date(timeIntervalSince1970: seconds))
    }

    private func action(_ raw: String) -> NotificationEventKind { .action(OpaqueIdentifier(raw)!) }

    func testDeliveryEvidenceIsStoredAndRecordedWithoutAuthentication() async {
        let rig = makeRig()
        XCTAssertEqual(rig.handler.ingest(event(.delivered)), .persisted)
        let report = await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["evidence:delivered:rem-1#2"])
        XCTAssertEqual(report.settled, 1)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testAcknowledgeAndCompleteAreDistinctOperations() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        rig.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["acknowledge:rem-1#2"], "acknowledging never completes")

        rig.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["acknowledge:rem-1#2", "complete:rem-1#2"], "completing never acknowledges")
    }

    func testCompleteWaitsForUnlockAndThenRunsOnce() async {
        let rig = makeRig()
        XCTAssertEqual(rig.handler.ingest(event(action(NotificationActionIdentifier.complete))), .persisted)
        let locked = await rig.handler.reconcile()
        XCTAssertEqual(locked.deferred, 1)
        XCTAssertTrue(rig.core.calls.isEmpty, "no mutation is sent while the session is capture-only")
        XCTAssertEqual(rig.journal.stored.count, 1, "the action is kept for the next unlock")

        rig.authorization.isAuthenticated = true
        let unlocked = await rig.handler.reconcile()
        XCTAssertEqual(unlocked.settled, 1)
        XCTAssertEqual(rig.core.calls, ["complete:rem-1#2"])
        let again = await rig.handler.reconcile()
        XCTAssertEqual(again, NotificationActionReconciliation())
        XCTAssertEqual(rig.core.calls.count, 1)
    }

    func testDuplicateCallbacksWhileWaitingCoalesceIntoOneCoreCall() async {
        let rig = makeRig()
        for _ in 0..<3 { rig.handler.ingest(event(action(NotificationActionIdentifier.complete))) }
        XCTAssertEqual(rig.journal.stored.count, 1)
        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls.filter { $0.hasPrefix("complete") }.count, 1)
    }

    func testRepeatAfterSettlingIsAnsweredByTheCoreAndLeavesNothingBehind() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        rig.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        await rig.handler.reconcile()
        rig.core.completeAnswer = NotificationActionAnswer(outcome: "already_ended")
        rig.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        let report = await rig.handler.reconcile()
        XCTAssertEqual(report.settled, 1, "the core's no-change answer settles the repeat")
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testStaleAndDeletedAnswersSettleTheEntry() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        for outcome in ["stale_notification", "item_deleted", "unknown_notification", "item_mismatch", "not_installed"] {
            rig.core.completeAnswer = NotificationActionAnswer(outcome: outcome)
            rig.handler.ingest(event(action(NotificationActionIdentifier.complete)))
            let report = await rig.handler.reconcile()
            XCTAssertEqual(report.settled, 1, outcome)
        }
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testCallbackThatCannotBeStoredReportsFailureAndNeverReachesTheCore() async {
        let journal = InMemoryActionJournal()
        journal.failsAppends = true
        let core = FakeActionCore()
        let reported = OpenedItems()
        let handler = ActionHandler(
            core: core, journal: journal, authorization: FakeActionAuthorization(), openItem: { _ in },
            ingestFailure: { reported.add($0.identifier.rawValue) }, startsDeliveryOnIngest: false)
        XCTAssertEqual(handler.ingest(event(action(NotificationActionIdentifier.acknowledge))), .failed)
        await handler.reconcile()
        XCTAssertEqual(reported.all, ["rem-1#2"])
        XCTAssertTrue(core.calls.isEmpty)
    }

    func testIngestStartsDeliveryWithoutAnExplicitReconcile() async {
        let core = FakeActionCore()
        let handler = ActionHandler(
            core: core, journal: InMemoryActionJournal(), authorization: FakeActionAuthorization(), openItem: { _ in })
        XCTAssertEqual(handler.ingest(event(.delivered)), .persisted)
        for _ in 0..<500 where core.calls.isEmpty { try? await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(core.calls, ["evidence:delivered:rem-1#2"])
    }

    func testUnknownActionIsIgnoredAndStoresNothing() async {
        let rig = makeRig()
        XCTAssertEqual(rig.handler.ingest(event(action("ohand.action.unknown"))), .ignored)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testCoreOutageKeepsTheEntryAndALaterPassDeliversIt() async {
        let rig = makeRig()
        rig.core.failure = CoreFailure(errorClass: .transient, code: "storage_busy", message: "busy")
        rig.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        let failed = await rig.handler.reconcile()
        XCTAssertEqual(failed.retained, 1)
        XCTAssertEqual(rig.journal.stored.count, 1)

        rig.core.failure = nil
        let recovered = await rig.handler.reconcile()
        XCTAssertEqual(recovered.settled, 1)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testPermanentRefusalDiscardsTheEntryInsteadOfReplayingForever() async {
        let rig = makeRig()
        rig.core.failure = CoreFailure(errorClass: .permanent, code: "invalid_request", message: "invalid")
        rig.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        let report = await rig.handler.reconcile()
        XCTAssertEqual(report.discarded, 1)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testEntryThatCannotBeRemovedReplaysIdempotentlyNotAsANewMutation() async {
        let rig = makeRig()
        rig.journal.failsRemovals = true
        rig.handler.ingest(event(action(NotificationActionIdentifier.acknowledge)))
        let first = await rig.handler.reconcile()
        XCTAssertEqual(first.retained, 1)
        rig.journal.failsRemovals = false
        rig.core.acknowledgeAnswer = NotificationActionAnswer(outcome: "already_recorded")
        let second = await rig.handler.reconcile()
        XCTAssertEqual(second.settled, 1)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testTapRecordsEvidenceNowAndRoutesOnlyAfterUnlockUsingTheCoreResolvedItem() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        let locked = await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["evidence:opened:rem-1#2"], "no private read while locked")
        XCTAssertEqual(locked.deferred, 1)
        XCTAssertTrue(rig.openedItems.isEmpty)

        rig.authorization.isAuthenticated = true
        let unlocked = await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["evidence:opened:rem-1#2", "resolve:rem-1#2"])
        XCTAssertEqual(unlocked.openedItemIDs, ["item-from-core"])
        XCTAssertEqual(rig.openedItems, ["item-from-core"], "the route is the core's item, not the payload target")
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testALockedTapAsksToUnlockOncePerPassAndOpensAfterSuccess() async {
        let asks = OpenedItems()
        let rig = makeRig(requestUnlock: { asks.add("unlock") })
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        XCTAssertEqual(asks.all, ["unlock"])

        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()
        XCTAssertEqual(rig.openedItems, ["item-from-core"])
        XCTAssertEqual(asks.all, ["unlock"], "an authenticated pass does not ask again")
    }

    func testDiscardingHeldOpensMeansALaterUnlockReplaysNothing() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        XCTAssertEqual(rig.journal.stored.count, 1)

        await rig.handler.discardHeldOpens()
        XCTAssertTrue(rig.journal.stored.isEmpty)

        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()
        XCTAssertTrue(rig.openedItems.isEmpty, "the dropped tap is not replayed by an unrelated unlock")
        XCTAssertEqual(rig.core.calls, ["evidence:opened:rem-1#2"])
    }

    func testDroppedOpenIsNotReplayedWhenAnUnrelatedAuthenticationWinsTheRace() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        rig.authorization.isAuthenticated = true

        await rig.handler.discardHeldOpens()
        await rig.handler.reconcile()

        XCTAssertTrue(rig.openedItems.isEmpty, "the person declined, so a session that authenticated first opens nothing")
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testDroppedOpenSurvivesAJournalThatCannotRemoveIt() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        rig.journal.failsRemovals = true

        await rig.handler.discardHeldOpens()
        XCTAssertEqual(
            rig.journal.stored.map(\.operation), [.discardOpen, .open],
            "the removal failed, so the entry is still stored, with a durable discard beside it")

        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()
        XCTAssertTrue(rig.openedItems.isEmpty, "a stored but dropped open is not replayed")

        rig.journal.failsRemovals = false
        await rig.handler.reconcile()
        XCTAssertTrue(rig.journal.stored.isEmpty)
        XCTAssertTrue(rig.openedItems.isEmpty)
    }

    func testDroppedOpenIsNotReplayedAfterARelaunchWhenTheRemovalFailed() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        rig.journal.failsRemovals = true
        await rig.handler.discardHeldOpens()

        // A new process: nothing in memory, only the journal.
        rig.journal.failsRemovals = false
        let relaunched = ActionHandler(
            core: rig.core, journal: rig.journal, authorization: rig.authorization,
            openItem: { [opened = rig.opened] in opened.add($0) })
        rig.authorization.isAuthenticated = true
        await relaunched.reconcile()

        XCTAssertTrue(rig.openedItems.isEmpty, "the durable discard outlives the process")
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testDroppedOpenIsNotReplayedWhenTheJournalCouldNotBeReadAtDiscardTime() async {
        let rig = makeRig()
        rig.handler.ingest(event(.opened))
        await rig.handler.reconcile()
        rig.journal.failsReads = true
        await rig.handler.discardHeldOpens()

        rig.journal.failsReads = false
        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()

        XCTAssertTrue(rig.openedItems.isEmpty, "the unread discard still covers the open stored at that time")
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testATapAfterAnUnreadDiscardIsStillOpened() async {
        let rig = makeRig()
        rig.journal.failsReads = true
        await rig.handler.discardHeldOpens()
        rig.handler.ingest(event(.opened))

        rig.journal.failsReads = false
        rig.authorization.isAuthenticated = true
        await rig.handler.reconcile()

        XCTAssertEqual(rig.openedItems, ["item-from-core"], "a new tap is a new decision to open")
    }

    func testDiscardingHeldOpensKeepsOtherDeferredWork() async {
        let rig = makeRig()
        rig.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        rig.handler.ingest(event(.opened, identifier: NotificationIdentifier(reminderID: "rem-2", scheduleGeneration: 1)!))
        await rig.handler.reconcile()
        await rig.handler.discardHeldOpens()
        XCTAssertEqual(rig.journal.stored.map(\.operation), [.complete])
    }

    func testTapForDeletedOrUnknownNotificationOpensNothing() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        for outcome in ["item_deleted", "unknown_notification"] {
            rig.core.resolveAnswer = NotificationActionAnswer(outcome: outcome)
            rig.handler.ingest(event(.opened))
            await rig.handler.reconcile()
        }
        XCTAssertTrue(rig.openedItems.isEmpty)
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testRelockWhileResolvingKeepsTheTapAndShowsNothing() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        rig.core.onResolve = { rig.authorization.isAuthenticated = false }
        rig.handler.ingest(event(.opened))
        let report = await rig.handler.reconcile()
        XCTAssertEqual(report.deferred, 1)
        XCTAssertTrue(rig.openedItems.isEmpty)
        XCTAssertEqual(rig.journal.stored.map(\.operation), [.open])
    }

    func testRelockAndReauthenticationWhileResolvingStillDropsThePreRelockAnswer() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        rig.core.onResolve = {
            rig.authorization.isAuthenticated = false
            rig.authorization.isAuthenticated = true
        }
        rig.handler.ingest(event(.opened))
        let report = await rig.handler.reconcile()
        XCTAssertTrue(rig.authorization.isAuthenticated)
        XCTAssertEqual(report.deferred, 1)
        XCTAssertTrue(rig.openedItems.isEmpty)
        XCTAssertEqual(rig.journal.stored.map(\.operation), [.open])

        rig.core.onResolve = nil
        let replay = await rig.handler.reconcile()
        XCTAssertEqual(replay.openedItemIDs, ["item-from-core"])
        XCTAssertEqual(rig.openedItems, ["item-from-core"])
        XCTAssertTrue(rig.journal.stored.isEmpty)
    }

    func testEntriesLeftByAnEarlierLaunchAreReconciledByANewHandler() async {
        let journal = InMemoryActionJournal()
        let first = makeRig(journal: journal)
        first.handler.ingest(event(action(NotificationActionIdentifier.complete)))
        await first.handler.reconcile()
        XCTAssertEqual(journal.stored.count, 1)

        let relaunched = makeRig(journal: journal)
        relaunched.authorization.isAuthenticated = true
        let report = await relaunched.handler.reconcile()
        XCTAssertEqual(report.settled, 1)
        XCTAssertEqual(relaunched.core.calls, ["complete:rem-1#2"])
    }

    func testEvidenceReplaysBeforeTheUsersDecision() async {
        let rig = makeRig()
        rig.authorization.isAuthenticated = true
        rig.handler.ingest(event(action(NotificationActionIdentifier.complete), at: 1_790_000_002))
        rig.handler.ingest(event(.delivered, at: 1_790_000_001))
        await rig.handler.reconcile()
        XCTAssertEqual(rig.core.calls, ["evidence:delivered:rem-1#2", "complete:rem-1#2"])
    }
}

final class FileNotificationActionJournalTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-action-journal-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private func entry(
        _ operation: NotificationActionEntry.Operation, notification: String = "rem-1#2", at milliseconds: Int64 = 10
    ) -> NotificationActionEntry {
        NotificationActionEntry(
            operation: operation, notificationID: notification, targetID: "item-1",
            occurredAtUnixMilliseconds: milliseconds)
    }

    func testEntriesSurviveANewJournalInstanceAndCoalesceDuplicates() throws {
        let journal = FileNotificationActionJournal(directory: directory)
        XCTAssertEqual(try journal.pending(), [])
        try journal.append(entry(.complete))
        try journal.append(entry(.complete, at: 99))
        try journal.append(entry(.acknowledge))

        let reopened = FileNotificationActionJournal(directory: directory)
        let pending = try reopened.pending()
        XCTAssertEqual(pending.map(\.operation), [.acknowledge, .complete])
        XCTAssertEqual(pending.first { $0.operation == .complete }?.occurredAtUnixMilliseconds, 10)
    }

    func testRemoveIsIdempotentAndIgnoresUnfinishedWrites() throws {
        let journal = FileNotificationActionJournal(directory: directory)
        try journal.append(entry(.open))
        try Data("partial".utf8).write(to: directory.appendingPathComponent("junk.action.json.incomplete"))
        try Data("not json".utf8).write(to: directory.appendingPathComponent("broken.action.json"))
        XCTAssertEqual(try journal.pending().map(\.operation), [.open])
        try journal.remove(entry(.open))
        try journal.remove(entry(.open))
        XCTAssertEqual(try journal.pending(), [])
    }

    func testAppendFailsLoudlyWhenTheDirectoryCannotBeCreated() throws {
        let blocker = directory.deletingLastPathComponent().appendingPathComponent("blocker-\(UUID().uuidString)")
        try Data().write(to: blocker)
        defer { try? FileManager.default.removeItem(at: blocker) }
        let journal = FileNotificationActionJournal(directory: blocker.appendingPathComponent("journal"))
        XCTAssertThrowsError(try journal.append(entry(.complete)))
    }
}
