import Foundation
import OhAndCoreBridge

/// What a stored control reports: the item's new revision, and whether the answer repeats an earlier identical command.
public struct ItemLifecycleControlReceipt: Equatable, Sendable {
    public let itemID: String
    public let revision: Int
    public let replayed: Bool

    public init(itemID: String, revision: Int, replayed: Bool) {
        self.itemID = itemID
        self.revision = revision
        self.replayed = replayed
    }
}

/// What a stored reopen reports: the item's new revision, whether it repeats an earlier identical command, and the
/// instant of the reminder the ending cancelled, as the core stored it (nil when there was none). Reopening never
/// reinstalls that reminder; the instant is only there so the caller can offer it again.
public struct ItemReopenReceipt: Equatable, Sendable {
    public let itemID: String
    public let revision: Int
    public let replayed: Bool
    public let formerReminderInstant: String?

    public init(itemID: String, revision: Int, replayed: Bool, formerReminderInstant: String?) {
        self.itemID = itemID
        self.revision = revision
        self.replayed = replayed
        self.formerReminderInstant = formerReminderInstant
    }
}

/// What a reopen did. `alreadyOpen` means the item was not completed or cancelled and nothing was written.
public enum ItemReopenResult: Equatable, Sendable {
    case reopened(ItemReopenReceipt)
    case alreadyOpen
}

/// Why a control was not stored. Every case except `core` and `unreadable` means the core decided and nothing changed.
public enum ItemLifecycleControlError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty ID or a negative revision).
    case invalidRequest
    /// The item has moved on since the user saw it; `currentRevision` is what to reload and ask about again.
    case staleRevision(currentRevision: Int)
    /// The command ID was already used for a different control.
    case commandConflict
    case itemDeleted
    /// The item was already completed or cancelled (`lifecycle` is `completed` or `cancelled`).
    case itemEnded(lifecycle: String)
    /// No stored item has that ID.
    case itemNotFound
    case core(CoreFailure)
    case unreadable
}

/// Completes, cancels or quiets an item through the core and nothing else.
///
/// * **Compare and set.** Every command names the revision the user saw. The core stores it through its event path
///   when that revision is still current and refuses with `staleRevision` otherwise.
/// * **Idempotent.** The caller supplies the command ID and keeps it across retries. Repeating a stored command gets
///   the first outcome (`replayed`); the same ID for another control is `commandConflict`.
/// * **Reminders follow through the reminder pass.** Completing or cancelling an item cancels its reminder in the core
///   in the same step. This service never touches the notification center: the pending notification is removed by the
///   next reminder pass (`ServiceCompositionRoot.reconcileReminders`).
/// * **Authenticated.** The session must be authenticated when the call is made; a capture-only session gets
///   `notAuthenticated` and the core is not asked. A write the core confirmed is reported truthfully even if the session
///   relocked while it ran.
public final class ItemLifecycleControlService: @unchecked Sendable {
    private let core: ItemLifecycleControlCore
    private let session: ReadAuthenticationSession
    private let changes: ItemChangePublishing?

    init(core: ItemLifecycleControlCore, session: ReadAuthenticationSession, changes: ItemChangePublishing? = nil) {
        self.core = core
        self.session = session
        self.changes = changes
    }

    public func complete(commandID: String, itemID: String, expectedRevision: Int) async throws
        -> ItemLifecycleControlReceipt
    {
        try await apply(
            ItemLifecycleControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, control: .complete))
    }

    public func cancel(commandID: String, itemID: String, expectedRevision: Int) async throws
        -> ItemLifecycleControlReceipt
    {
        try await apply(
            ItemLifecycleControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, control: .cancel))
    }

    public func notNow(commandID: String, itemID: String, expectedRevision: Int) async throws
        -> ItemLifecycleControlReceipt
    {
        try await apply(
            ItemLifecycleControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, control: .notNow))
    }

    public func stopSuggesting(commandID: String, itemID: String, expectedRevision: Int) async throws
        -> ItemLifecycleControlReceipt
    {
        try await apply(
            ItemLifecycleControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, control: .stopSuggesting))
    }

    /// Returns a completed or cancelled item to active. The reminder the ending cancelled stays cancelled and suggestion
    /// state is unchanged; the receipt carries the reminder's former instant so the caller can offer to set it again.
    public func reopen(commandID: String, itemID: String, expectedRevision: Int) async throws -> ItemReopenResult {
        let answer = try await submit(
            ItemLifecycleControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, control: .reopen))
        let result = try Self.reopenResult(from: answer)
        if case .reopened(let receipt) = result { changes?.publish(.item(receipt.itemID)) }
        return result
    }

    private func apply(_ command: ItemLifecycleControlCommand) async throws -> ItemLifecycleControlReceipt {
        let answer = try await submit(command)
        let receipt = try Self.receipt(from: answer)
        changes?.publish(.item(receipt.itemID))
        return receipt
    }

    private func submit(_ command: ItemLifecycleControlCommand) async throws -> ItemLifecycleControlAnswer {
        guard session.currentAuthorization() != nil else { throw ItemLifecycleControlError.notAuthenticated }
        guard !command.commandID.isEmpty, !command.itemID.isEmpty, command.expectedRevision >= 0 else {
            throw ItemLifecycleControlError.invalidRequest
        }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ItemLifecycleControlAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let value):
            return value
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw ItemLifecycleControlError.itemNotFound
            case "invalid_request": throw ItemLifecycleControlError.invalidRequest
            default: throw ItemLifecycleControlError.core(failure)
            }
        }
    }

    private static func reopenResult(from answer: ItemLifecycleControlAnswer) throws -> ItemReopenResult {
        switch answer.outcome {
        case "reopened":
            guard let itemID = answer.itemID, let revision = answer.revision else {
                throw ItemLifecycleControlError.unreadable
            }
            return .reopened(
                ItemReopenReceipt(
                    itemID: itemID, revision: revision, replayed: answer.replayed ?? false,
                    formerReminderInstant: answer.formerReminderInstant))
        case "already_open":
            return .alreadyOpen
        default:
            try failure(from: answer)
        }
    }

    private static func receipt(from answer: ItemLifecycleControlAnswer) throws -> ItemLifecycleControlReceipt {
        switch answer.outcome {
        case "applied":
            guard let itemID = answer.itemID, let revision = answer.revision else {
                throw ItemLifecycleControlError.unreadable
            }
            return ItemLifecycleControlReceipt(itemID: itemID, revision: revision, replayed: answer.replayed ?? false)
        default:
            try failure(from: answer)
        }
    }

    /// Throws the error a refusing answer means; every answer that is not a stored control is one.
    private static func failure(from answer: ItemLifecycleControlAnswer) throws -> Never {
        switch answer.outcome {
        case "stale_revision":
            guard let current = answer.currentRevision else { throw ItemLifecycleControlError.unreadable }
            throw ItemLifecycleControlError.staleRevision(currentRevision: current)
        case "command_conflict":
            throw ItemLifecycleControlError.commandConflict
        case "item_deleted":
            throw ItemLifecycleControlError.itemDeleted
        case "item_ended":
            guard let lifecycle = answer.lifecycle else { throw ItemLifecycleControlError.unreadable }
            throw ItemLifecycleControlError.itemEnded(lifecycle: lifecycle)
        default:
            throw ItemLifecycleControlError.unreadable
        }
    }
}
