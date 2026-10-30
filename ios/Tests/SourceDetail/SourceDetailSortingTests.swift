import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class OneItemReader: SourceDetailReading, @unchecked Sendable {
    func detail(itemID: String) async throws -> CoreItemDetail {
        CoreItemDetail(
            itemID: itemID, captureID: "capture-1", revision: 0, itemType: nil, scope: "personal", sessionTopic: nil,
            lifecycleState: "active", capturedAt: "2026-10-22T09:30:00Z", text: "buy oat milk", textBasis: .original,
            originalText: "buy oat milk", correctedText: nil, audioReference: nil, transcript: nil, history: [],
            historyTruncated: false)
    }
}

private final class NoAudio: SourceAudioOpening, @unchecked Sendable {
    func openAudio(itemID: String) async throws -> SourceAudioSession { throw PlaybackError.noAudio }
    func audioAvailability(itemID: String) async -> PlaybackAvailability { .noAudio }
}

private final class ScriptedSorting: SourceDetailSortingReading, @unchecked Sendable {
    var states: [SourceDetailSorting]
    var summary: FailedNotesRerunSummary = .done(queued: 1, needsSetting: 0)
    private(set) var tryAgainCalls = 0

    init(_ states: [SourceDetailSorting]) { self.states = states }

    func sorting(itemID: String) async -> SourceDetailSorting {
        states.count > 1 ? states.removeFirst() : (states.first ?? .none)
    }

    func tryAgain(itemID: String) async -> FailedNotesRerunSummary {
        tryAgainCalls += 1
        return summary
    }
}

private final class ScriptedJobs: FailedInterpretationsControlling, @unchecked Sendable {
    var pages: [JobConfigurationFailedInterpretations]
    var rerunResult = JobConfigurationRerunResult(moved: 1, skipped: 0, skippedItems: [])
    private(set) var rerunRequests: [[String]] = []

    init(_ pages: [JobConfigurationFailedInterpretations]) { self.pages = pages }

    func failedInterpretations(limit: Int?, cursor: String?) async throws -> JobConfigurationFailedInterpretations {
        pages[cursor.flatMap { Int($0) } ?? 0]
    }

    func rerunFailedInterpretations(itemIDs: [String]) async throws -> JobConfigurationRerunResult {
        rerunRequests.append(itemIDs)
        return rerunResult
    }
}

/// The core's durable state for the failed-work read, the rerun and the item status, shared by every model and source a
/// test builds, as the store is. A rerun does what F43 and F56 do: the replaced failure leaves the failed list, the note
/// stays `uninterpreted`, and its status reports `rerun_state = queued` until the replacement ends.
private final class FakeCore: FailedInterpretationsControlling, @unchecked Sendable {
    private(set) var failed: [JobConfigurationFailedInterpretation]
    private(set) var rerunStates: [String: String] = [:]
    private(set) var rerunRequests: [[String]] = []

    init(failed: [JobConfigurationFailedInterpretation]) { self.failed = failed }

    func failedInterpretations(limit: Int?, cursor: String?) async throws -> JobConfigurationFailedInterpretations {
        JobConfigurationFailedInterpretations(failed: failed, nextCursor: nil)
    }

    func rerunFailedInterpretations(itemIDs: [String]) async throws -> JobConfigurationRerunResult {
        rerunRequests.append(itemIDs)
        let moving = failed.filter { itemIDs.contains($0.itemID) }
        failed.removeAll { itemIDs.contains($0.itemID) }
        for record in moving { rerunStates[record.itemID] = "queued" }
        return JobConfigurationRerunResult(moved: moving.count, skipped: 0, skippedItems: [])
    }

    /// The replacement job was claimed, or ended without failing.
    func setRerunState(_ state: String?, itemID: String) { rerunStates[itemID] = state }

    func status(itemID: String) -> (state: String, jobStatus: String?, rerunState: String?)? {
        (state: "uninterpreted", jobStatus: nil, rerunState: rerunStates[itemID])
    }
}

private final class QuietPermissions: PermissionsReading, @unchecked Sendable {
    func refresh() async -> [PermissionEntry] {
        [PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)]
    }
}

private final class NoDrain: ProcessingRetrying {
    private(set) var count = 0
    func retryNow() { count += 1 }
}

private final class IdleHealth: ProcessingHealthReading, @unchecked Sendable {
    func read() async -> ProcessingHealthOutcome {
        let json = """
            {"operation_id":1,"generated_at":"2026-10-22T00:00:00Z","summary":"idle","stalled":false,
            "stall_reasons":[],"pending_jobs":0,"running_jobs":0,"overdue_jobs":0,
            "oldest_pending_age_seconds":null,"oldest_overdue_seconds":null,"last_interpretation_success_at":null,
            "errors":[]}
            """
        return .snapshot(try! JSONDecoder().decode(ProcessingHealth.self, from: Data(json.utf8)))
    }
}

