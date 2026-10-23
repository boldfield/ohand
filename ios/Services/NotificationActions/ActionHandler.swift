import Foundation
import OhAndCoreBridge

/// What the action handler needs from the Rust core. `CoreNotificationActionClient` is the production implementation;
/// every answer is the core's word for what happened, and a throw means the core could not answer.
public protocol NotificationActionCore: Sendable {
    func recordEvidence(
        notificationID: String,
        kind: NotificationActionCommand.Evidence,
        observedAt: Date,
        itemID: String?
    ) async throws -> NotificationActionAnswer
    func acknowledge(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer
    func complete(notificationID: String, itemID: String?) async throws -> NotificationActionAnswer
    func resolve(notificationID: String) async throws -> NotificationActionAnswer
}

/// Whether the session may read or change private content right now. Authorizations are epoch-bound, so a result
/// admitted before a relock is recognized as stale even if the session has authenticated again since.
protocol ActionAuthorizing: AnyObject {
    func currentAuthorization() -> ReadAuthorization?
    func isCurrent(_ authorization: ReadAuthorization) -> Bool
}

extension ReadAuthenticationSession: ActionAuthorizing {}

/// The identifiers of the actions the app attaches to its reminder notifications.
public enum NotificationActionIdentifier {
    public static let acknowledge = "ohand.action.acknowledge"
    public static let complete = "ohand.action.complete"
    /// The category the app attaches to its reminder notifications; it carries the two actions above.
    public static let reminderCategory = "ohand.category.reminder"
}

/// What `ingest` did with one OS callback.
enum NotificationActionIngestResult: Equatable {
    /// The callback is stored (or was already stored) and will reach the core.
    case persisted
    /// The callback is not one this handler acts on, for example an unknown action identifier. Nothing was stored.
    case ignored
    /// The callback could not be stored. Nothing will reach the core unless the OS delivers it again.
    case failed
}

/// What one pass over the stored entries did.
struct NotificationActionReconciliation: Equatable {
    /// Entries the core answered definitively and that were removed, including "nothing changed" answers.
    var settled = 0
    /// Entries kept because they need an authenticated session that is not present.
    var deferred = 0
    /// Entries kept because the core could not answer or the entry could not be removed; they replay later.
    var retained = 0
    /// Entries removed because the core refused them as permanently invalid.
    var discarded = 0
    /// Items the user's taps were routed to, in order.
    var openedItemIDs: [String] = []
}

/// Routes notification taps and actions into the core's revisioned user events.
///
/// * Every callback is stored in the journal before anything else happens, so a callback that arrives while the
///   network, a provider or the core is unavailable is not lost; if it cannot be stored, `ingest` says so.
/// * Acknowledging and completing are two operations. Acknowledging records the user's explicit acknowledgment of the
///   reminder and does not touch the item; completing records a completion event on the item. Neither implies the other.
/// * Completing and following a tap to its item need an authenticated session. Until then their entries wait in the
///   journal and run at the next `reconcile`, which the app calls at launch and the handler calls itself when the
///   session authenticates (`attach(to:)`). Delivery and open evidence, and acknowledgment, name no private content and
///   run without authentication.
/// * The core decides whether an identifier is stale, the item is deleted, or the callback repeats. Those answers
///   settle the entry without changing a record, so a replayed or duplicate callback cannot resurrect or mutate twice.
/// * The tap's destination is whatever the core resolves for the notification identifier, never the payload's target.
final class ActionHandler: @unchecked Sendable {
    private let core: NotificationActionCore
    private let journal: NotificationActionJournal
    private let authorization: ActionAuthorizing
    private let openItem: @Sendable (String) -> Void
    private let ingestFailure: @Sendable (NotificationEvent) -> Void
    private let requestUnlock: @Sendable () -> Void
    private let startsDeliveryOnIngest: Bool
    private let lock = NSLock()
    private var tail: Task<Void, Never>?
    private var droppedOpenNotificationIDs: Set<String> = []
    private var dropsEveryStoredOpen = false
    /// Notifications tapped since a discard that could not read the journal; that discard must not sweep them up.
    private var opensStoredSinceUnreadDiscard: Set<String> = []

    init(
        core: NotificationActionCore,
        journal: NotificationActionJournal,
        authorization: ActionAuthorizing,
        openItem: @escaping @Sendable (String) -> Void,
        ingestFailure: @escaping @Sendable (NotificationEvent) -> Void = { _ in },
        requestUnlock: @escaping @Sendable () -> Void = {},
        startsDeliveryOnIngest: Bool = true
    ) {
        self.core = core
        self.journal = journal
        self.authorization = authorization
        self.openItem = openItem
        self.ingestFailure = ingestFailure
        self.requestUnlock = requestUnlock
        self.startsDeliveryOnIngest = startsDeliveryOnIngest
    }

