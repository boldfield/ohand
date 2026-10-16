import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The suggestion operations this screen needs. `SuggestionResponseService` is the production implementation.
protocol SuggestionResponding: AnyObject, Sendable {
    func activate(notificationID: String, scope: SuggestionScope?) async throws -> SuggestionActivation
    func respond(
        commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind
    ) async throws -> SuggestionResponseReceipt
}

extension SuggestionResponseService: SuggestionResponding {}

/// What the screen shows. Only `offer` carries private text, and only while the session is authenticated.
enum SuggestionScreenState: Equatable {
    /// Nothing has been asked yet.
    case idle
    case loading
    case offer(SuggestionOffer)
    /// Nothing is eligible. Deliberately carries no count, backlog or cleanup prompt.
    case nothingToOffer
    /// The answer was stored. The screen does not roll on to another item.
    case responded(SuggestionResponseKind)
    /// The session is capture-only, so nothing is requested or shown.
    case locked
    /// The core could not answer; nothing was changed.
    case unavailable
}

/// A short message about the last response. Wording never includes stored items.
enum SuggestionScreenNotice: Equatable {
    /// The item changed or went away while it was on screen, and the screen resolved again.
    case changed
    /// The answer was not stored and can be tried again.
    case notSaved
}

/// Presents the one current eligible suggestion and routes not-now, stop and done through the revisioned response
/// operations.
///
/// Rules this type keeps:
/// * Nothing is requested before authentication. A capture-only session, or a relock while a call runs, leaves the
///   screen locked with no offer text held.
/// * Every response states the revision the user saw. A stale revision, or an item that was completed, deleted or
///   cancelled, is never retried blindly: the screen resolves the prompt again and shows what is eligible now.
/// * A response keeps one command ID across retries, so a repeat of the same tap is stored once.
/// * An empty result is calm: no count, no backlog and no request to clean up the inbox.
/// * This type never touches reminders; the core's response operation owns every stored change.
/// Confined to the main queue; the entry points that change state run on the main actor.
final class SuggestionScreenModel: ObservableObject {
    @Published private(set) var state: SuggestionScreenState = .idle
    @Published private(set) var notice: SuggestionScreenNotice?
    @Published private(set) var isBusy = false
    /// The tapped prompt the screen presents. Holds only the date-based prompt identifier, never stored content.
    @Published private(set) var promptID: String?
    /// Lets a hosted test see what the screen renders; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    private struct PendingResponse {
        let itemID: String
        let revision: Int
        let response: SuggestionResponseKind
        let commandID: String
    }

    private let responder: SuggestionResponding
    private let makeCommandID: () -> String
    private var notificationID: String?
    private var pendingResponse: PendingResponse?

    init(
        responder: SuggestionResponding,
        makeCommandID: @escaping () -> String = { UUID().uuidString.lowercased() }
    ) {
        self.responder = responder
        self.makeCommandID = makeCommandID
    }

    /// Records the prompt a tap opened. The screen opens it once it is on display, which is after authentication.
    func receive(promptID: String) {
        self.promptID = promptID
    }

    /// Opens the daily prompt `notificationID` and shows the item the core offers now, if any. Without a prompt there is
    /// nothing to open, and the screen says so calmly. A call while an answer is being stored is ignored, so a late
    /// result cannot overwrite what the newer call shows; the screen resolves again once the answer is stored.
    @MainActor func open(notificationID: String?) async {
        guard !isBusy else { return }
        self.notificationID = notificationID
        pendingResponse = nil
        notice = nil
        await resolve()
    }

    /// Asks again for what is eligible now.
    @MainActor func refresh() async {
        guard !isBusy else { return }
        await resolve()
    }

    /// Drops everything private from the screen. The host calls it when the session relocks.
    @MainActor func lock() {
        pendingResponse = nil
        notice = nil
        state = .locked
    }

    @MainActor func respond(_ response: SuggestionResponseKind) async {
        guard !isBusy, case .offer(let offer) = state, offer.responses.contains(response) else { return }
        isBusy = true
        defer { isBusy = false }
        let commandID = commandID(for: offer, response: response)
        do {
            _ = try await responder.respond(
                commandID: commandID, itemID: offer.itemID, expectedRevision: offer.revision, response: response)
            pendingResponse = nil
            notice = nil
            state = .responded(response)
        } catch SuggestionResponseError.notAuthenticated, SuggestionResponseError.relockedBeforeDelivery {
            lock()
        } catch SuggestionResponseError.staleRevision, SuggestionResponseError.noLongerCurrent,
            SuggestionResponseError.commandConflict
        {
            pendingResponse = nil
            await resolve()
            if state != .locked { notice = .changed }
        } catch {
            notice = .notSaved
        }
    }

    // MARK: private

    /// The same item, revision and answer reuse one command ID until the answer is stored or refused.
    private func commandID(for offer: SuggestionOffer, response: SuggestionResponseKind) -> String {
        if let pendingResponse, pendingResponse.itemID == offer.itemID, pendingResponse.revision == offer.revision,
            pendingResponse.response == response
        {
            return pendingResponse.commandID
        }
        let fresh = PendingResponse(
            itemID: offer.itemID, revision: offer.revision, response: response, commandID: makeCommandID())
        pendingResponse = fresh
        return fresh.commandID
    }

    @MainActor private func resolve() async {
        guard let notificationID else {
            state = .nothingToOffer
            return
        }
        state = .loading
        do {
            switch try await responder.activate(notificationID: notificationID, scope: nil) {
            case .offer(let offer): state = .offer(offer)
            case .nothingToOffer: state = .nothingToOffer
            }
        } catch SuggestionResponseError.notAuthenticated, SuggestionResponseError.relockedBeforeDelivery {
            lock()
        } catch {
            state = .unavailable
        }
    }
}
