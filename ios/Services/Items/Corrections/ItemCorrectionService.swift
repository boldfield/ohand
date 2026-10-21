import Foundation
import OhAndCoreBridge

/// What a stored correction reports: the item's new revision, whether the answer repeats an earlier identical command,
/// and whether the correction also cancelled the item's live reminder (a type change away from `action`).
public struct ItemCorrectionReceipt: Equatable, Sendable {
    public let itemID: String
    public let revision: Int
    public let replayed: Bool
    public let reminderCancelled: Bool

    public init(itemID: String, revision: Int, replayed: Bool, reminderCancelled: Bool) {
        self.itemID = itemID
        self.revision = revision
        self.replayed = replayed
        self.reminderCancelled = reminderCancelled
    }
}

/// Why a correction was not stored. Every case except `core` and `unreadable` means the core decided and nothing changed.
public enum ItemCorrectionError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty ID, a blank text or topic, a negative revision).
    case invalidRequest
    /// The item has moved on since the user saw it; `currentRevision` is what to reload and ask about again.
    case staleRevision(currentRevision: Int)
    /// The command ID was already used for a different correction.
    case commandConflict
    case itemDeleted
    /// No stored item has that ID.
    case itemNotFound
    case core(CoreFailure)
    case unreadable
}

/// Corrects an item's text, type or session topic through the core and nothing else.
///
/// * **Compare and set.** Every correction names the revision the user saw. The core stores it through its event path
///   when that revision is still current and refuses with `staleRevision` otherwise; the original capture is never
///   altered, so the correction can always be undone by a later one.
/// * **Idempotent.** The caller supplies the command ID and keeps it across retries. Repeating a stored command with
///   the same change gets the first outcome (`replayed`); the same ID for another change is `commandConflict`.
/// * **A correction beats model output.** The core refuses interpretations and transcripts that were computed before it.
/// * **Authenticated.** The session must be authenticated when the call is made; a capture-only session gets
///   `notAuthenticated` and the core is not asked. A write the core confirmed is reported truthfully even if the session
///   relocked while it ran.
public final class ItemCorrectionService: @unchecked Sendable {
    private let core: ItemCorrectionCore
    private let session: ReadAuthenticationSession
    private let changes: ItemChangePublishing?

    init(core: ItemCorrectionCore, session: ReadAuthenticationSession, changes: ItemChangePublishing? = nil) {
        self.core = core
        self.session = session
        self.changes = changes
    }

    public func correctText(
        commandID: String, itemID: String, expectedRevision: Int, text: String
    ) async throws -> ItemCorrectionReceipt {
        try await correct(
            ItemCorrectionCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, change: .text(text)))
    }

    public func correctType(
        commandID: String, itemID: String, expectedRevision: Int, itemType: ItemCorrectionItemType
    ) async throws -> ItemCorrectionReceipt {
        try await correct(
            ItemCorrectionCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, change: .itemType(itemType)))
    }

    public func correctSessionTopic(
        commandID: String, itemID: String, expectedRevision: Int, sessionTopic: String
    ) async throws -> ItemCorrectionReceipt {
        try await correct(
            ItemCorrectionCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
                change: .sessionTopic(sessionTopic)))
    }

    private func correct(_ command: ItemCorrectionCommand) async throws -> ItemCorrectionReceipt {
        guard session.currentAuthorization() != nil else { throw ItemCorrectionError.notAuthenticated }
        guard Self.isWellFormed(command) else { throw ItemCorrectionError.invalidRequest }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ItemCorrectionAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        let answer: ItemCorrectionAnswer
        switch result {
        case .success(let value):
            answer = value
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw ItemCorrectionError.itemNotFound
            case "invalid_request": throw ItemCorrectionError.invalidRequest
            default: throw ItemCorrectionError.core(failure)
            }
        }
        let receipt = try Self.receipt(from: answer)
        changes?.publish(.item(receipt.itemID))
        return receipt
    }

    private static func receipt(from answer: ItemCorrectionAnswer) throws -> ItemCorrectionReceipt {
        switch answer.outcome {
        case "corrected":
            guard let itemID = answer.itemID, let revision = answer.revision else {
                throw ItemCorrectionError.unreadable
            }
            return ItemCorrectionReceipt(
                itemID: itemID, revision: revision, replayed: answer.replayed ?? false,
                reminderCancelled: answer.reminderCancelled ?? false)
        case "stale_revision":
            guard let current = answer.currentRevision else { throw ItemCorrectionError.unreadable }
            throw ItemCorrectionError.staleRevision(currentRevision: current)
        case "command_conflict":
            throw ItemCorrectionError.commandConflict
        case "item_deleted":
            throw ItemCorrectionError.itemDeleted
        default:
            throw ItemCorrectionError.unreadable
        }
    }

    private static func isWellFormed(_ command: ItemCorrectionCommand) -> Bool {
        guard !command.commandID.isEmpty, !command.itemID.isEmpty, command.expectedRevision >= 0 else { return false }
        switch command.change {
        case .text(let text), .sessionTopic(let text):
            return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .itemType:
            return true
        }
    }
}
