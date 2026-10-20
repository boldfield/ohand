import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class FakeHistoryReader: ReminderHistoryReading, ReminderLinkChecking, @unchecked Sendable {
    private let lock = NSLock()
    private var historyResult: Result<ReminderHistoryAnswer, Error>
    private var readResult: Result<ReminderReadAnswer, Error>
    private var reads = 0

    init(history: Result<ReminderHistoryAnswer, Error>, read: Result<ReminderReadAnswer, Error> = .failure(ReminderControlError.itemNotFound)) {
        historyResult = history
        readResult = read
    }

    var readCount: Int { lock.lock(); defer { lock.unlock() }; return reads }
    func setHistory(_ result: Result<ReminderHistoryAnswer, Error>) { lock.lock(); historyResult = result; lock.unlock() }

    func history(itemID: String) async throws -> ReminderHistoryAnswer {
        lock.lock(); defer { lock.unlock() }
        return try historyResult.get()
    }

    func reminder(itemID: String) async throws -> ReminderReadAnswer {
        lock.lock(); defer { lock.unlock() }
        reads += 1
        return try readResult.get()
    }
}

private final class FakeNavigator: ReminderLinkNavigating {
    private(set) var opened: [String] = []
    func openItem(itemID: String) { opened.append(itemID) }
}

final class ReminderHistoryTests: XCTestCase {
    private static let itemID = "item-1"

    private func answer(entries: String, summary: String? = nil, truncated: Bool = false, itemID: String = "item-1")
        throws -> ReminderHistoryAnswer
    {
        let summaryJSON = summary ?? "null"
        let json = """
            {"item_id":"\(itemID)","reminder_id":"reminder-1","summary":\(summaryJSON),"entries":\(entries),
             "total_entries":2,"truncated":\(truncated)}
            """
        return try JSONDecoder().decode(ReminderHistoryAnswer.self, from: Data(json.utf8))
    }

    private func summaryJSON(delivery: String, ack: String, elapsedNoEvidence: Bool) -> String {
        """
        {"request_state":"resolved","schedule_state":"scheduled","due":"elapsed","delivery_state":"\(delivery)",
         "acknowledgment_state":"\(ack)","elapsed_without_delivery_evidence":\(elapsedNoEvidence)}
        """
    }

    private let sampleEntries = """
        [{"occurred_at":"2026-10-15T09:31:00Z","kind":"acknowledged","generation":1},
         {"occurred_at":"2026-10-15T09:30:00Z","kind":"time_requested","generation":1,
          "due_at":"2026-10-16T09:30:00Z","installation":"attempted_unconfirmed"}]
        """

    func testRowsShowOnlyRecordedEventsAndNeverClockWords() async throws {
        let reader = FakeHistoryReader(
            history: .success(try answer(entries: sampleEntries, summary: summaryJSON(delivery: "unknown", ack: "not_acknowledged", elapsedNoEvidence: true))))
        let model = ReminderHistoryModel(itemID: Self.itemID, reader: reader)
        await model.load()
        XCTAssertEqual(model.rows.map(\.title), ["Acknowledged", "Time saved"])
        let everything = (model.rows.flatMap { [$0.title, $0.detail ?? ""] } + model.summaryLines).joined(separator: " ").lowercased()
        for banned in ["fired", "missed", "seen"] { XCTAssertFalse(everything.contains(banned), banned) }
        XCTAssertTrue(model.summaryLines.contains("The time has passed. Nothing recorded whether it was shown."))
        XCTAssertTrue(model.rows[1].detail?.contains("not confirmed") == true, "unconfirmed install stays unconfirmed")
    }

    func testAcknowledgeIsNotCompleteAndUnknownDeliveryStaysUnknown() async throws {
        let reader = FakeHistoryReader(
            history: .success(try answer(entries: sampleEntries, summary: summaryJSON(delivery: "unknown", ack: "acknowledged", elapsedNoEvidence: false))))
        let model = ReminderHistoryModel(itemID: Self.itemID, reader: reader)
        await model.load()
        XCTAssertTrue(model.rows[0].detail?.contains("not marked complete") == true)
        XCTAssertTrue(model.summaryLines.contains("No delivery has been recorded."))
        XCTAssertFalse(model.summaryLines.joined().contains("delivered"))
    }

