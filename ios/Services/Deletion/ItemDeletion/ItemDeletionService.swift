import Foundation
import OhAndCoreBridge

/// How far an item's deletion has progressed, as the core derives it from durable cleanup rows only.
public enum ItemDeletionProgress: Equatable, Sendable {
    /// The item has no deletion intent.
    case notRequested
    /// The item is deleted and unreadable; `pending` effects still have to run and `failed` ones are terminally failed.
    case inProgress(pending: [DeletionCleanupKind], failed: [DeletionCleanupKind])
    /// Every other effect finished but these terminally failed. They are never retried.
    case failed([DeletionCleanupKind])
    /// Every cleanup effect is durably completed.
    case complete
}

/// What a recorded deletion reports. `progress` is read after the cleanup service ran, so it is the truth at return;
/// `cleanupSettled` is false when cleanup left something pending or failed, or could not run at all, in which case the
/// normal lifecycle pass resumes it.
public struct ItemDeletionReceipt: Equatable, Sendable {
    public let itemID: String
    /// The revision the deletion was applied against.
    public let deletionRevision: Int
    /// True when the item was already deleted by an identical request; nothing was written.
    public let replayed: Bool
    public let progress: ItemDeletionProgress
    public let cleanupSettled: Bool

    public init(
        itemID: String, deletionRevision: Int, replayed: Bool, progress: ItemDeletionProgress, cleanupSettled: Bool
    ) {
        self.itemID = itemID
        self.deletionRevision = deletionRevision
        self.replayed = replayed
        self.progress = progress
        self.cleanupSettled = cleanupSettled
    }
}

/// Why a deletion or progress request was not answered. `core` and `unreadable` mean the outcome is unknown; every
/// other case means the core decided and nothing changed.
public enum ItemDeletionError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The session relocked while a progress read was in flight; its answer was discarded unread.
    case relocked
    /// The request was refused before it reached the core (an empty ID or a negative revision).
    case invalidRequest
    /// The item has moved on, or was deleted at another revision; `currentRevision` is what to reload and ask about.
    case staleRevision(currentRevision: Int)
    /// No stored item has that ID.
    case itemNotFound
    case core(CoreFailure)
    case unreadable
}

/// Runs the native cleanup of one deleted item. `DeletionCleanupService` is the production implementation; it performs
/// the effects, records each completion with the core and reports what is left.
protocol ItemDeletionCleanup: Sendable {
    func cleanUp(itemID: String) async -> DeletionItemResult
}

extension DeletionCleanupService: ItemDeletionCleanup {}

/// Deletes one item through the core and then asks the existing deletion cleanup service to remove its native effects.
///
/// * **Intent first.** `delete` records the intent through the core in one transaction (tombstone, content and search
///   erased, jobs and reminders cancelled, cleanup rows enqueued). Only after the core confirms does it trigger
///   cleanup. The service never touches files or notifications itself.
/// * **Cleanup never undoes the intent.** If cleanup is unavailable or leaves work pending, the deletion still stands
///   and is reported truthfully; the normal lifecycle pass completes it. Repeating `delete` with the revision it was
///   first applied against is a replay that retries cleanup.
/// * **Authenticated.** The session must be authenticated when the call is made; a capture-only session gets
///   `notAuthenticated` and the core is not asked. A deletion the core confirmed is reported even if the session
///   relocked while it ran, but only with the progress the deletion answer itself carried.
/// * **Gated progress.** A progress read reports state derived from stored history, so it goes through the private
///   read gate: an answer that arrives after a relock is discarded and the caller gets `relocked`.
public final class ItemDeletionService: @unchecked Sendable {
    private let core: ItemDeletionCore
    private let session: ReadAuthenticationSession
    private let lock = NSLock()
    private var cleanupProvider: (@Sendable () -> ItemDeletionCleanup?)?
    private var intentObserver: (@Sendable (String) -> Void)?
    private let changes: ItemChangePublishing?

    init(core: ItemDeletionCore, session: ReadAuthenticationSession, changes: ItemChangePublishing? = nil) {
        self.core = core
        self.session = session
        self.changes = changes
    }

    /// Sets where the cleanup service comes from. It is resolved at each call, because the lifecycle coordinator that
    /// owns it is connected after this service is built.
    func setCleanupProvider(_ provider: (@Sendable () -> ItemDeletionCleanup?)?) {
        lock.lock()
        cleanupProvider = provider
        lock.unlock()
    }

