import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedHistoryReader: ReminderHistoryReading, @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<ReminderHistoryAnswer, Error>
    init(_ result: Result<ReminderHistoryAnswer, Error>) { self.result = result }
    func set(_ next: Result<ReminderHistoryAnswer, Error>) { lock.lock(); result = next; lock.unlock() }
    func history(itemID: String) async throws -> ReminderHistoryAnswer {
        lock.lock(); defer { lock.unlock() }
        return try result.get()
    }
}

/// Hosts the real `ReminderHistoryList` over the real `ReminderHistoryModel` and reads the lines it renders.
@MainActor
final class ReminderHistoryViewTests: XCTestCase {
    private var window: UIWindow?
    private var rendered: [String: String] = [:]

    override func tearDown() async throws {
        window?.isHidden = true
        window = nil
        try await super.tearDown()
    }

    private func answer(entries: String, summary: String = "null", truncated: Bool = false) throws -> ReminderHistoryAnswer {
        let json = """
            {"item_id":"item-1","reminder_id":"reminder-1","summary":\(summary),"entries":\(entries),
             "total_entries":2,"truncated":\(truncated)}
            """
        return try JSONDecoder().decode(ReminderHistoryAnswer.self, from: Data(json.utf8))
    }

    private func host(_ model: ReminderHistoryModel, expecting: Int) async {
        let hosting = UIHostingController(
            rootView: ScrollView {
                ReminderHistoryList(model: model).onPreferenceChange(ReminderEditSection.RenderedKey.self) { [unowned self] in
                    rendered = $0
                }
            })
        let host = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        host.rootViewController = hosting
        host.makeKeyAndVisible()
        window = host
        for _ in 0..<60 {
            hosting.view.setNeedsLayout()
            hosting.view.layoutIfNeeded()
            if lines.count >= expecting { break }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        try? await Task.sleep(nanoseconds: 200_000_000)
    }

    private var lines: [String] {
        rendered.filter { $0.key.hasPrefix(ReminderHistoryList.linePrefix) }.map(\.value)
    }

    private let entries = """
        [{"occurred_at":"2026-10-15T09:31:00Z","kind":"acknowledged","generation":1},
         {"occurred_at":"2026-10-15T09:30:00Z","kind":"time_requested","generation":1,
          "due_at":"2026-10-16T09:30:00Z","installation":"attempted_unconfirmed"}]
        """
    private let summary = """
        {"request_state":"resolved","schedule_state":"scheduled","due":"elapsed","delivery_state":"unknown",
         "acknowledgment_state":"acknowledged","elapsed_without_delivery_evidence":true}
        """

    func testPopulatedHistoryRendersUnknownDeliveryAcknowledgedAndTruncation() async throws {
        let reader = ScriptedHistoryReader(.success(try answer(entries: entries, summary: summary, truncated: true)))
        let model = ReminderHistoryModel(itemID: "item-1", reader: reader)
        await model.load()
        await host(model, expecting: model.rows.count + model.summaryLines.count + 1)
        let shown = lines.joined(separator: "\n")
        XCTAssertTrue(shown.contains("Acknowledged"), shown)
        XCTAssertTrue(shown.contains("Time saved"), shown)
        XCTAssertTrue(shown.contains("The time has passed. Nothing recorded whether it was shown."), shown)
        XCTAssertTrue(shown.contains(ReminderHistoryText.truncatedText), shown)
        for banned in ["fired", "missed", "seen"] { XCTAssertFalse(shown.lowercased().contains(banned), banned) }
    }

    func testTheHeadingNamesTheReminderHistoryAndIsNotTheItemHistoryHeading() async throws {
        XCTAssertEqual(ReminderHistoryList.headingText, "Reminder history")
        XCTAssertNotEqual(ReminderHistoryList.headingText, SourceDetailView.itemHistoryHeading)
        XCTAssertEqual(SourceDetailView.itemHistoryHeading, "Item history")
    }

    func testEmptyLockedAndUnreadableStatesRenderTheirOwnWords() async throws {
        let empty = ReminderHistoryModel(itemID: "item-1", reader: ScriptedHistoryReader(.success(try answer(entries: "[]"))))
        await empty.load()
        await host(empty, expecting: 1)
        XCTAssertEqual(lines, [ReminderHistoryText.emptyText])

        let locked = ReminderHistoryModel(
            itemID: "item-1", reader: ScriptedHistoryReader(.failure(ReminderControlError.notAuthenticated)))
        await locked.load()
        await host(locked, expecting: 1)
        XCTAssertEqual(lines, [ReminderHistoryText.lockedText])

        let unreadable = ReminderHistoryModel(
            itemID: "item-1", reader: ScriptedHistoryReader(.failure(ReminderControlError.unreadable)))
        await unreadable.load()
        await host(unreadable, expecting: 1)
        XCTAssertEqual(lines, [ReminderHistoryText.unreadableText])
    }
}