private func failedRecord(_ itemID: String, reason: String) -> JobConfigurationFailedInterpretation {
    JobConfigurationFailedInterpretation(
        itemID: itemID, capturedAt: "2026-10-22T09:30:00Z", failureReason: reason, pinned: false, profileVersion: nil,
        recovery: .runAgain)
}

@MainActor
final class SourceDetailSortingTests: XCTestCase {
    private static let itemID = "item-sorting-1"
    private var window: UIWindow?
    private var shown: Set<String> = []
    private var generation = 0

    override func tearDownWithError() throws {
        window?.isHidden = true
        window = nil
        try super.tearDownWithError()
    }

    private func makeModel(_ sorting: SourceDetailSortingReading?) -> SourceDetailModel {
        let model = SourceDetailModel(itemID: Self.itemID, reader: OneItemReader(), opener: NoAudio())
        model.sortingSource = sorting
        return model
    }

    private func host(_ model: SourceDetailModel, style: UIUserInterfaceStyle) {
        window?.rootViewController = nil
        window?.isHidden = true
        window = nil
        shown = []
        generation += 1
        let current = generation
        let screen = SourceDetailView(
            model: model, renderReporter: { [unowned self] identifier, isShown in
                guard current == generation else { return }
                if isShown { shown.insert(identifier) } else { shown.remove(identifier) }
            })
        let hosted = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        hosted.overrideUserInterfaceStyle = style
        hosted.rootViewController = UIHostingController(rootView: NavigationStack { screen })
        hosted.makeKeyAndVisible()
        window = hosted
    }

    private func wait(_ condition: String, file: StaticString = #filePath, line: UInt = #line, until holds: () -> Bool) {
        waitUntilPumping(condition, timeout: 15, file: file, line: line, holds)
    }

    func testAFailedNoteShowsOneReasonLineAndTryAgainInBothAppearances() {
        for style in [UIUserInterfaceStyle.light, .dark] {
            let sorting = ScriptedSorting([.failed(reason: StatusText.failureReason("rejected")), .sorting])
            let model = makeModel(sorting)
            host(model, style: style)
            wait("the failed line and Try again are shown, \(style.rawValue)") {
                self.shown.contains(SourceDetailView.sortingFailedIdentifier) && self.shown.contains(SourceDetailView.tryAgainIdentifier)
            }
            XCTAssertEqual(model.sorting, .failed(reason: "The request was not accepted"))
            XCTAssertFalse(shown.contains(SourceDetailView.sortingIdentifier))
        }
    }

    func testTryAgainRunsThisNoteAgainAndTheScreenThenSaysSorting() async {
        let sorting = ScriptedSorting([.failed(reason: "The request was not accepted"), .sorting])
        let model = makeModel(sorting)
        await model.load()
        XCTAssertEqual(model.sorting, .failed(reason: "The request was not accepted"))

        await model.tryAgain()

        XCTAssertEqual(sorting.tryAgainCalls, 1)
        XCTAssertEqual(model.tryAgainSummary, .done(queued: 1, needsSetting: 0))
        XCTAssertEqual(model.sorting, .sorting)
        await model.tryAgain()
        XCTAssertEqual(sorting.tryAgainCalls, 1, "a note that is not failed offers no Try again")
    }

    /// A fresh source over the shared core, as opening the note's detail builds one.
    private func freshSource(_ core: FakeCore) -> ItemSortingSource {
        ItemSortingSource(jobs: core, processing: { core.status(itemID: $0) }, afterQueued: {})
    }

    /// Hosts `model` in both appearances and checks that "Sorting…" is rendered and "Try again" is not.
    private func assertRendersSorting(_ model: SourceDetailModel, _ step: String, file: StaticString = #filePath, line: UInt = #line) {
        for style in [UIUserInterfaceStyle.light, .dark] {
            host(model, style: style)
            wait("Sorting… is shown \(step), \(style.rawValue)", file: file, line: line) {
                self.shown.contains(SourceDetailView.sortingIdentifier)
            }
            XCTAssertFalse(shown.contains(SourceDetailView.tryAgainIdentifier), step, file: file, line: line)
            XCTAssertFalse(shown.contains(SourceDetailView.sortingFailedIdentifier), step, file: file, line: line)
        }
    }

