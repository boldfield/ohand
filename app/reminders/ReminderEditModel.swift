import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// Reads, edits and cancels one item's reminder. `ReminderControlService` is the production implementation: reads need
/// an authenticated session, and the core refuses an edit made against a revision or version the user did not see.
protocol ReminderEditing: AnyObject, Sendable {
    func reminder(itemID: String) async throws -> ReminderReadAnswer
    func correctTime(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int, instant: Date,
        timeZoneID: String
    ) async throws -> ReminderControlReceipt
    func cancel(
        commandID: String, itemID: String, expectedRevision: Int, expectedReminderVersion: Int
    ) async throws -> ReminderControlReceipt
}

extension ReminderControlService: ReminderEditing {}

/// Why the section has nothing to show.
enum ReminderEditUnavailable: Equatable {
    case locked
    /// The item cannot carry a reminder: only an action can.
    case notAnAction
    case unreadable
}

/// The reminder section's state for one item.
///
/// Rules this type keeps:
/// * The core is the only owner of the reminder. An edit names the item revision and reminder version the user saw and
///   the core refuses a stale one; this type never writes a time of its own choosing and never presents a model's
///   reading of a request as the user's choice.
/// * One desired reminder. A save or cancel keeps its command ID until it is stored or the choice changes, so retrying
///   after an unconfirmed answer, or after the section is reopened with the same choice, repeats the same command and
///   the core answers it once. A changed choice is a new command against the version the user last saw; the core
///   cancels the earlier schedule generation, so a stale request is never left installed beside the new one.
/// * Saving is storing. Nothing here says the system holds the reminder: `standing` comes from the core's schedule
///   state and the notification permission, and a stored edit asks for a reminder pass (`reconcile`) whose outcome
///   decides what is shown.
/// * A locked answer clears everything shown. Confined to the main queue.
final class ReminderEditModel: ObservableObject {
    @Published private(set) var reminder: ReminderRecordAnswer?
    @Published private(set) var unavailable: ReminderEditUnavailable?
    @Published private(set) var isLoading = false
    @Published private(set) var isWorking = false
    /// The item is completed or cancelled. The core refuses every edit to it, so none is offered.
    @Published private(set) var isClosed = false
    @Published private(set) var message: ReminderEditMessage?
    @Published private(set) var permission: NotificationPermissionState?
    @Published var draftDate: Date {
        didSet { if draftDate != oldValue { draftChanged() } }
    }
    @Published var draftTimeZoneID: String {
        didSet { if draftTimeZoneID != oldValue { draftChanged() } }
    }

    let itemID: String
    private let editor: ReminderEditing
    private let reconcile: () async -> NotificationPermissionState?
    private let now: () -> Date
    private let deviceTimeZone: () -> TimeZone
    private var itemRevision = 0
    private var loadGeneration = 0
    /// Bumped only by `lock()`, so an edit in flight can tell the section locked while it waited.
    private var lockGeneration = 0
    private var draftIsUserChoice = false
    private var pendingCommand: (id: String, choice: String)?

    /// `reconcile` runs one reminder pass and returns the notification permission it observed, or nil when it could
    /// not tell. It is the only route by which a stored edit reaches the system.
    init(
        itemID: String, editor: ReminderEditing, reconcile: @escaping () async -> NotificationPermissionState? = { nil },
        now: @escaping () -> Date = Date.init, deviceTimeZone: @escaping () -> TimeZone = { TimeZone.current }
    ) {
        self.itemID = itemID
        self.editor = editor
        self.reconcile = reconcile
        self.now = now
        self.deviceTimeZone = deviceTimeZone
        draftDate = now().addingTimeInterval(3600)
        draftTimeZoneID = deviceTimeZone().identifier
    }

    var standing: ReminderStanding { .of(reminder, permission: permission, now: now()) }
    var canSave: Bool { unavailable == nil && !isClosed && !isLoading && !isWorking }
    var canCancel: Bool { canSave && standing.offersCancel }
    var isShown: Bool { unavailable != .notAnAction }
    /// The last edit's outcome, or the standing closed notice when the item can no longer carry a change.
    var shownMessage: ReminderEditMessage? { message ?? (isClosed ? .itemClosed : nil) }

    // MARK: loading

    /// Reads the reminder. Anything shown earlier is dropped when the read fails, so stale text never stays.
    @MainActor func load() async {
        loadGeneration += 1
        let generation = loadGeneration
        isLoading = true
        defer { if generation == loadGeneration { isLoading = false } }
        do {
            let answer = try await editor.reminder(itemID: itemID)
            guard generation == loadGeneration else { return }
            adopt(answer)
            await learnPermission(generation: generation)
        } catch {
            guard generation == loadGeneration else { return }
            reject(error)
        }
    }

    /// A stored time shows the notification permission from the first read, not only after an edit in this session.
    @MainActor private func learnPermission(generation: Int) async {
        guard permission == nil, reminder?.requestState == "resolved" else { return }
        let observed = await reconcile()
        guard generation == loadGeneration, let observed else { return }
        permission = observed
    }

    @MainActor private func adopt(_ answer: ReminderReadAnswer) {
        guard answer.itemType == "action" else {
            clear()
            unavailable = .notAnAction
            return
        }
        unavailable = nil
        itemRevision = answer.itemRevision
        isClosed = answer.lifecycleState != "active"
        reminder = answer.reminder
        if !draftIsUserChoice { seedDraft(from: answer.reminder) }
    }

