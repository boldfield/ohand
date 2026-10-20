import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// Stores the user's corrections. `ItemCorrectionService` is the production implementation: it needs an authenticated
/// read session and stores each correction through the core's compare-and-set path.
protocol ItemControlsCorrecting: AnyObject, Sendable {
    func correctText(commandID: String, itemID: String, expectedRevision: Int, text: String) async throws
        -> ItemCorrectionReceipt
    func correctType(commandID: String, itemID: String, expectedRevision: Int, itemType: ItemCorrectionItemType)
        async throws -> ItemCorrectionReceipt
    func correctSessionTopic(commandID: String, itemID: String, expectedRevision: Int, sessionTopic: String)
        async throws -> ItemCorrectionReceipt
}

extension ItemCorrectionService: ItemControlsCorrecting {}

/// What the controls know about the item: the stored values they edit and the revision every correction names.
struct ItemControlsSnapshot: Equatable {
    let revision: Int
    let text: String?
    let itemType: ItemCorrectionItemType?
    let sessionTopic: String?
    let scope: String
    let isDeleted: Bool
}

/// Which correction a message or pending state is about.
enum ItemControlsField: Equatable, Hashable {
    case text, type, sessionTopic
}

/// The outcome the screen shows after an attempt. Failures leave the stored item and the user's draft untouched.
enum ItemControlsOutcome: Equatable {
    case saved(ItemControlsField)
    /// A type change that also cancelled the item's live reminder.
    case savedAndReminderCancelled
    /// The item changed since the user looked. Nothing was written; the draft is kept and the newer values are shown.
    case conflict(ItemControlsField, currentRevision: Int)
    case locked
    case itemGone
    case invalid(ItemControlsField)
    case failed(ItemControlsField)
    /// The stored values could not be read, so nothing is offered to edit.
    case loadFailed
}

/// Why the stored values could not be read again after a correction was answered. The snapshot still names the
/// revision from before the correction, so nothing can be edited until a reload succeeds.
enum ItemControlsRefreshFailure: Equatable {
    case locked
    case itemGone
    case unreadable
}

/// The correction controls for one item: edit the words, correct the type, set the session topic.
///
/// A session topic is set or replaced here; the core refuses a blank topic, so clearing one is not offered.
///
/// Rules this type keeps:
/// * Each control writes exactly one kind of correction through the existing revisioned operation. A topic change never
///   sends a scope or type; a type change never sends text or a topic. Scope is shown, never edited here.
/// * A type is chosen first and applied by a separate explicit step, so picking `action` for an idea never happens by
///   accident, and nothing in this type derives a type from text or a topic.
/// * Every correction names the revision the user saw. A stale answer is shown as a conflict, the draft stays as typed
///   and the newer stored values are loaded; the correction is only sent again when the user presses the control again.
/// * The command ID is kept across retries of the same change, so a repeat after an unknown outcome stores once.
/// * After a correction is answered, editing stays paused until the stored values are read again. If that read fails,
///   the drafts and the saved/conflict outcome are kept, `refreshFailure` says why, and editing stays paused until
///   `reload()` succeeds, so a second press can never send a correction against the revision from before. The view
///   disables the controls meanwhile, and a draft changed after its correction was sent is never replaced by the
///   stored value, so nothing typed during a save can be lost.
/// * The original capture and transcript are never touched; the source-detail screen keeps showing them.
/// Confined to the main queue.
final class ItemControlsModel: ObservableObject {
    @Published private(set) var snapshot: ItemControlsSnapshot?
    @Published private(set) var outcome: ItemControlsOutcome?
    @Published private(set) var isWorking = false
    @Published var textDraft = ""
    @Published var topicDraft = ""
    /// The type the user has picked but not yet applied.
    @Published var pendingType: ItemCorrectionItemType?
    /// Set when the read after a correction failed; the snapshot is then older than what is stored.
    @Published private(set) var refreshFailure: ItemControlsRefreshFailure?

