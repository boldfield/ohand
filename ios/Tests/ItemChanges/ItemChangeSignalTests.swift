import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class ChangeRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [ItemChange] = []
    private var threads: [Bool] = []

    func record(_ change: ItemChange) {
        lock.lock()
        stored.append(change)
        threads.append(Thread.isMainThread)
        lock.unlock()
    }

    var changes: [ItemChange] {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    var allOnMainThread: Bool {
        lock.lock()
        defer { lock.unlock() }
        return threads.allSatisfy { $0 }
    }
}

private final class ScriptedCorrectionCore: ItemCorrectionCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<ItemCorrectionAnswer, CoreFailure>]

    init(_ script: [Result<ItemCorrectionAnswer, CoreFailure>]) { self.script = script }

    func submit(
        _ command: ItemCorrectionCommand,
        completion: @escaping @Sendable (Result<ItemCorrectionAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreItemCorrectionClient.unreadableAnswer))
    }
}

private final class ScriptedActionCore: NotificationActionCore, @unchecked Sendable {
    let completeOutcome: String
    let resolvedItemID: String?
    /// How many lookups fail before one answers.
    var failingResolves = 0
    private(set) var completions = 0
    init(completeOutcome: String, resolvedItemID: String? = nil) {
        self.completeOutcome = completeOutcome
        self.resolvedItemID = resolvedItemID
    }

    func recordEvidence(
        notificationID: String, kind: NotificationActionCommand.Evidence, observedAt: Date, itemID: String?
    ) async throws -> NotificationActionAnswer { NotificationActionAnswer(outcome: "recorded") }

    func acknowledge(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        NotificationActionAnswer(outcome: "recorded")
    }

