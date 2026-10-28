import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class QuietPermissions: PermissionsReading, @unchecked Sendable {
    func refresh() async -> [PermissionEntry] {
        [PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)]
    }
}

private final class CountingDrain: ProcessingRetrying {
    private(set) var count = 0
    func retryNow() { count += 1 }
}

private final class FixedHealthReader: ProcessingHealthReading, @unchecked Sendable {
    func read() async -> ProcessingHealthOutcome {
        let fault = "{\"kind\":\"permanent_failure\",\"reason\":\"rejected\",\"recovery\":\"terminal\","
            + "\"job_count\":1,\"oldest_age_seconds\":5,\"next_attempt_at\":null}"
        let json = """
            {"operation_id":1,"generated_at":"2026-10-22T00:00:00Z","summary":"needs_attention","stalled":false,
            "stall_reasons":[],"pending_jobs":0,"running_jobs":0,"overdue_jobs":0,
            "oldest_pending_age_seconds":null,"oldest_overdue_seconds":null,"last_interpretation_success_at":null,
            "errors":[\(fault)]}
            """
        return .snapshot(try! JSONDecoder().decode(ProcessingHealth.self, from: Data(json.utf8)))
    }
}

private final class FakeFailedWork: FailedInterpretationsControlling, @unchecked Sendable {
    var listing: Result<[JobConfigurationFailedInterpretation], Error>
    var rerunResult: Result<JobConfigurationRerunResult, Error> = .success(
        JobConfigurationRerunResult(moved: 0, skipped: 0, skippedItems: []))
    /// Pages after the first, read with cursors "1", "2", and so on.
    var laterPages: [[JobConfigurationFailedInterpretation]] = []
    private(set) var rerunRequests: [[String]] = []

    init(_ listing: Result<[JobConfigurationFailedInterpretation], Error>) { self.listing = listing }

    func failedInterpretations(limit: Int?, cursor: String?) async throws -> JobConfigurationFailedInterpretations {
        let index = cursor.flatMap { Int($0) } ?? 0
        let page = index == 0 ? try listing.get() : laterPages[index - 1]
        return JobConfigurationFailedInterpretations(
            failed: page, nextCursor: index < laterPages.count ? String(index + 1) : nil)
    }

    func rerunFailedInterpretations(itemIDs: [String]) async throws -> JobConfigurationRerunResult {
        rerunRequests.append(itemIDs)
        let result = try rerunResult.get()
        // The new jobs replace the failed ones, so the next read no longer lists them.
        if result.moved > 0 { listing = .success([]) }
        return result
    }
}

private final class FakeNoteText: FailedNoteTextReading, @unchecked Sendable {
    func firstLine(itemID: String) async -> String? { "First line of \(itemID)" }
}

private func failedNote(_ number: Int, reason: String = "rejected") -> JobConfigurationFailedInterpretation {
    JobConfigurationFailedInterpretation(
        itemID: "item-\(number)", capturedAt: "2026-10-22T0\(number % 10):15:00Z", failureReason: reason, pinned: true,
        profileVersion: "profile-1", recovery: .runAgain)
}

/// The Status screen's failed-notes section, hosted over fakes, in both appearances.
@MainActor
final class StatusFailedNotesTests: XCTestCase {
    private var window: UIWindow?
    private var rendered: [String: String] = [:]
    private var actions: [String: () -> Void] = [:]
    /// Counts hosted screens, so a report from a screen that has been replaced is ignored.
    private var generation = 0

    override func setUpWithError() throws {
        try super.setUpWithError()
        rendered = [:]
        actions = [:]
    }

    override func tearDownWithError() throws {
        window?.isHidden = true
        window = nil
        try super.tearDownWithError()
    }

    private func makeModel(
        _ work: FailedInterpretationsControlling, drain: ProcessingRetrying = CountingDrain(),
        unlock: (() -> Void)? = nil
    ) -> StatusModel {
        StatusModel(
            health: FixedHealthReader(), permissions: QuietPermissions(), retrier: drain, failedWork: work,
            noteText: FakeNoteText(), requestUnlock: unlock, openSystemSettings: {})
    }