    func testLockClearsRowsAndReentryShowsSameBoundedListWithoutDigest() async throws {
        let reader = FakeHistoryReader(history: .success(try answer(entries: sampleEntries, truncated: true)))
        let model = ReminderHistoryModel(itemID: Self.itemID, reader: reader)
        await model.load()
        let before = model.rows
        await model.lock()
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.unavailable, .locked)
        await model.load()
        XCTAssertEqual(model.rows, before, "re-entry shows the stored list, not a replay")
        XCTAssertEqual(model.rows.count, 2)
        XCTAssertTrue(model.isTruncated)
        let text = (model.rows.flatMap { [$0.title, $0.detail ?? ""] } + model.summaryLines).joined(separator: " ").lowercased()
        XCTAssertFalse(text.contains("while you were away"))
        XCTAssertFalse(text.contains("digest"))
    }

    func testFailedAuthenticationClearsAndAnAnswerForAnotherItemIsDropped() async throws {
        let reader = FakeHistoryReader(history: .success(try answer(entries: sampleEntries)))
        let model = ReminderHistoryModel(itemID: Self.itemID, reader: reader)
        await model.load()
        reader.setHistory(.failure(ReminderControlError.notAuthenticated))
        await model.load()
        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertEqual(model.unavailable, .locked)

        let other = FakeHistoryReader(history: .success(try answer(entries: sampleEntries, itemID: "other")))
        let guarded = ReminderHistoryModel(itemID: Self.itemID, reader: other)
        await guarded.load()
        XCTAssertTrue(guarded.rows.isEmpty, "an answer for a different item is never shown")
    }

    // MARK: links

    private func readAnswer(lifecycle: String = "active", itemID: String = "item-1") -> ReminderReadAnswer {
        ReminderReadAnswer(itemID: itemID, itemRevision: 1, itemType: "action", lifecycleState: lifecycle, reminder: nil)
    }

    @MainActor func testLinkToReadableItemOpensIt() async {
        let reader = FakeHistoryReader(history: .failure(ReminderControlError.unreadable), read: .success(readAnswer()))
        let navigator = FakeNavigator()
        let outcome = await ReminderLinkRouter(checker: reader, navigator: navigator).route(targetID: "item-1")
        XCTAssertEqual(outcome, .opened(itemID: "item-1"))
        XCTAssertEqual(navigator.opened, ["item-1"])
    }

    @MainActor func testStaleDeletedAndMismatchedLinksOpenNothing() async {
        for result: Result<ReminderReadAnswer, Error> in [
            .failure(ReminderControlError.itemNotFound), .failure(ReminderControlError.itemDeleted),
            .failure(ReminderControlError.unreadable), .success(readAnswer(lifecycle: "deleted")),
            .success(readAnswer(itemID: "other")),
        ] {
            let navigator = FakeNavigator()
            let reader = FakeHistoryReader(history: .failure(ReminderControlError.unreadable), read: result)
            let outcome = await ReminderLinkRouter(checker: reader, navigator: navigator).route(targetID: "item-1")
            XCTAssertEqual(outcome, .unavailable)
            XCTAssertTrue(navigator.opened.isEmpty)
        }
    }

    @MainActor func testMalformedLinkIsRejectedBeforeAnyRead() async {
        let reader = FakeHistoryReader(history: .failure(ReminderControlError.unreadable), read: .success(readAnswer()))
        let navigator = FakeNavigator()
        let router = ReminderLinkRouter(checker: reader, navigator: navigator)
        for bad in [nil, "", "has space", "a/b", String(repeating: "x", count: 65)] {
            let outcome = await router.route(targetID: bad)
            XCTAssertEqual(outcome, .invalid)
        }
        XCTAssertEqual(reader.readCount, 0)
        XCTAssertTrue(navigator.opened.isEmpty)
    }

    @MainActor func testLockedSessionHandsOffToAuthenticatingNavigationOnly() async {
        let reader = FakeHistoryReader(history: .failure(ReminderControlError.unreadable), read: .failure(ReminderControlError.notAuthenticated))
        let navigator = FakeNavigator()
        let outcome = await ReminderLinkRouter(checker: reader, navigator: navigator).route(targetID: "item-1")
        XCTAssertEqual(outcome, .opened(itemID: "item-1"))
        XCTAssertEqual(navigator.opened, ["item-1"], "navigation authenticates before showing anything")
        XCTAssertNil(outcome.notice)
    }
}
