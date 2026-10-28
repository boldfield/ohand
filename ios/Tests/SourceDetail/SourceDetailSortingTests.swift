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

    /// The production transition: a permanent failure leaves the note `uninterpreted`; the rerun queues a replacement
    /// without changing that, and the replaced failure leaves the failed-work list.
    func testARerunPermanentFailureStaysVisibleAsSortingWhileItsReplacementJobRuns() async {
        let failed = JobConfigurationFailedInterpretations(
            failed: [failedRecord(Self.itemID, reason: "rejected")], nextCursor: nil)
        let jobs = ScriptedJobs([failed])
        let source = ItemSortingSource(jobs: jobs, processing: { _ in (state: "uninterpreted", jobStatus: nil) }, afterQueued: {})
        let model = makeModel(source)
        await model.load()
        XCTAssertEqual(model.sorting, .failed(reason: "The request was not accepted"))
        jobs.pages = [JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)]

        await model.tryAgain()

        XCTAssertEqual(model.sorting, .sorting)
        XCTAssertEqual(model.tryAgainSummary, .done(queued: 1, needsSetting: 0))
        for style in [UIUserInterfaceStyle.light, .dark] {
            host(model, style: style)
            wait("Sorting… and the queued line are shown, \(style.rawValue)") {
                self.shown.contains(SourceDetailView.sortingIdentifier) && self.shown.contains(SourceDetailView.tryAgainSummaryIdentifier)
            }
            XCTAssertFalse(shown.contains(SourceDetailView.tryAgainIdentifier))
        }
    }

    func testAnUninterpretedNoteThatWasNeverRunAgainShowsNothing() async {
        let jobs = ScriptedJobs([JobConfigurationFailedInterpretations(failed: [], nextCursor: nil)])
        let source = ItemSortingSource(jobs: jobs, processing: { _ in (state: "uninterpreted", jobStatus: nil) }, afterQueued: {})
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
            await ItemSortingSource(jobs: jobs, processing: { _ in (state: processing, jobStatus: job) }, afterQueued: {})
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
