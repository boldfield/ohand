import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers each command with the next scripted result and records what the service asked.
private final class ScriptedSuggestionCore: SuggestionResponseCore, @unchecked Sendable {
    private let lock = NSLock()
    private var script: [Result<SuggestionResponseAnswer, CoreFailure>]
    private var received: [SuggestionResponseCommand] = []

    init(_ script: [Result<SuggestionResponseAnswer, CoreFailure>]) {
        self.script = script
    }

    var commands: [SuggestionResponseCommand] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    func submit(
        _ command: SuggestionResponseCommand,
        completion: @escaping @Sendable (Result<SuggestionResponseAnswer, CoreFailure>) -> Void
    ) {
        lock.lock()
        received.append(command)
        let next = script.isEmpty ? nil : script.removeFirst()
        lock.unlock()
        completion(next ?? .failure(CoreSuggestionResponseClient.unreadableAnswer))
    }
}

final class SuggestionResponseServiceTests: XCTestCase {
    private let boundary = FakeReadAuthenticationBoundary()

    private func makeSession(authenticated: Bool) -> ReadAuthenticationSession {
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return session
    }

    private func offer(text: String? = "Water the plants") -> SuggestionResponseAnswer {
        SuggestionResponseAnswer(
            outcome: "offer", itemID: "item-1", revision: 4, text: text, scope: "personal",
            selectionReason: "never_offered", responses: ["not_now", "stop_suggesting", "done"])
    }