    /// The production transition: a permanent failure leaves the note `uninterpreted`; the rerun queues a replacement
    /// without changing that, and the replaced failure leaves the failed-work list. The core then reports `rerun_state`.
    func testARerunPermanentFailureStaysVisibleAsSortingWhileItsReplacementJobRuns() async {
        let core = FakeCore(failed: [failedRecord(Self.itemID, reason: "rejected")])
        let model = makeModel(freshSource(core))
        await model.load()
        XCTAssertEqual(model.sorting, .failed(reason: "The request was not accepted"))

        await model.tryAgain()

        XCTAssertEqual(core.rerunRequests, [[Self.itemID]])
        XCTAssertEqual(model.sorting, .sorting)
        XCTAssertEqual(model.tryAgainSummary, .done(queued: 1, needsSetting: 0))
        assertRendersSorting(model, "after Try again on the detail screen")
        XCTAssertTrue(shown.contains(SourceDetailView.tryAgainSummaryIdentifier))

        // Reopening the note (or restarting the app) builds a fresh source and model; the state is durable.
        let reopened = makeModel(freshSource(core))
        await reopened.load()
        XCTAssertEqual(reopened.sorting, .sorting)
        assertRendersSorting(reopened, "on the reopened note")

        core.setRerunState(nil, itemID: Self.itemID)
        let finished = await freshSource(core).sorting(itemID: Self.itemID)
        XCTAssertEqual(finished, .none, "when the replacement ends without failing, nothing is said")
    }

    /// "Try again" on Status, then the note is opened: a fresh source and detail model say "Sorting…" from the core.
    func testATryAgainOnStatusShowsSortingOnAFreshlyOpenedDetail() async {
        let core = FakeCore(failed: [failedRecord(Self.itemID, reason: "rejected"), failedRecord("item-other", reason: "timeout")])
        let drain = NoDrain()
        let status = StatusModel(
            health: IdleHealth(), permissions: QuietPermissions(), retrier: drain, failedWork: core,
            openSystemSettings: {})
        await status.refresh()
        XCTAssertEqual(status.failedNotes.map(\.itemID), [Self.itemID, "item-other"])

        await status.retryFailedNotes()

        XCTAssertEqual(core.rerunRequests, [[Self.itemID, "item-other"]])
        XCTAssertEqual(status.rerunSummary, .done(queued: 2, needsSetting: 0))
        XCTAssertEqual(drain.count, 1)
        XCTAssertTrue(status.failedNotes.isEmpty, "the replaced failures leave the list")

        let opened = makeModel(freshSource(core))
        await opened.load()
        XCTAssertEqual(opened.sorting, .sorting)
        assertRendersSorting(opened, "on the note opened after Try again on Status")
    }

    /// The same, after the note is closed and opened again while the replacement is claimed and running.
    func testATryAgainOnStatusStillShowsSortingWhenTheNoteIsReopened() async {
        let core = FakeCore(failed: [failedRecord(Self.itemID, reason: "unavailable")])
        let status = StatusModel(
            health: IdleHealth(), permissions: QuietPermissions(), retrier: NoDrain(), failedWork: core,
            openSystemSettings: {})
        await status.refresh()
        await status.retryFailedNotes()

        let first = makeModel(freshSource(core))
        await first.load()
        assertRendersSorting(first, "on the first opening")
        window?.isHidden = true
        window = nil

        core.setRerunState("running", itemID: Self.itemID)
        let reopened = makeModel(freshSource(core))
        await reopened.load()
        XCTAssertEqual(reopened.sorting, .sorting)
        assertRendersSorting(reopened, "on the reopened note")
    }

