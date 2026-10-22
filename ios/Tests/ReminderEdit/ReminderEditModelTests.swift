import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Scripted editor: answers reads from a stored reminder and records each command it was sent.
final class FakeReminderEditor: ReminderEditing, @unchecked Sendable {
    struct Sent: Equatable {
        let commandID: String
        let expectedRevision: Int
        let expectedReminderVersion: Int
        let instant: Date?
        let timeZoneID: String?
    }

    private let lock = NSLock()
    private var storedRead: Result<ReminderReadAnswer, Error>
    private var commandResults: [Result<ReminderControlReceipt, Error>] = []
    private var sentCommands: [Sent] = []

    /// Runs inside each command before it answers, so a test can hold a save in flight.
    var beforeAnswer: (@Sendable () async -> Void)?

    init(read: Result<ReminderReadAnswer, Error>) { storedRead = read }

    var sent: [Sent] { locked { sentCommands } }

    func setRead(_ read: Result<ReminderReadAnswer, Error>) { locked { storedRead = read } }
    func script(_ results: [Result<ReminderControlReceipt, Error>]) { locked { commandResults = results } }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }

    func reminder(itemID: String) async throws -> ReminderReadAnswer {
        try locked { storedRead }.get()
    }

    func correctTime(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int, instant: Date,
        timeZoneID: String
    ) async throws -> ReminderControlReceipt {
        await beforeAnswer?()
        return try next(
            Sent(
                commandID: commandID, expectedRevision: expectedRevision,
                expectedReminderVersion: expectedReminderVersion, instant: instant, timeZoneID: timeZoneID))
    }

    func cancel(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int
    ) async throws -> ReminderControlReceipt {
        try next(
            Sent(
                commandID: commandID, expectedRevision: expectedRevision,
                expectedReminderVersion: expectedReminderVersion, instant: nil, timeZoneID: nil))
    }

    private func next(_ command: Sent) throws -> ReminderControlReceipt {
        let result: Result<ReminderControlReceipt, Error>? = locked {
            sentCommands.append(command)
            return commandResults.isEmpty ? nil : commandResults.removeFirst()
        }
        return try (result ?? .failure(ReminderControlError.unreadable)).get()
    }
}

private final class PassCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var stored = 0
    var count: Int { lock.lock(); defer { lock.unlock() }; return stored }
    func increment() { lock.lock(); stored += 1; lock.unlock() }
}

final class ReminderEditModelTests: XCTestCase {
    private static let itemID = "item-1"
    private static let clock = Date(timeIntervalSince1970: 1_800_000_000)

    private func record(
        state: String = "resolved", schedule: String = "pending_schedule", reason: String? = nil,
        due: Date? = nil, zone: String? = "UTC", version: Int = 1, delivery: String = "unknown"
    ) -> ReminderRecordAnswer {
        ReminderRecordAnswer(
            reminderID: "reminder-1", requestState: state, unschedulableReason: reason, scheduleState: schedule,
            deliveryState: delivery, resolvedInstant: due.map(ReminderInstant.string(from:)), timezoneID: zone,
            scheduleGeneration: version, stateVersion: version, updatedAt: "2026-10-01T09:30:00Z")
    }

    private func read(
        _ reminder: ReminderRecordAnswer?, type: String? = "action", lifecycle: String = "active"
    ) -> ReminderReadAnswer {
        ReminderReadAnswer(
            itemID: Self.itemID, itemRevision: 3, itemType: type, lifecycleState: lifecycle, reminder: reminder)
    }

    private func receipt(_ reminder: ReminderRecordAnswer) -> ReminderControlReceipt {
        ReminderControlReceipt(itemID: Self.itemID, reminder: reminder, replayed: false)
    }

    private func make(
        _ editor: FakeReminderEditor, reconcile: @escaping () async -> NotificationPermissionState? = { .usable }
    ) -> ReminderEditModel {
        ReminderEditModel(
            itemID: Self.itemID, editor: editor, reconcile: reconcile, now: { Self.clock },
            deviceTimeZone: { TimeZone(identifier: "UTC")! })
    }

    private func standing(_ reminder: ReminderRecordAnswer?, _ permission: NotificationPermissionState? = .usable)
        -> ReminderStanding
    {
        .of(reminder, permission: permission, now: Self.clock)
    }

    // MARK: honest states