    func testActivationReturnsTheOfferedItemAndItsDisplayableText() async throws {
        let core = ScriptedSuggestionCore([.success(offer()), .success(offer(text: nil))])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))

        let first = try await service.activate(notificationID: "prompt:2026-10-08")
        let second = try await service.activate(notificationID: "prompt:2026-10-08", scope: .work)

        XCTAssertEqual(
            first,
            .offer(
                SuggestionOffer(
                    itemID: "item-1", revision: 4, text: "Water the plants", scope: .personal,
                    responses: [.notNow, .stopSuggesting, .done])))
        guard case .offer(let withoutText) = second else { return XCTFail("expected an offer") }
        XCTAssertNil(withoutText.text)
        XCTAssertEqual(
            core.commands,
            [
                .activatePrompt(notificationID: "prompt:2026-10-08", scope: nil),
                .activatePrompt(notificationID: "prompt:2026-10-08", scope: .work),
            ])
    }

    func testActivationReportsNothingToOfferHonestly() async throws {
        let core = ScriptedSuggestionCore([.success(SuggestionResponseAnswer(outcome: "nothing_to_offer"))])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))
        let activation = try await service.activate(notificationID: "prompt:2026-10-08")
        XCTAssertEqual(activation, .nothingToOffer)
    }

    func testEachResponseSendsItsCommandAndReturnsTheReceipt() async throws {
        let core = ScriptedSuggestionCore([
            .success(SuggestionResponseAnswer(outcome: "applied", replayed: false)),
            .success(SuggestionResponseAnswer(outcome: "applied", replayed: false)),
            .success(SuggestionResponseAnswer(outcome: "applied", replayed: true)),
        ])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))

        let notNow = try await service.respond(
            commandID: "c-1", itemID: "item-1", expectedRevision: 4, response: .notNow)
        let stop = try await service.respond(
            commandID: "c-2", itemID: "item-1", expectedRevision: 4, response: .stopSuggesting)
        let done = try await service.respond(
            commandID: "c-3", itemID: "item-1", expectedRevision: 4, response: .done)

        XCTAssertEqual(notNow, SuggestionResponseReceipt(replayed: false))
        XCTAssertEqual(stop, SuggestionResponseReceipt(replayed: false))
        XCTAssertEqual(done, SuggestionResponseReceipt(replayed: true))
        XCTAssertEqual(
            core.commands,
            [
                .respond(commandID: "c-1", itemID: "item-1", expectedRevision: 4, response: .notNow),
                .respond(commandID: "c-2", itemID: "item-1", expectedRevision: 4, response: .stopSuggesting),
                .respond(commandID: "c-3", itemID: "item-1", expectedRevision: 4, response: .done),
            ])
    }

    func testCaptureOnlyAndRelockedSessionsAreRefusedAndTheCoreIsNotAsked() async {
        let core = ScriptedSuggestionCore([.success(offer()), .success(SuggestionResponseAnswer(outcome: "applied"))])
        let capturing = SuggestionResponseService(core: core, session: makeSession(authenticated: false))
        let relockedSession = makeSession(authenticated: true)
        let relocked = SuggestionResponseService(core: core, session: relockedSession)
        relockedSession.relock(.explicit)

        for service in [capturing, relocked] {
            do {
                _ = try await service.activate(notificationID: "prompt:2026-10-08")
                XCTFail("a capture-only session must not activate")
            } catch {
                XCTAssertEqual(error as? SuggestionResponseError, .notAuthenticated)
            }
            do {
                _ = try await service.respond(commandID: "c", itemID: "item-1", expectedRevision: 0, response: .done)
                XCTFail("a capture-only session must not respond")
            } catch {
                XCTAssertEqual(error as? SuggestionResponseError, .notAuthenticated)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testMalformedRequestsNeverReachTheCore() async {
        let core = ScriptedSuggestionCore([])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))
        let attempts: [() async throws -> Void] = [
            { _ = try await service.activate(notificationID: "") },
            { _ = try await service.respond(commandID: "", itemID: "item-1", expectedRevision: 0, response: .done) },
            { _ = try await service.respond(commandID: "c", itemID: "", expectedRevision: 0, response: .done) },
            { _ = try await service.respond(commandID: "c", itemID: "item-1", expectedRevision: -1, response: .done) },
        ]
        for attempt in attempts {
            do {
                try await attempt()
                XCTFail("expected a refusal")
            } catch {
                XCTAssertEqual(error as? SuggestionResponseError, .invalidRequest)
            }
        }
        XCTAssertTrue(core.commands.isEmpty)
    }

    func testTheCoresRefusalsBecomeTypedErrors() async {
        let busy = CoreFailure(errorClass: .transient, code: "store_busy", message: "busy")
        let core = ScriptedSuggestionCore([
            .success(SuggestionResponseAnswer(outcome: "stale_revision", currentRevision: 7)),
            .success(SuggestionResponseAnswer(outcome: "no_longer_current")),
            .success(SuggestionResponseAnswer(outcome: "command_conflict")),
            .failure(CoreFailure(errorClass: .permanent, code: "invalid_request", message: "refused")),
            .failure(CoreFailure(errorClass: .unauthorized, code: "suggestion_not_authorized", message: "locked")),
            .failure(busy),
            .success(SuggestionResponseAnswer(outcome: "something_new")),
            .success(SuggestionResponseAnswer(outcome: "stale_revision")),
        ])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))
        var seen: [SuggestionResponseError] = []
        for index in 0..<8 {
            do {
                _ = try await service.respond(
                    commandID: "c-\(index)", itemID: "item-1", expectedRevision: 0, response: .notNow)
                XCTFail("expected a refusal")
            } catch {
                seen.append(error as! SuggestionResponseError)
            }
        }
        XCTAssertEqual(
            seen,
            [
                .staleRevision(currentRevision: 7), .noLongerCurrent, .commandConflict, .invalidRequest,
                .notAuthenticated, .core(busy), .unreadable, .unreadable,
            ])
    }

    func testActivationFailuresFromTheReadGateAreTyped() async {
        let relocked = CoreFailure(errorClass: .unauthorized, code: "read_relocked", message: "relocked")
        let core = ScriptedSuggestionCore([
            .failure(relocked),
            .success(SuggestionResponseAnswer(outcome: "offer", itemID: "item-1")),
        ])
        let service = SuggestionResponseService(core: core, session: makeSession(authenticated: true))
        do {
            _ = try await service.activate(notificationID: "prompt:2026-10-08")
            XCTFail("a relocked activation must not deliver")
        } catch {
            XCTAssertEqual(error as? SuggestionResponseError, .relockedBeforeDelivery)
        }
        do {
            _ = try await service.activate(notificationID: "prompt:2026-10-08")
            XCTFail("an incomplete offer must not be delivered")
        } catch {
            XCTAssertEqual(error as? SuggestionResponseError, .unreadable)
        }
    }
}