    func testEveryRerunStateSaysSorting() async {
        let jobs = ScriptedJobs([JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)])
        func state(_ rerun: String) async -> SourceDetailSorting {
            await ItemSortingSource(
                jobs: jobs, processing: { _ in (state: "abstained", jobStatus: nil, rerunState: rerun) }, afterQueued: {}
            ).sorting(itemID: Self.itemID)
        }
        for rerun in ["queued", "running", "retrying_after_transient", "awaiting_configuration"] {
            let sorting = await state(rerun)
            XCTAssertEqual(sorting, .sorting, rerun)
        }
    }

    func testAFailedNoteBeyondTheFirstFivePagesIsStillFound() async {
        var pages = (0..<7).map {
            JobConfigurationFailedInterpretations(failed: [failedRecord("other-\($0)", reason: "timeout")], nextCursor: "\($0 + 1)")
        }
        pages.append(JobConfigurationFailedInterpretations(failed: [failedRecord(Self.itemID, reason: "rejected")], nextCursor: nil))
        let source = ItemSortingSource(jobs: ScriptedJobs(pages), processing: { _ in nil }, afterQueued: {})
        let state = await source.sorting(itemID: Self.itemID)
        XCTAssertEqual(state, .failed(reason: "The request was not accepted"))
    }

    func testARepeatedCursorEndsTheSearchInsteadOfLooping() async {
        let loop = JobConfigurationFailedInterpretations(failed: [failedRecord("other", reason: "timeout")], nextCursor: "0")
        let source = ItemSortingSource(jobs: ScriptedJobs([loop]), processing: { _ in nil }, afterQueued: {})
        let state = await source.sorting(itemID: Self.itemID)
        XCTAssertEqual(state, .none)
    }

    func testAnUninterpretedNoteThatWasNeverRunAgainShowsNothing() async {
        let jobs = ScriptedJobs([JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)])
        let source = ItemSortingSource(jobs: jobs, processing: { _ in (state: "uninterpreted", jobStatus: nil, rerunState: nil) }, afterQueued: {})
        let state = await source.sorting(itemID: Self.itemID)
        XCTAssertEqual(state, .none)
    }

    func testASortingNoteShowsSortingAndNothingElse() {
        for style in [UIUserInterfaceStyle.light, .dark] {
            host(makeModel(ScriptedSorting([.sorting])), style: style)
            wait("the sorting line is shown, \(style.rawValue)") { self.shown.contains(SourceDetailView.sortingIdentifier) }
            XCTAssertFalse(shown.contains(SourceDetailView.tryAgainIdentifier))
            XCTAssertFalse(shown.contains(SourceDetailView.sortingFailedIdentifier))
        }
    }

    func testANoteThatIsNeitherFailedNorSortingShowsNothing() {
        for style in [UIUserInterfaceStyle.light, .dark] {
            host(makeModel(ScriptedSorting([.none])), style: style)
            wait("the note is shown, \(style.rawValue)") { self.shown.contains(SourceDetailView.currentTextIdentifier) }
            XCTAssertFalse(shown.contains(SourceDetailView.sortingIdentifier))
            XCTAssertFalse(shown.contains(SourceDetailView.sortingFailedIdentifier))
            XCTAssertFalse(shown.contains(SourceDetailView.tryAgainIdentifier))
        }
    }

    func testTheSourceFindsAFailedNoteOnALaterPageAndRunsOnlyThatNoteAgain() async {
        let first = JobConfigurationFailedInterpretations(failed: [failedRecord("other", reason: "timeout")], nextCursor: "1")
        let second = JobConfigurationFailedInterpretations(
            failed: [failedRecord(Self.itemID, reason: "unsupported")], nextCursor: nil)
        let jobs = ScriptedJobs([first, second])
        var drains = 0
        let source = ItemSortingSource(jobs: jobs, processing: { _ in nil }, afterQueued: { drains += 1 })

        let state = await source.sorting(itemID: Self.itemID)
        XCTAssertEqual(state, .failed(reason: "No provider was allowed when it was saved"))
        let summary = await source.tryAgain(itemID: Self.itemID)
        XCTAssertEqual(summary, .done(queued: 1, needsSetting: 0))
        XCTAssertEqual(jobs.rerunRequests, [[Self.itemID]])
        XCTAssertEqual(drains, 1)
    }

    func testTheSourceSaysSortingOnlyForWorkInProgress() async {
        let jobs = ScriptedJobs([JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)])
        func state(_ processing: String, _ job: String?) async -> SourceDetailSorting {
            await ItemSortingSource(jobs: jobs, processing: { _ in (state: processing, jobStatus: job, rerunState: nil) }, afterQueued: {})
                .sorting(itemID: Self.itemID)
        }
        let inProgress = await state("processing", nil)
        let queued = await state("unprocessed", nil)
        let waitingForSetup = await state("unprocessed", "awaiting_configuration")
        let done = await state("processed", nil)
        XCTAssertEqual(inProgress, .sorting)
        XCTAssertEqual(queued, .sorting)
        XCTAssertEqual(waitingForSetup, .none)
        XCTAssertEqual(done, .none)
    }

    func testARunThatCannotQueueReportsTheSettingAndKeepsTheNoteFailed() async {
        let sorting = ScriptedSorting([.failed(reason: "No provider was allowed when it was saved")])
        sorting.summary = .done(queued: 0, needsSetting: 1)
        let model = makeModel(sorting)
        await model.load()

        await model.tryAgain()

        XCTAssertEqual(model.tryAgainSummary?.text, "1 note can't run until a setting changes.")
        XCTAssertEqual(model.sorting, .failed(reason: "No provider was allowed when it was saved"))
    }
}