    /// The handler to give `NotificationEventIngestor`.
    var eventHandler: NotificationEventHandler {
        { [self] event in _ = ingest(event) }
    }

    /// Stores the callback, then starts delivering it. Returns once the callback is durable (or has failed to be).
    @discardableResult
    func ingest(_ event: NotificationEvent) -> NotificationActionIngestResult {
        let entries = Self.entries(for: event)
        guard !entries.isEmpty else { return .ignored }
        if case .opened = event.kind {
            lock.lock()
            if dropsEveryStoredOpen { opensStoredSinceUnreadDiscard.insert(event.identifier.rawValue) }
            lock.unlock()
        }
        do {
            for entry in entries { try journal.append(entry) }
        } catch {
            ingestFailure(event)
            return .failed
        }
        // A new tap on a notification whose earlier tap was dropped is a new decision to open.
        if case .opened = event.kind {
            lock.lock()
            droppedOpenNotificationIDs.remove(event.identifier.rawValue)
            lock.unlock()
            try? journal.remove(Self.discardMarker(for: event.identifier.rawValue))
        }
        if startsDeliveryOnIngest { _ = enqueuePass() }
        return .persisted
    }

    /// Delivers every stored entry the session allows. Passes never overlap and run in the order they were requested,
    /// so when this returns every earlier pass has finished too.
    @discardableResult
    func reconcile() async -> NotificationActionReconciliation {
        await enqueuePass().value
    }

    /// Drops every stored open, after the person declined or failed to unlock for it. Later unlocks then replay
    /// nothing, so a stale tap cannot replace the screen the person chose.
    ///
    /// The drop is decided when this is called, not when the queued work runs: the opens stored now are marked
    /// dropped at once, so a pass that runs first, even one made runnable by an unrelated authentication, discards
    /// them instead of opening them. The marks outlive a journal that cannot be read or written, so a later pass
    /// still discards them. The removal runs in order with the passes.
    func discardHeldOpens() async {
        let snapshot = try? journal.pending()
        let discard: Task<Void, Never>
        lock.lock()
        if let snapshot {
            for entry in snapshot where entry.operation == .open {
                droppedOpenNotificationIDs.insert(entry.notificationID)
                // Durable, so a removal that fails and a relaunch cannot bring the open back.
                try? journal.append(Self.discardMarker(for: entry.notificationID))
            }
        } else {
            dropsEveryStoredOpen = true
        }
        let previous = tail
        discard = Task { [self] in
            _ = await previous?.value
            removeDroppedOpens()
        }
        tail = discard
        lock.unlock()
        await discard.value
    }

    private func removeDroppedOpens() {
        guard let entries = try? journal.pending() else { return }
        let dropped = adoptDurableDiscards(from: entries)
        for entry in entries where entry.operation == .open && dropped.contains(entry.notificationID) {
            do {
                try journal.remove(entry)
            } catch {
                continue
            }
        }
        // A mark stays until no stored open it names is left, so a removal that failed is still discarded later.
        let remaining = Set(((try? journal.pending()) ?? entries).filter { $0.operation == .open }.map(\.notificationID))
        lock.lock()
        droppedOpenNotificationIDs.formIntersection(remaining)
        lock.unlock()
        removeSpentDiscardMarkers(in: entries, remainingOpens: remaining)
    }

    private static func discardMarker(for notificationID: String) -> NotificationActionEntry {
        NotificationActionEntry(
            operation: .discardOpen, notificationID: notificationID, targetID: nil, occurredAtUnixMilliseconds: 0)
    }

    /// Turns what a journal read shows into the in-memory marks: stored discard markers, and a discard that could not
    /// read the journal when it was made (which covers the opens stored then, not taps made since). Returns the marks.
    private func adoptDurableDiscards(from entries: [NotificationActionEntry]) -> Set<String> {
        var newlyDropped: [String] = []
        lock.lock()
        for entry in entries where entry.operation == .discardOpen {
            droppedOpenNotificationIDs.insert(entry.notificationID)
        }
        if dropsEveryStoredOpen {
            for entry in entries where entry.operation == .open
                && !opensStoredSinceUnreadDiscard.contains(entry.notificationID) {
                droppedOpenNotificationIDs.insert(entry.notificationID)
                newlyDropped.append(entry.notificationID)
            }
            dropsEveryStoredOpen = false
            opensStoredSinceUnreadDiscard = []
        }
        let marks = droppedOpenNotificationIDs
        lock.unlock()
        for notificationID in newlyDropped { try? journal.append(Self.discardMarker(for: notificationID)) }
        return marks
    }

    /// A discard marker is spent once no stored open it names is left.
    private func removeSpentDiscardMarkers(in entries: [NotificationActionEntry], remainingOpens: Set<String>) {
        for entry in entries where entry.operation == .discardOpen && !remainingOpens.contains(entry.notificationID) {
            try? journal.remove(entry)
        }
    }