    let itemID: String
    private let reader: SourceDetailReading
    private let corrector: ItemControlsCorrecting
    private let makeCommandID: () -> String
    private let onCorrected: @MainActor () -> Void
    private var heldCommand: (key: String, commandID: String)?
    private var loadGeneration = 0
    /// The drafts a failed post-correction read would have replaced; `reload()` replaces them once it succeeds.
    private var pendingRefreshResets: Set<ItemControlsField> = []
    /// The draft sent with the last answered correction. If the user changed that draft since, it is not replaced.
    private var sentDraft: (field: ItemControlsField, value: String)?

    init(
        itemID: String, reader: SourceDetailReading, corrector: ItemControlsCorrecting,
        makeCommandID: @escaping () -> String = { UUID().uuidString },
        onCorrected: @escaping @MainActor () -> Void = {}
    ) {
        self.itemID = itemID
        self.reader = reader
        self.corrector = corrector
        self.makeCommandID = makeCommandID
        self.onCorrected = onCorrected
    }

    var canEdit: Bool { snapshot != nil && snapshot?.isDeleted == false && !isWorking && refreshFailure == nil }

    /// Whether `reload()` is worth offering: the values could not be read again, but the item may still be readable.
    var canReload: Bool { !isWorking && (refreshFailure == .locked || refreshFailure == .unreadable) }

    /// Whether applying the picked type would turn the item into an action, which also affects reminders.
    var pendingTypeMakesAction: Bool { pendingType == .action && snapshot?.itemType != .action }

    var canApplyType: Bool {
        guard canEdit, let pendingType else { return false }
        return pendingType != snapshot?.itemType
    }

    var canSaveText: Bool {
        canEdit && !textDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && textDraft != snapshot?.text
    }

    var canSaveTopic: Bool {
        canEdit && !topicDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && topicDraft != snapshot?.sessionTopic
    }

    // MARK: loading

    /// Reads the stored values. Only the drafts named in `resetting` are replaced by what is stored, so saving one
    /// field never discards unsaved edits in the others, and a conflict (`resetting: []`) keeps everything typed.
    /// With `afterSubmit`, a failed read leaves the drafts and the saved/conflict outcome as they were, so a refresh
    /// failure never erases unsaved edits or hides the result of the correction that just happened. It records the
    /// failure in `refreshFailure` instead, which pauses editing because the snapshot is out of date.
    @MainActor func load(
        resetting fields: Set<ItemControlsField> = [.text, .type, .sessionTopic], afterSubmit: Bool = false
    ) async {
        loadGeneration += 1
        let generation = loadGeneration
        do {
            let detail = try await reader.detail(itemID: itemID)
            guard generation == loadGeneration else { return }
            let loaded = ItemControlsSnapshot(
                revision: detail.revision, text: detail.text,
                itemType: detail.itemType.flatMap(ItemCorrectionItemType.init(rawValue:)),
                sessionTopic: detail.sessionTopic, scope: detail.scope,
                isDeleted: detail.lifecycleState == "deleted")
            let resets = fields.filter { field in
                guard let sentDraft, sentDraft.field == field else { return true }
                return draftValue(field) == sentDraft.value
            }
            snapshot = loaded
            refreshFailure = nil
            pendingRefreshResets = []
            sentDraft = nil
            if resets.contains(.text) { textDraft = loaded.text ?? "" }
            if resets.contains(.sessionTopic) { topicDraft = loaded.sessionTopic ?? "" }
            if resets.contains(.type) { pendingType = loaded.itemType }
        } catch {
            guard generation == loadGeneration else { return }
            if afterSubmit {
                pendingRefreshResets.formUnion(fields)
                switch error as? ItemDetailError {
                case .some(.notAuthenticated), .some(.relocked): refreshFailure = .locked
                case .some(.notFound), .some(.invalidItemID): refreshFailure = .itemGone
                default: refreshFailure = .unreadable
                }
                return
            }
            snapshot = nil
            refreshFailure = nil
            pendingRefreshResets = []
            sentDraft = nil
            textDraft = ""
            topicDraft = ""
            pendingType = nil
            switch error as? ItemDetailError {
            case .some(.notAuthenticated), .some(.relocked): outcome = .locked
            case .some(.notFound), .some(.invalidItemID): outcome = .itemGone
            default: outcome = .loadFailed
            }
        }
    }

