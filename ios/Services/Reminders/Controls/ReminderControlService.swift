import Foundation
import OhAndCoreBridge

/// Why a reminder read or command did not succeed. Every case except `core` and `unreadable` means the core decided (or
/// the gate refused) and nothing changed.
public enum ReminderControlError: Error, Equatable, Sendable {
    /// The read session is capture-only, or relocked before the answer; nothing was delivered.
    case notAuthenticated
    /// The request was refused before it reached the core (an empty ID, a negative revision or version, an unknown zone).
    case invalidRequest
    /// The item has moved on since the user saw it; `currentRevision` is what to reload and ask about again.
    case staleRevision(currentRevision: Int)
    /// The reminder has moved on since the user saw it; `currentVersion` is what to reload and ask about again.
    case staleReminderVersion(currentVersion: Int)
    /// The chosen instant is not in the future. Nothing was stored; the user must choose a later time.
    case timeInPast
    /// The command ID was already used for a different command.
    case commandConflict
    case itemDeleted
    /// The item is completed or cancelled and can no longer carry a reminder.
    case itemNotActive(lifecycleState: String)
    /// Only an action carries a reminder.
    case itemCannotCarryReminder
    /// A cancel was asked for an item that has no reminder.
    case noReminder
    /// No readable item has that ID (unknown, deleted or outside the session's scopes; the core does not say which).
    case itemNotFound
    case core(CoreFailure)
    case unreadable
}

/// What a stored command reports: the item and the reminder as it now stands (take `stateVersion` for the next edit),
/// and whether the answer repeats an earlier identical command.
public struct ReminderControlReceipt: Equatable, Sendable {
    public let itemID: String
    public let reminder: ReminderRecordAnswer
    public let replayed: Bool

    public init(itemID: String, reminder: ReminderRecordAnswer, replayed: Bool) {
        self.itemID = itemID
        self.reminder = reminder
        self.replayed = replayed
    }
}

/// Reads, edits, cancels and reviews an item's reminder through the core and nothing else.
///
/// * **Reads are gated.** `reminder` and `history` go through the private read gate: a capture-only session gets
///   `notAuthenticated` and the core is not asked, and an answer that arrives after a relock is dropped undecoded.
/// * **Writes need an authenticated session**, checked when the call is made; a capture-only session gets
///   `notAuthenticated` and the core is not asked. A write the core confirmed is reported truthfully even if the session
///   relocked while it ran.
/// * **Compare and set.** An edit or cancel names the item revision and reminder version the user saw (take them from
///   `reminder`); the core refuses a stale one. Commands are idempotent by the caller's command ID.
/// * **No backdating, no substitution.** The core refuses an instant that is not in the future (`timeInPast`) and stores
///   exactly the instant given, floored to a whole second; it never picks another time for the user.
/// * **No native effect here.** A stored command changes what the core wants; the next reminder pass
///   (`ServiceCompositionRoot.reconcileReminders`) installs or removes the notification.
public final class ReminderControlService: @unchecked Sendable {
    private let core: ReminderControlCore
    private let session: ReadAuthenticationSession
    private let changes: ItemChangePublishing?

    init(core: ReminderControlCore, session: ReadAuthenticationSession, changes: ItemChangePublishing? = nil) {
        self.core = core
        self.session = session
        self.changes = changes
    }

    /// The item's revision and its reminder (`nil` when it has none), with the states the item status reports.
    public func reminder(itemID: String) async throws -> ReminderReadAnswer {
        guard !itemID.isEmpty else { throw ReminderControlError.invalidRequest }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ReminderReadAnswer, CoreFailure>, Never>) in
            core.readReminder(itemID: itemID) { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    /// The reminder's recorded history, newest first and bounded; `truncated` says older entries are left out.
    public func history(itemID: String) async throws -> ReminderHistoryAnswer {
        guard !itemID.isEmpty else { throw ReminderControlError.invalidRequest }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ReminderHistoryAnswer, CoreFailure>, Never>) in
            core.readHistory(itemID: itemID) { continuation.resume(returning: $0) }
        }
        return try Self.value(of: result)
    }

    /// Sets the reminder to exactly `instant` (floored to a whole second) in the zone `timeZoneID`, creating the
    /// reminder when the item has none (`expectedReminderVersion` 0).
    public func correctTime(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int, instant: Date,
        timeZoneID: String
    ) async throws -> ReminderControlReceipt {
        try await send(
            ReminderControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
                expectedReminderVersion: expectedReminderVersion,
                change: .correctTime(instant: instant, timeZoneID: timeZoneID)))
    }

    /// Cancels the item's reminder, keeping the item.
    public func cancel(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int
    ) async throws -> ReminderControlReceipt {
        try await send(
            ReminderControlCommand(
                commandID: commandID, itemID: itemID, expectedRevision: expectedRevision,
                expectedReminderVersion: expectedReminderVersion, change: .cancel))
    }

    private func send(_ command: ReminderControlCommand) async throws -> ReminderControlReceipt {
        guard session.currentAuthorization() != nil else { throw ReminderControlError.notAuthenticated }
        guard Self.isWellFormed(command) else { throw ReminderControlError.invalidRequest }
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<ReminderControlAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        let receipt = try Self.receipt(from: Self.value(of: result))
        changes?.publish(.item(receipt.itemID))
        return receipt
    }

    private static func value<Answer>(of result: Result<Answer, CoreFailure>) throws -> Answer {
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "not_found": throw ReminderControlError.itemNotFound
            case "invalid_request": throw ReminderControlError.invalidRequest
            case "read_not_authenticated", "read_relocked": throw ReminderControlError.notAuthenticated
            default: throw ReminderControlError.core(failure)
            }
        }
    }

    private static func receipt(from answer: ReminderControlAnswer) throws -> ReminderControlReceipt {
        switch answer.outcome {
        case "corrected", "cancelled":
            guard let itemID = answer.itemID, let reminder = answer.reminder else {
                throw ReminderControlError.unreadable
            }
            return ReminderControlReceipt(itemID: itemID, reminder: reminder, replayed: answer.replayed ?? false)
        case "stale_revision":
            guard let current = answer.currentRevision else { throw ReminderControlError.unreadable }
            throw ReminderControlError.staleRevision(currentRevision: current)
        case "stale_reminder_version":
            guard let current = answer.currentReminderVersion else { throw ReminderControlError.unreadable }
            throw ReminderControlError.staleReminderVersion(currentVersion: current)
        case "time_in_past":
            throw ReminderControlError.timeInPast
        case "command_conflict":
            throw ReminderControlError.commandConflict
        case "item_deleted":
            throw ReminderControlError.itemDeleted
        case "item_not_active":
            throw ReminderControlError.itemNotActive(lifecycleState: answer.lifecycleState ?? "")
        case "item_cannot_carry_reminder":
            throw ReminderControlError.itemCannotCarryReminder
        case "no_reminder":
            throw ReminderControlError.noReminder
        default:
            throw ReminderControlError.unreadable
        }
    }

    private static func isWellFormed(_ command: ReminderControlCommand) -> Bool {
        guard !command.commandID.isEmpty, !command.itemID.isEmpty, command.expectedRevision >= 0,
            command.expectedReminderVersion >= 0
        else { return false }
        if case .correctTime(_, let timeZoneID) = command.change {
            return TimeZone(identifier: timeZoneID) != nil
        }
        return true
    }
}
