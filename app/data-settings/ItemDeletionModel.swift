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
/// * Nothing is deleted without an explicit confirmation of a specific revision the user saw.
/// * The deletion is `complete` only when the service reports every cleanup effect durably completed. Until then it
///   stays `pending` with the effects still outstanding, and a retry repeats the same request (a replay) so cleanup
///   runs again without the revision having to match the changed item.
/// * An error before the core records the deletion is shown as an error and never as pending or complete.
/// * Messages are fixed words and effect names; no identifier, path or secret is shown.
@MainActor
final class ItemDeletionModel: ObservableObject {
    @Published private(set) var phase: ItemDeletionPhase = .idle

    let itemID: String
    private let controls: ItemDeletionControlling
    private let readScope: () -> CompositionReadScope
    /// The revision the recorded deletion was applied against, kept so a retry replays it.
    private var appliedRevision: Int?
    private var session = 0

    nonisolated init(itemID: String, controls: ItemDeletionControlling, readScope: @escaping () -> CompositionReadScope) {
        self.itemID = itemID
        self.controls = controls
        self.readScope = readScope
    }

    /// True while a retry makes sense: cleanup is outstanding rather than terminally failed.
    var canRetry: Bool {
        if case .pending = phase { return canRetryPending }
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

    /// Records the deletion of the item the user saw at `revision`.
    func confirmDeletion(revision: Int) async {
        guard phase == .confirming else { return }
        guard readScope() == .authenticated else { lock(); return }
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
        do {
            let receipt = try await controls.delete(itemID: itemID, expectedRevision: revision)
            // A recorded deletion is reported whatever happened to the screen meanwhile.
            adopt(receipt.progress, settled: receipt.cleanupSettled)
        } catch {
            fail(error)
        }
    }

    private func adopt(_ progress: ItemDeletionProgress, settled: Bool = true) {
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
        case .staleRevision: phase = .failed(DeletionText.staleText)
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