    private func host(_ model: StatusModel, style: UIUserInterfaceStyle) {
        window?.rootViewController = nil
        window?.isHidden = true
        window = nil
        rendered = [:]
        actions = [:]
        generation += 1
        let current = generation
        let screen = StatusView(
            model: model, makeProviderSettings: { AnyView(Text("Processing")) },
            renderReporter: { [unowned self] in if current == generation { rendered = $0 } },
            actionReporter: { [unowned self] in if current == generation { actions = $0 } })
        let shown = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        shown.overrideUserInterfaceStyle = style
        shown.rootViewController = UIHostingController(rootView: NavigationStack { screen })
        shown.makeKeyAndVisible()
        window = shown
    }

    private func wait(_ condition: String, file: StaticString = #filePath, line: UInt = #line, until holds: () -> Bool) {
        waitUntilPumping(condition, timeout: 15, file: file, line: line, holds)
    }

    private let appearances: [UIUserInterfaceStyle] = [.light, .dark]

    func testLockedKeepsTheCountOnlyWordingAndOffersUnlock() {
        for style in appearances {
            var unlockAsked = 0
            let model = makeModel(FakeFailedWork(.failure(JobConfigurationError.notAuthenticated)), unlock: { unlockAsked += 1 })
            host(model, style: style)
            wait("the unlock offer is shown, \(style.rawValue)") { self.rendered[StatusView.unlockIdentifier] != nil }

            XCTAssertEqual(rendered[StatusView.unlockIdentifier], StatusText.unlockLabel)
            XCTAssertTrue(rendered[StatusView.rowIdentifier(.processing)]?.contains(StatusText.processingAttentionHeadline) == true)
            XCTAssertTrue(rendered.keys.allSatisfy { !$0.hasPrefix("status.failed-note.") }, "no note is named while locked")
            XCTAssertNil(rendered[StatusView.retryNotesIdentifier], "nothing to run again while locked")
            XCTAssertEqual(model.failedNotesAccess, .locked)
            XCTAssertEqual(unlockAsked, 0)
        }
    }

    func testOneFailedNoteShowsItsFirstLineTimeAndReason() {
        for style in appearances {
            let note = failedNote(1)
            let model = makeModel(FakeFailedWork(.success([note])))
            host(model, style: style)
            let identifier = StatusView.noteIdentifier("item-1")
            wait("the note row shows its first line, \(style.rawValue)") {
                self.rendered[identifier]?.contains("First line of item-1") == true
            }

            let row = rendered[identifier] ?? ""
            XCTAssertTrue(row.contains(StatusText.capturedLabel(FailedNote(note).capturedAt)), row)
            XCTAssertTrue(row.contains("The request was not accepted"), row)
            XCTAssertNil(rendered[StatusView.showAllIdentifier], "one note needs no Show all")
            XCTAssertEqual(rendered[StatusView.retryNotesIdentifier], StatusText.retryLabel)
            XCTAssertNil(rendered[StatusView.unlockIdentifier])
        }
    }

    func testEveryPageOfFailedNotesIsReadAndRunAgain() async {
        let work = FakeFailedWork(.success((1...2).map { failedNote($0) }))
        work.laterPages = [[failedNote(3)], [failedNote(4)]]
        work.rerunResult = .success(JobConfigurationRerunResult(moved: 4, skipped: 0, skippedItems: []))
        let model = makeModel(work)
        await model.refresh()
        XCTAssertEqual(model.failedNotes.map(\.itemID), ["item-1", "item-2", "item-3", "item-4"])

        await model.retryFailedNotes()

        XCTAssertEqual(work.rerunRequests, [["item-1", "item-2", "item-3", "item-4"]])
    }

    func testMoreThanFiveNotesListFiveAndLeadToShowAll() {
        for style in appearances {
            let model = makeModel(FakeFailedWork(.success((1...7).map { failedNote($0) })))
            host(model, style: style)
            wait("five rows and Show all are shown, \(style.rawValue)") {
                self.rendered[StatusView.showAllIdentifier] != nil && self.rendered[StatusView.noteIdentifier("item-5")] != nil
            }

            for number in 1...5 { XCTAssertNotNil(rendered[StatusView.noteIdentifier("item-\(number)")], "note \(number) is listed") }
            XCTAssertNil(rendered[StatusView.noteIdentifier("item-6")])
            XCTAssertNil(rendered[StatusView.noteIdentifier("item-7")])
            XCTAssertEqual(model.failedNotes.count, 7)
        }
    }

