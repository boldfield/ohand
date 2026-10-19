import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The item deletion operations this control uses. `ItemDeletionService` is the production implementation; the control
/// reuses its operations and adds none.
protocol ItemDeletionControlling: AnyObject {
    func delete(itemID: String, expectedRevision: Int) async throws -> ItemDeletionReceipt
    func progress(itemID: String) async throws -> ItemDeletionProgress
}

extension ItemDeletionService: ItemDeletionControlling {}

/// What the per-item delete control is showing.
enum ItemDeletionPhase: Equatable {
    case idle
    /// The user pressed delete and has not yet confirmed.
    case confirming
    case deleting
    /// The deletion is recorded and the item is unreadable, but a cleanup effect is still outstanding or failed.
    case pending(String)
    case complete
    case failed(String)
    case locked
}

/// The state of one item's delete control.
///
/// Rules this type keeps:
/// * Nothing is deleted without an explicit confirmation, and only against the revision this control last read.
/// * The deletion is `complete` only when the service reports every cleanup effect durably completed. Until then it
///   stays `pending` with the effects still outstanding. A retry repeats the same request (a replay) so cleanup runs
///   again; it is offered only while this control holds the revision that request was applied against. After a restart
///   it does not, and the control offers a progress check instead, because the lifecycle pass resumes the cleanup.
/// * The moment the deletion is recorded, `isRecorded` turns true, whatever cleanup still has to do, so the host can
///   read the item again and stop showing it.
/// * An error before the core records the deletion is shown as an error and never as pending or complete.
/// * Messages are fixed words and effect names; no identifier, path or secret is shown.
@MainActor
final class ItemDeletionModel: ObservableObject {
    @Published private(set) var phase: ItemDeletionPhase = .idle
    /// The revision of the live item as last read; deletion is applied against exactly this. `nil` when the item cannot
    /// be read or is already deleted, in which case no new deletion is offered.
    @Published private(set) var revision: Int?
    /// True once the core has recorded this item's deletion (before its cleanup has necessarily finished).
    @Published private(set) var isRecorded = false

    let itemID: String
    private let controls: ItemDeletionControlling
    private let reader: SourceDetailReading
    private let readScope: () -> CompositionReadScope
    /// The revision the recorded deletion was applied against, kept so a retry replays it.
    private var appliedRevision: Int?
    private var session = 0

    /// How often the control asks the core whether the deletion is recorded while the delete call is still running.
    static let recordedPollNanoseconds: UInt64 = 100_000_000

    nonisolated init(
        itemID: String, controls: ItemDeletionControlling, reader: SourceDetailReading,
        readScope: @escaping () -> CompositionReadScope
    ) {
        self.itemID = itemID
        self.controls = controls
        self.reader = reader
        self.readScope = readScope
    }

    /// True while a retry can run: cleanup is outstanding rather than terminally failed, and the revision it was
    /// applied against is known.
    var canRetry: Bool {
        if case .pending = phase { return canRetryPending && appliedRevision != nil }
        return false
    }

    /// True while a recorded deletion has cleanup outstanding that this control cannot retry itself (it was recorded
    /// before this screen opened): the user can check how far it has got.
    var canCheckProgress: Bool {
        if case .pending = phase { return canRetryPending && appliedRevision == nil }
        return false
    }

    private var canRetryPending = false

    var isBusy: Bool { phase == .deleting }

    func requestDeletion() {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .idle || isFailed else { return }
        phase = .confirming
    }

    func cancelConfirmation() {
        if phase == .confirming { phase = .idle }
    }

    private var isFailed: Bool {
        if case .failed = phase { return true }
        return false
    }

    /// Reads the live item's revision, so a deletion is applied against what the screen shows.
    func prepare() async {
        guard readScope() == .authenticated else { return }
        let thisSession = session
        do {
            let detail = try await reader.detail(itemID: itemID)
            guard thisSession == session, readScope() == .authenticated, phase != .deleting else { return }
            revision = detail.lifecycleState == "deleted" ? nil : detail.revision
        } catch {
            guard thisSession == session, phase != .deleting else { return }
            revision = nil
        }
    }

