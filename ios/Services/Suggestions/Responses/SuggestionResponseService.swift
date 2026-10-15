import Foundation
import OhAndCoreBridge

/// The item a prompt offers. `text` is what may be displayed and is `nil` when the item has none (an untranscribed
/// voice capture). `revision` is the revision the user is shown; a response must state it.
public struct SuggestionOffer: Equatable, Sendable {
    public let itemID: String
    public let revision: Int
    public let text: String?
    public let scope: SuggestionScope
    public let responses: [SuggestionResponseKind]

    public init(
        itemID: String, revision: Int, text: String?, scope: SuggestionScope, responses: [SuggestionResponseKind]
    ) {
        self.itemID = itemID
        self.revision = revision
        self.text = text
        self.scope = scope
        self.responses = responses
    }
}

/// What opening a prompt found.
public enum SuggestionActivation: Equatable, Sendable {
    case offer(SuggestionOffer)
    /// Nothing is eligible any more. Deliberately carries no count or backlog.
    case nothingToOffer
}

/// What a stored response reports: whether the answer repeats an earlier identical command.
public struct SuggestionResponseReceipt: Equatable, Sendable {
    public let replayed: Bool

    public init(replayed: Bool) {
        self.replayed = replayed
    }
}

/// Why a suggestion call did not succeed. Every case except `core` and `unreadable` means the core decided and
/// nothing changed.
public enum SuggestionResponseError: Error, Equatable, Sendable {
    /// The read session is capture-only; nothing was requested from the core and nothing is disclosed.
    case notAuthenticated
    /// The session relocked while an activation ran, so its answer was dropped undelivered.
    case relockedBeforeDelivery
    /// The request was refused before it reached the core (an empty ID, a negative revision, a notification that is
    /// not a daily prompt).
    case invalidRequest
    /// The item changed since the user saw it; `currentRevision` is what the offer would show now.
    case staleRevision(currentRevision: Int)
    /// The item was deleted, completed, cancelled or is gone, so there is nothing to respond to.
    case noLongerCurrent
    /// The command ID was already used for a different response.
    case commandConflict
    case core(CoreFailure)
    case unreadable
}

/// Opens a daily prompt and records the user's answer through the core and nothing else.
///
/// * **Authenticated.** Both calls need an authenticated read session when they are made; a capture-only session gets
///   `notAuthenticated` and the core is not asked, so nothing is disclosed before authentication. An activation also
///   goes through the private read gate, so an answer that arrives after a relock is discarded. A response the core
///   confirmed is reported truthfully even if the session relocked while it ran.
/// * **Honest offers.** Eligibility is evaluated by the core over current state when the prompt is opened. When nothing
///   is eligible the answer is `nothingToOffer`, not a stale or invented suggestion.
/// * **Revisioned and idempotent responses.** Every response names the revision the offer showed and a command ID the
///   caller keeps across retries. A stale revision, a vanished item and a reused command ID are refused as typed
///   errors and write nothing.
/// * **Reminders.** Done completes the item and cancels its own reminder in the same core transaction; the pending OS
///   request is removed by the next reminder pass, never by this service.
public final class SuggestionResponseService: @unchecked Sendable {
    private let core: SuggestionResponseCore
    private let session: ReadAuthenticationSession

    init(core: SuggestionResponseCore, session: ReadAuthenticationSession) {
        self.core = core
        self.session = session
    }

    /// Opens the daily prompt `notificationID` and returns the one item to offer, if any. `scope` limits the offer to
    /// one item scope; `nil` allows every scope the session may read.
    public func activate(notificationID: String, scope: SuggestionScope? = nil) async throws -> SuggestionActivation {
        guard session.currentAuthorization() != nil else { throw SuggestionResponseError.notAuthenticated }
        guard !notificationID.isEmpty else { throw SuggestionResponseError.invalidRequest }
        let answer = try await ask(.activatePrompt(notificationID: notificationID, scope: scope))
        switch answer.outcome {
        case "offer":
            guard let itemID = answer.itemID, let revision = answer.revision, let rawScope = answer.scope,
                let offeredScope = SuggestionScope(rawValue: rawScope)
            else { throw SuggestionResponseError.unreadable }
            let responses = (answer.responses ?? []).compactMap(SuggestionResponseKind.init(rawValue:))
            return .offer(
                SuggestionOffer(
                    itemID: itemID, revision: revision, text: answer.text, scope: offeredScope, responses: responses))
        case "nothing_to_offer":
            return .nothingToOffer
        default:
            throw SuggestionResponseError.unreadable
        }
    }

    public func respond(
        commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind
    ) async throws -> SuggestionResponseReceipt {
        guard session.currentAuthorization() != nil else { throw SuggestionResponseError.notAuthenticated }
        guard !commandID.isEmpty, !itemID.isEmpty, expectedRevision >= 0 else {
            throw SuggestionResponseError.invalidRequest
        }
        let answer = try await ask(
            .respond(commandID: commandID, itemID: itemID, expectedRevision: expectedRevision, response: response))
        switch answer.outcome {
        case "applied":
            return SuggestionResponseReceipt(replayed: answer.replayed ?? false)
        case "stale_revision":
            guard let current = answer.currentRevision else { throw SuggestionResponseError.unreadable }
            throw SuggestionResponseError.staleRevision(currentRevision: current)
        case "no_longer_current":
            throw SuggestionResponseError.noLongerCurrent
        case "command_conflict":
            throw SuggestionResponseError.commandConflict
        default:
            throw SuggestionResponseError.unreadable
        }
    }

    private func ask(_ command: SuggestionResponseCommand) async throws -> SuggestionResponseAnswer {
        let result = await withCheckedContinuation {
            (continuation: CheckedContinuation<Result<SuggestionResponseAnswer, CoreFailure>, Never>) in
            core.submit(command) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let answer):
            return answer
        case .failure(let failure):
            switch failure.code {
            case "invalid_request": throw SuggestionResponseError.invalidRequest
            case "suggestion_not_authorized", "read_not_authenticated": throw SuggestionResponseError.notAuthenticated
            case "read_relocked": throw SuggestionResponseError.relockedBeforeDelivery
            default: throw SuggestionResponseError.core(failure)
            }
        }
    }
}