    private func draftValue(_ field: ItemControlsField) -> String {
        switch field {
        case .text: return textDraft
        case .sessionTopic: return topicDraft
        case .type: return pendingType?.rawValue ?? ""
        }
    }

    /// Reads the stored values again after a failed post-correction read. Drafts the user has not saved are kept.
    @MainActor func reload() async {
        guard canReload else { return }
        isWorking = true
        await load(resetting: pendingRefreshResets, afterSubmit: true)
        isWorking = false
    }

    // MARK: corrections

    @MainActor func saveText() async {
        guard canSaveText, let snapshot else { return }
        let text = textDraft
        await submit(.text, key: "text:\(text)", draft: text) { commandID in
            try await self.corrector.correctText(
                commandID: commandID, itemID: self.itemID, expectedRevision: snapshot.revision, text: text)
        }
    }

    @MainActor func applyType() async {
        guard canApplyType, let snapshot, let type = pendingType else { return }
        await submit(.type, key: "type:\(type.rawValue)", draft: type.rawValue) { commandID in
            try await self.corrector.correctType(
                commandID: commandID, itemID: self.itemID, expectedRevision: snapshot.revision, itemType: type)
        }
    }

    @MainActor func saveSessionTopic() async {
        guard canSaveTopic, let snapshot else { return }
        let topic = topicDraft
        await submit(.sessionTopic, key: "topic:\(topic)", draft: topic) { commandID in
            try await self.corrector.correctSessionTopic(
                commandID: commandID, itemID: self.itemID, expectedRevision: snapshot.revision, sessionTopic: topic)
        }
    }

    @MainActor private func submit(
        _ field: ItemControlsField, key: String, draft: String,
        _ send: (String) async throws -> ItemCorrectionReceipt
    ) async {
        guard !isWorking else { return }
        isWorking = true
        outcome = nil
        // Repeating the same change reuses its command ID, so a retry after an unknown outcome stores once.
        let commandID: String
        if let held = heldCommand, held.key == key {
            commandID = held.commandID
        } else {
            commandID = makeCommandID()
            heldCommand = (key, commandID)
        }
        do {
            let receipt = try await send(commandID)
            heldCommand = nil
            outcome = receipt.reminderCancelled && field == .type ? .savedAndReminderCancelled : .saved(field)
            sentDraft = (field, draft)
            // Editing stays paused until the new revision is read, so no second press can name the old one.
            await load(resetting: [field], afterSubmit: true)
            onCorrected()
            isWorking = false
            return
        } catch let error as ItemCorrectionError {
            switch error {
            case .staleRevision(let current):
                heldCommand = nil
                outcome = .conflict(field, currentRevision: current)
                // Show what is stored now but keep the draft the user typed.
                await load(resetting: [], afterSubmit: true)
                // The detail card above shows the same stored values, so it refreshes too.
                onCorrected()
                isWorking = false
                return
            case .notAuthenticated:
                outcome = .locked
            case .itemDeleted, .itemNotFound:
                outcome = .itemGone
            case .invalidRequest, .commandConflict:
                heldCommand = nil
                outcome = .invalid(field)
            case .core, .unreadable:
                outcome = .failed(field)
            }
        } catch {
            outcome = .failed(field)
        }
        isWorking = false
    }

    static func assembled(
        itemID: String, root: ServiceCompositionRoot, onCorrected: @escaping @MainActor () -> Void
    ) -> ItemControlsModel {
        ItemControlsModel(
            itemID: itemID, reader: root.itemDetails, corrector: root.itemCorrections, onCorrected: onCorrected)
    }
}