    /// Records the deletion of the item at the revision this control last read.
    func confirmDeletion() async {
        guard phase == .confirming else { return }
        guard readScope() == .authenticated else { lock(); return }
        guard let revision else { phase = .failed(DeletionText.staleText); return }
        appliedRevision = revision
        await run(revision: revision)
    }

    /// Repeats the recorded deletion so its cleanup runs again.
    func retry() async {
        guard canRetry, let revision = appliedRevision else { return }
        guard readScope() == .authenticated else { lock(); return }
        await run(revision: revision)
    }

    /// Reads how far an earlier deletion of this item has progressed, so a restart shows the actual state.
    func refreshProgress() async {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .idle || phase == .locked || phase.isPending else { return }
        let thisSession = session
        do {
            let progress = try await controls.progress(itemID: itemID)
            guard thisSession == session, readScope() == .authenticated else { return }
            if progress != .notRequested { adopt(progress) }
        } catch {
            guard thisSession == session else { return }
            if Self.isLocked(error) { lock() }
        }
    }

    /// Call when the screen goes away or the app locks. A recorded deletion is never cancelled; only the screen's
    /// transient choice is dropped.
    func clear() {
        session += 1
        if phase == .confirming || isFailed { phase = .idle }
    }

    private func run(revision: Int) async {
        phase = .deleting
        let watcher = Task { [weak self] in await self?.watchForRecordedIntent() }
        defer { watcher.cancel() }
        do {
            let receipt = try await controls.delete(itemID: itemID, expectedRevision: revision)
            // A recorded deletion is reported whatever happened to the screen meanwhile.
            isRecorded = true
            adopt(receipt.progress, settled: receipt.cleanupSettled)
        } catch {
            fail(error)
        }
    }

    /// While the delete call runs its cleanup, tells the host as soon as the core shows the intent is recorded, so the
    /// item stops being shown at that moment and not when the cleanup ends.
    private func watchForRecordedIntent() async {
        while !Task.isCancelled && !isRecorded {
            try? await Task.sleep(nanoseconds: Self.recordedPollNanoseconds)
            guard !Task.isCancelled else { return }
            if let progress = try? await controls.progress(itemID: itemID), progress != .notRequested {
                isRecorded = true
            }
        }
    }

    private func adopt(_ progress: ItemDeletionProgress, settled: Bool = true) {
        if progress != .notRequested { isRecorded = true }
        switch progress {
        case .notRequested:
            phase = .idle
        case .complete:
            canRetryPending = !settled
            phase = settled ? .complete : .pending(DeletionText.cleanupUnfinished)
        case .inProgress(let pending, let failed):
            canRetryPending = !pending.isEmpty
            phase = .pending(DeletionText.pending(pending, failed: failed))
        case .failed(let failed):
            canRetryPending = false
            phase = .pending(DeletionText.onlyFailed(failed))
        }
    }

    private func fail(_ error: Error) {
        guard let failure = error as? ItemDeletionError else {
            phase = .failed(DeletionText.unavailableText)
            return
        }
        switch failure {
        case .notAuthenticated, .relocked: lock()
        case .staleRevision(let current):
            revision = current
            phase = .failed(DeletionText.staleText)
        case .itemNotFound: phase = .failed(DeletionText.notFoundText)
        case .invalidRequest: phase = .failed(DeletionText.invalidText)
        case .core, .unreadable: phase = .failed(DeletionText.unavailableText)
        }
    }

    private static func isLocked(_ error: Error) -> Bool {
        switch error as? ItemDeletionError {
        case .some(.notAuthenticated), .some(.relocked): return true
        default: return false
        }
    }

    /// A relock hides the control's result only when nothing is recorded; a recorded deletion keeps its progress, which
    /// holds no private content.
    private func lock() {
        if phase.isPending || phase == .complete { return }
        session += 1
        phase = .locked
    }
}

extension ItemDeletionPhase {
    var isPending: Bool {
        if case .pending = self { return true }
        return false
    }
}