    func complete(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer {
        completions += 1
        return NotificationActionAnswer(outcome: completeOutcome)
    }

    func resolve(notificationID: String) async throws -> NotificationActionAnswer {
        if failingResolves > 0 {
            failingResolves -= 1
            throw CoreFailure(errorClass: .transient, code: "test_timeout", message: "no event for the operation")
        }
        return resolvedItemID.map { NotificationActionAnswer(outcome: "resolved", itemID: $0) }
            ?? NotificationActionAnswer(outcome: "unknown_notification")
    }
}

final class ItemChangeSignalTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func authenticatedSession() -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        return session
    }

    private func flushMainQueue() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            DispatchQueue.main.async { continuation.resume() }
        }
    }

    private func corrected() -> ItemCorrectionAnswer {
        ItemCorrectionAnswer(outcome: "corrected", itemID: "item-1", revision: 3, replayed: false, reminderCancelled: false)
    }

    func testAMainThreadPublishReachesSubscribersBeforeItReturnsAndRemovalStopsIt() {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        let remove = signal.observe { recorder.record($0) }
        signal.publish(.item("item-1"))
        XCTAssertEqual(recorder.changes, [.item("item-1")])
        remove()
        signal.publish(.allItems)
        XCTAssertEqual(recorder.changes, [.item("item-1")])
    }

    func testAPublishFromAnotherThreadIsDeliveredOnTheMainThread() {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        _ = signal.observe { recorder.record($0) }
        let delivered = expectation(description: "delivered")
        DispatchQueue.global().async {
            signal.publish(.item("item-2"))
            DispatchQueue.main.async { delivered.fulfill() }
        }
        wait(for: [delivered], timeout: 5)
        XCTAssertEqual(recorder.changes, [.item("item-2")])
        XCTAssertTrue(recorder.allOnMainThread)
    }

    func testEveryChangeIsAlsoPostedOnTheMainThreadCarryingOnlyTheChange() {
        let center = NotificationCenter()
        let signal = ItemChangeSignal(notificationCenter: center)
        let recorder = ChangeRecorder()
        let extraKeys = ChangeRecorder()
        let token = center.addObserver(forName: ItemChangeSignal.didChangeNotification, object: signal, queue: nil) {
            guard let change = ItemChangeSignal.change(in: $0) else { return }
            recorder.record(change)
            // Anything besides the change itself would be recorded here.
            if ($0.userInfo?.count ?? 0) != 1 { extraKeys.record(change) }
        }
        defer { center.removeObserver(token) }

        signal.publish(.item("item-3"))
        let delivered = expectation(description: "delivered")
        DispatchQueue.global().async {
            signal.publish(.allItems)
            DispatchQueue.main.async { delivered.fulfill() }
        }
        wait(for: [delivered], timeout: 5)

        XCTAssertEqual(recorder.changes, [.item("item-3"), .allItems])
        XCTAssertTrue(recorder.allOnMainThread)
        XCTAssertEqual(extraKeys.changes, [], "the notification carries nothing but the change")
        XCTAssertNil(ItemChangeSignal.change(
            in: Notification(name: Notification.Name("other"), object: nil, userInfo: ["change": ItemChange.allItems])))
    }

    func testAChangeConcernsItsOwnItemInAnyCaseAndAResetConcernsEveryItem() {
        XCTAssertTrue(ItemChange.item("ABC-1").concerns("abc-1"))
        XCTAssertFalse(ItemChange.item("abc-1").concerns("abc-2"))
        XCTAssertTrue(ItemChange.allItems.concerns("abc-2"))
    }

    func testAStoredCorrectionPublishesOnlyTheItemIdentifier() async throws {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        _ = signal.observe { recorder.record($0) }
        let service = ItemCorrectionService(
            core: ScriptedCorrectionCore([.success(corrected())]), session: authenticatedSession(), changes: signal)
        _ = try await service.correctText(commandID: "c-1", itemID: "item-1", expectedRevision: 2, text: "secret words")
        await flushMainQueue()
        XCTAssertEqual(recorder.changes, [.item("item-1")])
    }

    func testARefusalPublishesNothing() async {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        _ = signal.observe { recorder.record($0) }
        let stale = ItemCorrectionAnswer(
            outcome: "stale_revision", itemID: nil, revision: nil, replayed: nil, reminderCancelled: nil,
            currentRevision: 9)
        let service = ItemCorrectionService(
            core: ScriptedCorrectionCore([.success(stale), .failure(CoreItemCorrectionClient.unreadableAnswer)]),
            session: authenticatedSession(), changes: signal)
        do {
            _ = try await service.correctText(commandID: "c-1", itemID: "item-1", expectedRevision: 2, text: "x")
            XCTFail("a stale revision is refused")
        } catch {}
        do {
            _ = try await service.correctText(commandID: "c-2", itemID: "item-1", expectedRevision: 9, text: "x")
            XCTFail("a failure is refused")
        } catch {}
        await flushMainQueue()
        XCTAssertTrue(recorder.changes.isEmpty)
    }

    func testANotificationActionCompletionPublishesOnlyWhenTheCoreCompleted() async throws {
        for (outcome, expected) in [("completed", [ItemChange.item("item-3")]), ("already_ended", [])] {
            let signal = ItemChangeSignal()
            let recorder = ChangeRecorder()
            _ = signal.observe { recorder.record($0) }
            let core = ChangePublishingNotificationActionCore(
                base: ScriptedActionCore(completeOutcome: outcome), changes: signal)
            _ = try await core.complete(notificationID: "n-1", itemID: "item-3")
            await flushMainQueue()
            XCTAssertEqual(recorder.changes, expected, outcome)
        }
    }

    func testACompletionWithoutAPayloadItemPublishesTheResolvedItemNeverAllItems() async throws {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        _ = signal.observe { recorder.record($0) }
        let core = ChangePublishingNotificationActionCore(
            base: ScriptedActionCore(completeOutcome: "completed", resolvedItemID: "item-9"), changes: signal)
        _ = try await core.complete(notificationID: "n-1", itemID: nil)
        await flushMainQueue()
        XCTAssertEqual(recorder.changes, [.item("item-9")])

        let unresolved = ChangePublishingNotificationActionCore(
            base: ScriptedActionCore(completeOutcome: "completed"), changes: signal)
        _ = try await unresolved.complete(notificationID: "n-2", itemID: nil)
        await flushMainQueue()
        XCTAssertEqual(recorder.changes, [.item("item-9")])
    }

    func testAFailedLookupCompletesNothingSoTheRetryCompletesAndAnnouncesTheItem() async throws {
        let signal = ItemChangeSignal()
        let recorder = ChangeRecorder()
        _ = signal.observe { recorder.record($0) }
        let base = ScriptedActionCore(completeOutcome: "completed", resolvedItemID: "item-9")
        base.failingResolves = 1
        let core = ChangePublishingNotificationActionCore(base: base, changes: signal)

        do {
            _ = try await core.complete(notificationID: "n-1", itemID: nil)
            XCTFail("a failed lookup is reported, so the handler keeps the entry")
        } catch let failure as CoreFailure {
            XCTAssertEqual(failure.errorClass, .transient, "the handler retains a transient failure for a retry")
        }
        await flushMainQueue()
        XCTAssertEqual(base.completions, 0, "nothing was completed without knowing which item it was")
        XCTAssertTrue(recorder.changes.isEmpty)

        let answer = try await core.complete(notificationID: "n-1", itemID: nil)
        await flushMainQueue()
        XCTAssertEqual(answer.outcome, "completed")
        XCTAssertEqual(base.completions, 1)
        XCTAssertEqual(recorder.changes, [.item("item-9")], "the retried completion is announced")
    }
}