    func testEachUnsupportedOrHeldBackStateIsNamedAndNoneIsCalledInstalled() {
        let future = Self.clock.addingTimeInterval(3600)
        XCTAssertEqual(standing(record(state: "unsupported_recurrence", schedule: "not_scheduled")), .unsupportedRepeat)
        XCTAssertEqual(standing(record(state: "not_scheduled_yet", schedule: "not_scheduled")), .timeNotResolved)
        XCTAssertEqual(
            standing(record(state: "unschedulable", schedule: "not_scheduled", reason: "capacity_exceeded")),
            .heldBackForCapacity)
        XCTAssertEqual(
            standing(record(state: "unschedulable", schedule: "not_scheduled", reason: "time_in_past")),
            .opportunityExpired)
        XCTAssertEqual(standing(record(state: "cancelled", schedule: "not_scheduled")), .cancelled)
        XCTAssertEqual(standing(record(due: future), .denied), .permissionBlocked(.denied))
        XCTAssertEqual(standing(record(due: future), .notRequested), .permissionBlocked(.notRequested))
        XCTAssertEqual(standing(record(schedule: "schedule_failed", due: future)), .installFailed)
        XCTAssertEqual(standing(record(due: future)), .savedAwaitingInstall, "saved is not installed")
        XCTAssertEqual(standing(record(schedule: "scheduled", due: future)), .installed)
        XCTAssertEqual(standing(record(schedule: "scheduled", due: Self.clock.addingTimeInterval(-60))),
                       .passedWithoutDeliveryEvidence)
        XCTAssertEqual(standing(record(schedule: "scheduled", due: Self.clock.addingTimeInterval(-60), delivery: "delivered")),
                       .delivered)
        XCTAssertFalse(ReminderStanding.savedAwaitingInstall.detail.lowercased().contains("installed with"))
    }

    func testTheStoredZoneIsShownAndSeedsTheDraft() async {
        let due = Self.clock.addingTimeInterval(7200)
        let editor = FakeReminderEditor(read: .success(read(record(due: due, zone: "Asia/Tokyo"))))
        let model = make(editor)

        await model.load()

        XCTAssertEqual(model.reminder?.timezoneID, "Asia/Tokyo")
        XCTAssertEqual(model.draftTimeZoneID, "Asia/Tokyo")
        XCTAssertEqual(model.draftDate, Date(timeIntervalSince1970: due.timeIntervalSince1970.rounded(.down)))
    }

    func testTheSectionIsHiddenUntilTheReadSaysTheItemIsAnAction() async {
        let editor = FakeReminderEditor(read: .success(read(nil, type: "note")))
        let model = make(editor)
        XCTAssertFalse(model.isShown)

        await model.load()
        XCTAssertFalse(model.isShown)

        // The item became an action: a reload makes the section appear.
        editor.setRead(.success(read(nil)))
        await model.load()
        XCTAssertTrue(model.isShown)

        // The item stopped being an action: a reload removes it again.
        editor.setRead(.success(read(nil, type: "note")))
        await model.load()
        XCTAssertFalse(model.isShown)
    }

    func testAReloadAfterTheItemClosesMakesTheReminderNotEditable() async {
        let editor = FakeReminderEditor(read: .success(read(record(due: Self.clock.addingTimeInterval(7200)))))
        let model = make(editor)
        await model.load()
        XCTAssertTrue(model.canSave)

        editor.setRead(.success(read(record(due: Self.clock.addingTimeInterval(7200)), lifecycle: "completed")))
        await model.load()

        XCTAssertTrue(model.isShown)
        XCTAssertTrue(model.isClosed)
        XCTAssertFalse(model.canSave)
        XCTAssertFalse(model.canCancel)
    }

    func testAFailedFirstReadDoesNotShowTheSectionButALaterFailureKeepsAnEarlierActionAnswer() async {
        let editor = FakeReminderEditor(read: .failure(ReminderControlError.notAuthenticated))
        let model = make(editor)
        await model.load()
        XCTAssertFalse(model.isShown, "no read has said the item can carry a reminder")

        editor.setRead(.failure(ReminderControlError.unreadable))
        await model.load()
        XCTAssertFalse(model.isShown)

        editor.setRead(.success(read(nil)))
        await model.load()
        XCTAssertTrue(model.isShown)

        editor.setRead(.failure(ReminderControlError.notAuthenticated))
        await model.load()
        XCTAssertTrue(model.isShown, "a locked answer after an Action read is still shown")
        XCTAssertEqual(model.unavailable, .locked)
    }