    private func isDroppedOpen(_ entry: NotificationActionEntry) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return droppedOpenNotificationIDs.contains(entry.notificationID)
    }

    /// Reconciles whenever the session authenticates, which is when deferred entries become runnable.
    @discardableResult
    func attach(to session: ReadAuthenticationSession) -> SessionScopeObserverToken {
        session.observeScope { [weak self] transition in
            guard transition.scope == .authenticated else { return }
            _ = self?.enqueuePass()
        }
    }

    // MARK: private

    private func enqueuePass() -> Task<NotificationActionReconciliation, Never> {
        lock.lock()
        defer { lock.unlock() }
        let previous = tail
        let pass = Task<NotificationActionReconciliation, Never> { [self] in
            _ = await previous?.value
            return await drain()
        }
        tail = Task<Void, Never> { _ = await pass.value }
        return pass
    }

    private static func entries(for event: NotificationEvent) -> [NotificationActionEntry] {
        func entry(_ operation: NotificationActionEntry.Operation) -> NotificationActionEntry {
            NotificationActionEntry(
                operation: operation,
                notificationID: event.identifier.rawValue,
                targetID: event.opaqueTargetID?.rawValue,
                occurredAtUnixMilliseconds: Int64((event.occurredAt.timeIntervalSince1970 * 1000).rounded()))
        }
        switch event.kind {
        case .delivered:
            return [entry(.recordDelivered)]
        case .opened:
            return [entry(.recordOpened), entry(.open)]
        case .action(let action):
            switch action.rawValue {
            case NotificationActionIdentifier.acknowledge: return [entry(.acknowledge)]
            case NotificationActionIdentifier.complete: return [entry(.complete)]
            default: return []
            }
        }
    }

    private enum Disposition {
        case settled
        case deferred
        case retained
        case discarded
    }

    private func drain() async -> NotificationActionReconciliation {
        var report = NotificationActionReconciliation()
        var heldOpen = false
        let entries: [NotificationActionEntry]
        do {
            entries = try journal.pending()
        } catch {
            report.retained += 1
            return report
        }
        _ = adoptDurableDiscards(from: entries)
        let openIDsBeforePass = Set(entries.filter { $0.operation == .open }.map(\.notificationID))
        var openIDsLeft = openIDsBeforePass
        for entry in entries {
            if entry.operation == .discardOpen { continue }
            var openedItemID: String?
            let disposition = await process(entry, openedItemID: &openedItemID)
            switch disposition {
            case .deferred:
                report.deferred += 1
                if entry.operation == .open { heldOpen = true }
                continue
            case .retained:
                report.retained += 1
                continue
            case .settled, .discarded:
                break
            }
            do {
                try journal.remove(entry)
            } catch {
                report.retained += 1
                continue
            }
            if entry.operation == .open { openIDsLeft.remove(entry.notificationID) }
            if disposition == .settled { report.settled += 1 } else { report.discarded += 1 }
            if let openedItemID { report.openedItemIDs.append(openedItemID) }
        }
        removeSpentDiscardMarkers(in: entries, remainingOpens: openIDsLeft)
        // A tap that is waiting for authentication asks the person to unlock, once per pass, as a prompt tap does.
        if heldOpen { requestUnlock() }
        return report
    }

    private func process(_ entry: NotificationActionEntry, openedItemID: inout String?) async -> Disposition {
        do {
            switch entry.operation {
            case .recordDelivered, .recordOpened:
                _ = try await core.recordEvidence(
                    notificationID: entry.notificationID,
                    kind: entry.operation == .recordDelivered ? .delivered : .opened,
                    observedAt: entry.occurredAt,
                    itemID: entry.targetID)
            case .acknowledge:
                _ = try await core.acknowledge(notificationID: entry.notificationID, itemID: entry.targetID)
            case .complete:
                guard authorization.currentAuthorization() != nil else { return .deferred }
                _ = try await core.complete(notificationID: entry.notificationID, itemID: entry.targetID)
            case .open:
                // A tap the person declined to unlock for is never opened, even if a session authenticated since.
                if isDroppedOpen(entry) { return .discarded }
                guard let admitted = authorization.currentAuthorization() else { return .deferred }
                let answer = try await core.resolve(notificationID: entry.notificationID)
                if answer.outcome == "item", let itemID = answer.itemID {
                    // The session may have relocked, even relocked and authenticated again, while the core was
                    // answering: the answer belongs to the earlier epoch, so show nothing and keep the tap.
                    guard authorization.isCurrent(admitted) else { return .deferred }
                    openItem(itemID)
                    openedItemID = itemID
                }
            }
            return .settled
        } catch let failure as CoreFailure {
            switch failure.errorClass {
            case .permanent, .unsupported: return .discarded
            case .transient, .unauthorized, .cancelled: return .retained
            }
        } catch {
            return .retained
        }
    }
}
