import Foundation
import OhAndCoreBridge
import OhAndServices

/// What the detail screen says about whether the note's text has been sorted.
enum SourceDetailSorting: Equatable {
    /// Sorted, waiting on a setting or not applicable: the screen says nothing.
    case none
    case sorting
    /// Sorting failed. `reason` is fixed copy for the machine label the core recorded.
    case failed(reason: String)
}

/// Reads one note's sorting and runs a failed one again. `ItemSortingSource` is the production implementation. Both go
/// through private reads, so a locked session answers `.none` and a refusal.
protocol SourceDetailSortingReading: AnyObject, Sendable {
    func sorting(itemID: String) async -> SourceDetailSorting
    func tryAgain(itemID: String) async -> FailedNotesRerunSummary
}

/// The note's sorting from F43's failed-work read and the core's item status.
final class ItemSortingSource: SourceDetailSortingReading, @unchecked Sendable {
    typealias ProcessingRead = (String) async -> (state: String, jobStatus: String?, rerunState: String?)?

    private let jobs: FailedInterpretationsControlling
    private let processing: ProcessingRead
    private let afterQueued: () -> Void
    init(
        jobs: FailedInterpretationsControlling, processing: @escaping ProcessingRead,
        afterQueued: @escaping () -> Void
    ) {
        self.jobs = jobs
        self.processing = processing
        self.afterQueued = afterQueued
    }

    func sorting(itemID: String) async -> SourceDetailSorting {
        var cursor: String?
        var seenCursors: Set<String> = []
        while true {
            guard let page = try? await jobs.failedInterpretations(limit: 100, cursor: cursor) else { break }
            if let failed = page.failed.first(where: { $0.itemID == itemID }) {
                return .failed(reason: StatusText.failureReason(failed.failureReason))
            }
            // Follow the cursors to the end; a cursor seen twice would loop, so it ends the search.
            guard let next = page.nextCursor, seenCursors.insert(next).inserted else { break }
            cursor = next
        }
        guard let status = await processing(itemID) else { return .none }
        // A "Try again" leaves the note `uninterpreted` or `abstained` while its replacement job waits or runs; the core
        // reports that as `rerun_state`, so the answer survives reopening the note and restarting the app. Every value,
        // including a replacement waiting on a setting, means the note is being sorted again.
        if status.rerunState != nil { return .sorting }
        let isWorking = status.state == "processing" || status.state == "unprocessed"
        return isWorking && status.jobStatus != "awaiting_configuration" ? .sorting : .none
    }

    func tryAgain(itemID: String) async -> FailedNotesRerunSummary {
        guard let result = try? await jobs.rerunFailedInterpretations(itemIDs: [itemID]) else { return .failed }
        if result.moved > 0 { afterQueued() }
        return FailedNotesRerunSummary(result)
    }
}