    func testAnItemThatIsNotAnActionShowsNoSectionAndALockedReadShowsNoReminder() async {
        let notAction = make(FakeReminderEditor(read: .success(read(nil, type: "note"))))
        await notAction.load()
        XCTAssertFalse(notAction.isShown)

        let locked = make(FakeReminderEditor(read: .failure(ReminderControlError.notAuthenticated)))
        await locked.load()
        XCTAssertEqual(locked.unavailable, .locked)
        XCTAssertNil(locked.reminder)
    }

    // MARK: edits

    func testSavingStoresTheEditAsksForAReminderPassAndDoesNotClaimItIsInstalled() async {
        let existing = record(due: Self.clock.addingTimeInterval(3600), version: 4)
        let editor = FakeReminderEditor(read: .success(read(existing)))
        let moved = record(due: Self.clock.addingTimeInterval(86_400), version: 5)
        editor.script([.success(receipt(moved))])
        let passes = PassCounter()
        let model = make(editor, reconcile: { passes.increment(); return .usable })
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(86_400)
        model.draftTimeZoneID = "Europe/Paris"
        editor.setRead(.success(read(moved)))

        await model.save()

        let sent = editor.sent
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent.first?.expectedRevision, 3)
        XCTAssertEqual(sent.first?.expectedReminderVersion, 4, "the edit names the version the user saw")
        XCTAssertEqual(sent.first?.timeZoneID, "Europe/Paris")
        XCTAssertEqual(passes.count, 2, "one pass when the stored reminder was read, one after the edit")
        XCTAssertEqual(model.message, .saved)
        XCTAssertEqual(model.standing, .savedAwaitingInstall)
    }

    func testAPastTimeIsRefusedWithoutSendingAnything() async {
        let editor = FakeReminderEditor(read: .success(read(nil)))
        let model = make(editor)
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(-60)

        await model.save()

        XCTAssertEqual(model.message, .timeInPast)
        XCTAssertTrue(editor.sent.isEmpty)
    }

    func testRetryAfterAnUnconfirmedAnswerReusesTheCommandAndAChangedChoiceUsesANewOne() async {
        let editor = FakeReminderEditor(read: .success(read(nil)))
        let stored = record(due: Self.clock.addingTimeInterval(7200), version: 1)
        editor.script([
            .failure(ReminderControlError.unreadable), .success(receipt(stored)),
        ])
        let model = make(editor)
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(7200)

        await model.save()
        XCTAssertEqual(model.message, .notConfirmed)
        await model.save()

        let sent = editor.sent
        XCTAssertEqual(sent.count, 2)
        XCTAssertEqual(sent[0].commandID, sent[1].commandID, "one desired reminder: the retry repeats the command")

        editor.script([.failure(ReminderControlError.unreadable)])
        model.draftDate = Self.clock.addingTimeInterval(9000)
        await model.save()
        XCTAssertNotEqual(editor.sent[2].commandID, sent[1].commandID)
    }

    func testAStaleVersionReloadsTheStoredReminderAndKeepsTheUsersChoiceVisible() async {
        let seen = record(due: Self.clock.addingTimeInterval(3600), version: 1)
        let editor = FakeReminderEditor(read: .success(read(seen)))
        let newer = record(due: Self.clock.addingTimeInterval(5400), version: 2)
        editor.script([.failure(ReminderControlError.staleReminderVersion(currentVersion: 2))])
        let model = make(editor)
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(7200)
        editor.setRead(.success(read(newer)))

        await model.save()

        XCTAssertEqual(model.message, .changedElsewhere)
        XCTAssertEqual(model.reminder?.stateVersion, 2)
        XCTAssertEqual(model.draftDate, Self.clock.addingTimeInterval(7200), "the draft is not overwritten")
        editor.script([.success(receipt(record(due: Self.clock.addingTimeInterval(7200), version: 3)))])
        await model.save()
        XCTAssertEqual(editor.sent.last?.expectedReminderVersion, 2, "the next edit names the reloaded version")
    }

    func testCancelSendsTheSeenVersionAndShowsCancelled() async {
        let existing = record(schedule: "scheduled", due: Self.clock.addingTimeInterval(3600), version: 2)
        let editor = FakeReminderEditor(read: .success(read(existing)))
        let cancelled = record(state: "cancelled", schedule: "not_scheduled", version: 3)
        editor.script([.success(receipt(cancelled))])
        let model = make(editor)
        await model.load()
        editor.setRead(.success(read(cancelled)))

        await model.cancel()

        XCTAssertEqual(editor.sent.first?.expectedReminderVersion, 2)
        XCTAssertNil(editor.sent.first?.instant)
        XCTAssertEqual(model.standing, .cancelled)
        XCTAssertFalse(model.canCancel, "nothing is left to cancel")
    }

    func testARelockDuringAnEditClearsTheReminder() async {
        let editor = FakeReminderEditor(read: .success(read(record(due: Self.clock.addingTimeInterval(3600)))))
        editor.script([.failure(ReminderControlError.notAuthenticated)])
        let model = make(editor)
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(7200)

        await model.save()

        XCTAssertEqual(model.unavailable, .locked)
        XCTAssertNil(model.reminder)
    }

    func testARelockWhileASaveIsInFlightDoesNotBringTheReminderBack() async {
        let editor = FakeReminderEditor(read: .success(read(record(due: Self.clock.addingTimeInterval(3600)))))
        editor.script([.success(receipt(record(due: Self.clock.addingTimeInterval(7200), version: 2)))])
        let model = make(editor)
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(7200)
        let reachedSend = expectation(description: "the save reached the editor")
        let release = AsyncGate()
        editor.beforeAnswer = {
            reachedSend.fulfill()
            await release.wait()
        }

        let saving = Task { @MainActor in await model.save() }
        await fulfillment(of: [reachedSend], timeout: 5)
        await model.lock()
        await release.open()
        await saving.value

        XCTAssertEqual(model.unavailable, .locked)
        XCTAssertNil(model.reminder)
        XCTAssertNil(model.message)
        XCTAssertNil(model.permission)
    }

    func testAPassedStoredTimeAndAStaleReloadShowTheStoredInstantNotTheDraft() async {
        let passed = Self.clock.addingTimeInterval(-3600)
        let editor = FakeReminderEditor(
            read: .success(read(record(state: "unschedulable", reason: "time_in_past", due: passed, zone: "Asia/Tokyo"))))
        let model = make(editor)
        await model.load()

        XCTAssertTrue(ReminderStoredTime.text(for: model.reminder)?.hasPrefix("Stored time: 2027-01-15 16:00 ") == true)
        XCTAssertGreaterThan(model.draftDate, Self.clock, "the draft is a later suggestion, not the stored time")

        let seen = record(due: Self.clock.addingTimeInterval(3600), version: 1)
        let newer = record(due: Self.clock.addingTimeInterval(5400), version: 2)
        editor.setRead(.success(read(seen)))
        await model.load()
        model.draftDate = Self.clock.addingTimeInterval(7200)
        editor.script([.failure(ReminderControlError.staleReminderVersion(currentVersion: 2))])
        editor.setRead(.success(read(newer)))
        await model.save()
        XCTAssertEqual(model.reminder?.resolvedDate, Self.clock.addingTimeInterval(5400))
        XCTAssertNotEqual(model.draftDate, model.reminder?.resolvedDate)
        XCTAssertTrue(
            ReminderStoredTime.text(for: model.reminder)?.hasPrefix("Stored time: 2027-01-15 09:30 ") == true,
            "the newer stored instant, 90 minutes after the clock, is shown")
    }

    func testAClosedItemDisablesEditsAndStaysDisabledAfterTheCoreRefusesOne() async {
        let editor = FakeReminderEditor(
            read: .success(read(record(due: Self.clock.addingTimeInterval(3600)), lifecycle: "completed")))
        let model = make(editor)
        await model.load()
        XCTAssertTrue(model.isClosed)
        XCTAssertFalse(model.canSave)
        XCTAssertFalse(model.canCancel)
        XCTAssertEqual(model.shownMessage, .itemClosed)

        let open = FakeReminderEditor(read: .success(read(record(due: Self.clock.addingTimeInterval(3600)))))
        open.script([.failure(ReminderControlError.itemNotActive(lifecycleState: "cancelled"))])
        let racing = make(open)
        await racing.load()
        XCTAssertTrue(racing.canSave)
        open.setRead(.success(read(record(due: Self.clock.addingTimeInterval(3600)), lifecycle: "cancelled")))
        await racing.save()
        XCTAssertEqual(racing.message, .itemClosed)
        XCTAssertFalse(racing.canSave, "the reloaded record is closed, so the refused command cannot be repeated")
    }

    func testAFreshOpenOfAResolvedReminderShowsThePermissionState() async {
        let editor = FakeReminderEditor(read: .success(read(record(due: Self.clock.addingTimeInterval(3600)))))
        let model = make(editor, reconcile: { .denied })
        await model.load()
        XCTAssertEqual(model.standing, .permissionBlocked(.denied))
    }
}

/// Holds a suspended call until the test opens it.
actor AsyncGate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        waiters.forEach { $0.resume() }
        waiters = []
    }
}