    func testTryAgainThatQueuedNotesReportsTheCountAndAsksTheQueueToDrain() {
        for style in appearances {
            let work = FakeFailedWork(.success([failedNote(1), failedNote(2)]))
            work.rerunResult = .success(JobConfigurationRerunResult(moved: 2, skipped: 0, skippedItems: []))
            let drain = CountingDrain()
            let model = makeModel(work, drain: drain)
            host(model, style: style)
            wait("the Try again action is reported, \(style.rawValue)") { self.actions[StatusView.retryNotesIdentifier] != nil }

            actions[StatusView.retryNotesIdentifier]?()
            wait("the summary is shown, \(style.rawValue)") { self.rendered[StatusView.rerunSummaryIdentifier] != nil }

            XCTAssertEqual(work.rerunRequests, [["item-1", "item-2"]])
            XCTAssertEqual(rendered[StatusView.rerunSummaryIdentifier], "2 notes queued again.")
            XCTAssertNil(rendered[StatusView.processingSettingsIdentifier], "nothing is waiting on a setting")
            XCTAssertEqual(drain.count, 1, "the new jobs are sent")
        }
    }

    func testTryAgainThatCouldNotQueueBecauseNoProviderIsAllowedLinksToProcessing() {
        for style in appearances {
            let work = FakeFailedWork(.success([failedNote(1, reason: "unsupported")]))
            work.rerunResult = .success(
                JobConfigurationRerunResult(
                    moved: 0, skipped: 1, skippedItems: [JobConfigurationSkippedItem(itemID: "item-1", reason: .noProviderAllowed)]))
            let drain = CountingDrain()
            let model = makeModel(work, drain: drain)
            host(model, style: style)
            wait("the row shows its reason, \(style.rawValue)") {
                self.rendered[StatusView.noteIdentifier("item-1")]?.contains("No provider was allowed when it was saved") == true
                    && self.actions[StatusView.retryNotesIdentifier] != nil
            }

            actions[StatusView.retryNotesIdentifier]?()
            wait("the summary and the Processing link are shown, \(style.rawValue)") {
                self.rendered[StatusView.processingSettingsIdentifier] != nil
            }

            XCTAssertEqual(rendered[StatusView.rerunSummaryIdentifier], "1 note can't run until a setting changes.")
            XCTAssertEqual(rendered[StatusView.processingSettingsIdentifier], StatusText.processingSettingsLabel)
            XCTAssertEqual(drain.count, 0, "nothing was queued, so there is nothing to send")
            XCTAssertNotNil(rendered[StatusView.noteIdentifier("item-1")], "the note is still listed")
        }
    }

    func testEveryMachineLabelHasItsOwnPlainReasonAndAnUnknownOneIsNotEchoed() {
        let labels = [
            "timeout", "cancelled", "unavailable", "rate_limited", "unauthorized", "unsupported", "unsupported_job_version",
            "invalid_output", "output_too_large", "input_too_large", "capability_unavailable", "capability_did_not_settle",
            "profile_mismatch", "rejected", "source_unavailable", "permanent", "native_failure", "offline_deferred",
            "retries_exhausted", "interrupted", "internal_error", "profile_revoked", "profile_missing",
        ]
        let reasons = labels.map(StatusText.failureReason)
        XCTAssertEqual(Set(reasons).count, labels.count, "each label reads differently")
        XCTAssertEqual(StatusText.failureReason("rejected"), "The request was not accepted")
        XCTAssertEqual(StatusText.failureReason("unsupported"), "No provider was allowed when it was saved")
        let unknown = StatusText.failureReason("something_new")
        XCTAssertFalse(unknown.contains("something_new"))
        XCTAssertEqual(unknown, StatusText.failureReason("other"))
    }
}
