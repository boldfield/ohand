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
    /// Bumped by every `open`, so running work can tell that a newer prompt arrived while it was waiting.
    private var openGeneration = 0
    /// The `openGeneration` at the last relock. While no prompt has been opened since, nothing is requested.
    private var lockedGeneration = -1

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
    /// nothing to open, and the screen says so calmly. Only one call to the core runs at a time, and the latest prompt is
    /// always kept: a call while another is running records the prompt, and the running work resolves it before it
    /// finishes, however many prompts arrive meanwhile.
    @MainActor func open(notificationID: String?) async {
        openGeneration += 1
        self.notificationID = notificationID
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        pendingResponse = nil
        notice = nil
        await resolveLatest()
    }

    /// Asks again for what is eligible now.
    @MainActor func refresh() async {
        guard !isBusy else { return }
        isBusy = true
        defer { isBusy = false }
        await resolveLatest()
    }

    /// Drops everything private from the screen. The host calls it when the session relocks.
    @MainActor func lock() {
        lockedGeneration = openGeneration
        pendingResponse = nil
        notice = nil
        state = .locked
    }

    @MainActor func respond(_ response: SuggestionResponseKind) async {
        guard !isBusy, case .offer(let offer) = state, offer.responses.contains(response) else { return }
        isBusy = true
        defer { isBusy = false }
        let startedGeneration = openGeneration
        let commandID = commandID(for: offer, response: response)
        do {
            _ = try await responder.respond(
                commandID: commandID, itemID: offer.itemID, expectedRevision: offer.revision, response: response)
            pendingResponse = nil
            notice = nil
            if openGeneration != startedGeneration {
                await resolveLatest()
            } else if lockedGeneration != openGeneration {
                state = .responded(response)
            }
        } catch SuggestionResponseError.notAuthenticated, SuggestionResponseError.relockedBeforeDelivery {
            lock()
        } catch SuggestionResponseError.staleRevision, SuggestionResponseError.noLongerCurrent,
            SuggestionResponseError.commandConflict
        {
            pendingResponse = nil
            await resolveLatest()
            if state != .locked { notice = .changed }
        } catch {
            if openGeneration != startedGeneration {
                pendingResponse = nil
                await resolveLatest()
            } else if lockedGeneration != openGeneration {
                notice = .notSaved
            }
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

    /// Resolves the newest prompt. If another prompt arrives while an activation runs, that result is dropped and the
    /// newer prompt is resolved, until the result belongs to the newest prompt. A relock with no prompt opened since
    /// stops it, so nothing private is shown on a locked screen.
    @MainActor private func resolveLatest() async {
        while lockedGeneration != openGeneration {
            let generation = openGeneration
            guard let notificationID else {
                state = .nothingToOffer
                return
            }
            state = .loading
            let outcome: Result<SuggestionActivation, Error>
            do {
                outcome = .success(try await responder.activate(notificationID: notificationID, scope: nil))
            } catch {
                outcome = .failure(error)
            }
            guard openGeneration == generation else { continue }
            guard lockedGeneration != generation else { return }
            switch outcome {
            case .success(.offer(let offer)): state = .offer(offer)
            case .success(.nothingToOffer): state = .nothingToOffer
            case .failure(SuggestionResponseError.notAuthenticated),
                .failure(SuggestionResponseError.relockedBeforeDelivery):
                lock()
            case .failure: state = .unavailable
            }
            return
        }
    }
}