    /// Sets what is told, once the core has recorded the deletion of `itemID` and before cleanup runs, that it is
    /// deleted (it ends the item's open audio playback).
    func setIntentObserver(_ observer: (@Sendable (String) -> Void)?) {
        lock.lock()
        intentObserver = observer
        lock.unlock()
    }

    private var cleanup: ItemDeletionCleanup? {
        lock.lock()
        let provider = cleanupProvider
        lock.unlock()
        return provider?()
    }

    /// Records the deletion of `itemID`, which the user saw at `expectedRevision`, and triggers its cleanup at once.
    public func delete(itemID: String, expectedRevision: Int) async throws -> ItemDeletionReceipt {
        guard session.currentAuthorization() != nil else { throw ItemDeletionError.notAuthenticated }
        guard !itemID.isEmpty, expectedRevision >= 0 else { throw ItemDeletionError.invalidRequest }
        let answer = try await ask(.markDeleted(itemID: itemID, expectedRevision: expectedRevision))
        let recorded = try Self.recorded(from: answer)
        // Only a recorded deletion (a replay counts) ends playback; a refused, stale or failed one never reaches here.
        lock.lock()
        let observer = intentObserver
        lock.unlock()
        observer?(recorded.itemID)
        changes?.publish(.item(recorded.itemID))

        var cleanupSettled = false
        if let cleanup {
            cleanupSettled = await cleanup.cleanUp(itemID: recorded.itemID).isSettled
        }
        // A refused or failed read (the session may have relocked meanwhile) falls back to what the deletion answer
        // itself reported; a fresh read is never delivered without the gate's admission.
        let progress = (try? await readProgress(itemID: recorded.itemID)) ?? recorded.progress
        return ItemDeletionReceipt(
            itemID: recorded.itemID, deletionRevision: recorded.deletionRevision, replayed: recorded.replayed,
            progress: progress, cleanupSettled: cleanupSettled && progress == .complete)
    }

    /// How far the deletion of `itemID` has progressed. An item that was never deleted is `notRequested`.
    public func progress(itemID: String) async throws -> ItemDeletionProgress {
        guard session.currentAuthorization() != nil else { throw ItemDeletionError.notAuthenticated }
        guard !itemID.isEmpty else { throw ItemDeletionError.invalidRequest }
        return try await readProgress(itemID: itemID)
    }

    private func readProgress(itemID: String) async throws -> ItemDeletionProgress {
        let answer = try await ask(.progress(itemID: itemID))
        guard answer.outcome == "progress", let payload = answer.progress else { throw ItemDeletionError.unreadable }
        return try Self.progress(from: payload)
    }

    private func ask(_ command: ItemDeletionCommand) async throws -> ItemDeletionAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ItemDeletionAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "read_not_authenticated": throw ItemDeletionError.notAuthenticated
            case "read_relocked": throw ItemDeletionError.relocked
            case "not_found": throw ItemDeletionError.itemNotFound
            case "invalid_request": throw ItemDeletionError.invalidRequest
            default: throw ItemDeletionError.core(failure)
            }
        }
    }

    private struct Recorded {
        let itemID: String
        let deletionRevision: Int
        let replayed: Bool
        let progress: ItemDeletionProgress
    }

    private static func recorded(from answer: ItemDeletionAnswer) throws -> Recorded {
        switch answer.outcome {
        case "deletion_recorded":
            guard let itemID = answer.itemID, let deletionRevision = answer.deletionRevision,
                let payload = answer.progress
            else { throw ItemDeletionError.unreadable }
            return Recorded(
                itemID: itemID, deletionRevision: deletionRevision, replayed: answer.replayed ?? false,
                progress: try progress(from: payload))
        case "stale_revision":
            guard let current = answer.currentRevision else { throw ItemDeletionError.unreadable }
            throw ItemDeletionError.staleRevision(currentRevision: current)
        default:
            throw ItemDeletionError.unreadable
        }
    }

    private static func progress(from payload: ItemDeletionProgressPayload) throws -> ItemDeletionProgress {
        func kinds(_ names: [String]) throws -> [DeletionCleanupKind] {
            try names.map { name in
                guard let kind = DeletionCleanupKind(rawValue: name) else { throw ItemDeletionError.unreadable }
                return kind
            }
        }
        switch payload.state {
        case "not_requested": return .notRequested
        case "in_progress": return .inProgress(pending: try kinds(payload.pending), failed: try kinds(payload.failed))
        case "failed": return .failed(try kinds(payload.failed))
        case "complete": return .complete
        default: throw ItemDeletionError.unreadable
        }
    }
}