    @MainActor private func reject(_ error: Error) {
        clear()
        switch error as? ReminderControlError {
        case .some(.notAuthenticated): unavailable = .locked
        case .some(.itemNotFound), .some(.itemCannotCarryReminder): unavailable = .notAnAction
        default: unavailable = .unreadable
        }
    }

    @MainActor private func clear() {
        reminder = nil
        message = nil
        permission = nil
        isClosed = false
        pendingCommand = nil
        draftIsUserChoice = false
    }

    private func seedDraft(from reminder: ReminderRecordAnswer?) {
        let seeded = deviceTimeZone().identifier
        let zone = reminder?.timezoneID.flatMap { TimeZone(identifier: $0) != nil ? $0 : nil } ?? seeded
        let date = reminder?.resolvedDate.flatMap { $0 > now() ? $0 : nil } ?? now().addingTimeInterval(3600)
        // Assigning would mark the draft as the user's choice; this is the stored value being shown.
        isSeeding = true
        draftTimeZoneID = zone
        draftDate = date
        isSeeding = false
    }

    private var isSeeding = false

    private func draftChanged() {
        guard !isSeeding else { return }
        draftIsUserChoice = true
        // A different choice is a different command; an unconfirmed earlier one is not retried under it.
        pendingCommand = nil
    }

    // MARK: edits

    /// Stores the drafted date and time. A time that is not in the future is refused here and by the core.
    @MainActor func save() async {
        guard canSave else { return }
        guard TimeZone(identifier: draftTimeZoneID) != nil else {
            message = .invalidZone
            return
        }
        let instant = Date(timeIntervalSince1970: draftDate.timeIntervalSince1970.rounded(.down))
        guard instant > now() else {
            message = .timeInPast
            return
        }
        let zoneID = draftTimeZoneID
        let commandID = command(for: "time|\(Int(instant.timeIntervalSince1970))|\(zoneID)")
        let revision = itemRevision
        let version = reminder?.stateVersion ?? 0
        await perform(stored: .saved) { [editor, itemID] in
            try await editor.correctTime(
                commandID: commandID, itemID: itemID, expectedRevision: revision, expectedReminderVersion: version,
                instant: instant, timeZoneID: zoneID)
        }
    }

    /// Cancels the reminder, keeping the item.
    @MainActor func cancel() async {
        guard canCancel, let version = reminder?.stateVersion else { return }
        let commandID = command(for: "cancel")
        let revision = itemRevision
        await perform(stored: .cancelled) { [editor, itemID] in
            try await editor.cancel(
                commandID: commandID, itemID: itemID, expectedRevision: revision, expectedReminderVersion: version)
        }
    }

    private func command(for choice: String) -> String {
        if let pendingCommand, pendingCommand.choice == choice { return pendingCommand.id }
        let fresh = (id: UUID().uuidString.lowercased(), choice: choice)
        pendingCommand = fresh
        return fresh.id
    }

    @MainActor private func perform(
        stored outcome: ReminderEditMessage, _ send: () async throws -> ReminderControlReceipt
    ) async {
        isWorking = true
        message = nil
        let locks = lockGeneration
        defer { isWorking = false }
        do {
            let receipt = try await send()
            guard locks == lockGeneration else { return }
            pendingCommand = nil
            draftIsUserChoice = false
            reminder = receipt.reminder
            message = outcome
            // Installation is the reminder pass's work. Ask for it, then show what the core now says.
            let observed = await reconcile()
            guard locks == lockGeneration else { return }
            if let observed { permission = observed }
            await refreshAfterStore(locks: locks)
        } catch let error as ReminderControlError {
            guard locks == lockGeneration else { return }
            await handle(error)
        } catch {
            guard locks == lockGeneration else { return }
            message = .notConfirmed
        }
    }

    /// Re-reads after a stored edit so the schedule state shown is the core's, not the one the edit returned.
    @MainActor private func refreshAfterStore(locks: Int) async {
        let answer = try? await editor.reminder(itemID: itemID)
        guard locks == lockGeneration, let answer, answer.itemType == "action" else { return }
        itemRevision = answer.itemRevision
        isClosed = answer.lifecycleState != "active"
        reminder = answer.reminder
    }

    @MainActor private func handle(_ error: ReminderControlError) async {
        switch error {
        case .notAuthenticated:
            clear()
            unavailable = .locked
        case .timeInPast:
            message = .timeInPast
        case .staleRevision, .staleReminderVersion:
            // Something else moved the reminder. Show the stored one; the user chooses again against it.
            pendingCommand = nil
            await load()
            if unavailable == nil { message = .changedElsewhere }
        case .itemDeleted, .itemNotActive, .itemCannotCarryReminder:
            message = .itemClosed
            await load()
        case .invalidRequest, .noReminder:
            await load()
        case .commandConflict:
            pendingCommand = nil
            message = .notConfirmed
        case .itemNotFound:
            clear()
            unavailable = .notAnAction
        case .core, .unreadable:
            // The outcome is unknown; the command ID is kept so a retry is answered once.
            message = .notConfirmed
        }
    }

    /// Drops what is shown when the read session ends.
    @MainActor func lock() {
        loadGeneration += 1
        lockGeneration += 1
        clear()
        unavailable = .locked
    }

    static func assembled(itemID: String, root: ServiceCompositionRoot) -> ReminderEditModel {
        ReminderEditModel(
            itemID: itemID, editor: root.reminderControls,
            reconcile: { [weak root] in
                guard let root, let report = try? await root.reconcileReminders() else { return nil }
                return report.permission
            })
    }
}
