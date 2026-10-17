import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// A responder whose activations and responses finish only when the test releases them.
private final class GatedResponder: SuggestionResponding, @unchecked Sendable {
    private let lock = NSLock()
    private var activationGates: [String: [CheckedContinuation<SuggestionActivation, Error>]] = [:]
    private var responseGate: CheckedContinuation<SuggestionResponseReceipt, Error>?
    private(set) var activatedPrompts: [String] = []

    func activate(notificationID: String, scope: SuggestionScope?) async throws -> SuggestionActivation {
        try await withCheckedThrowingContinuation { continuation in
            lock.lock(); defer { lock.unlock() }
            activatedPrompts.append(notificationID)
            activationGates[notificationID, default: []].append(continuation)
        }
    }

    func respond(
        commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind
    ) async throws -> SuggestionResponseReceipt {
        try await withCheckedThrowingContinuation { continuation in
            lock.lock(); defer { lock.unlock() }
            responseGate = continuation
        }
    }

    var hasPendingResponse: Bool { lock.lock(); defer { lock.unlock() }; return responseGate != nil }

    func pendingActivations(for prompt: String) -> Int {
        lock.lock(); defer { lock.unlock() }
        return activationGates[prompt]?.count ?? 0
    }

    func finishActivation(prompt: String, with activation: SuggestionActivation) {
        lock.lock()
        let continuation = activationGates[prompt]?.removeFirst()
        lock.unlock()
        continuation?.resume(returning: activation)
    }

    func finishResponse(with result: Result<SuggestionResponseReceipt, Error>) {
        lock.lock()
        let continuation = responseGate
        responseGate = nil
        lock.unlock()
        continuation?.resume(with: result)
    }
}

final class SuggestionScreenInterleavingTests: XCTestCase {
    private func offer(_ itemID: String) -> SuggestionOffer {
        SuggestionOffer(
            itemID: itemID, revision: 1, text: "text \(itemID)", scope: .personal,
            responses: [.notNow, .stopSuggesting, .done])
    }

    private func waitUntil(_ condition: @escaping () -> Bool) async {
        let deadline = Date(timeIntervalSinceNow: 10)
        while !condition() && Date() < deadline { try? await Task.sleep(nanoseconds: 5_000_000) }
        XCTAssertTrue(condition())
    }

    @MainActor func testAPromptTappedDuringAResponseIsResolvedAfterItNotDropped() async {
        let responder = GatedResponder()
        let model = SuggestionScreenModel(responder: responder)
        let first = Task { @MainActor in await model.open(notificationID: "prompt:one") }
        await waitUntil { responder.pendingActivations(for: "prompt:one") == 1 }
        responder.finishActivation(prompt: "prompt:one", with: .offer(offer("a")))
        await first.value

        let responding = Task { @MainActor in await model.respond(.done) }
        await waitUntil { responder.hasPendingResponse }
        await model.open(notificationID: "prompt:two")
        XCTAssertTrue(responder.activatedPrompts == ["prompt:one"], "the new prompt waits for the response")

        let finishing = Task { @MainActor in
            responder.finishResponse(with: .success(SuggestionResponseReceipt(replayed: false)))
            await responding.value
        }
        await waitUntil { responder.pendingActivations(for: "prompt:two") == 1 }
        responder.finishActivation(prompt: "prompt:two", with: .offer(offer("b")))
        await finishing.value
        XCTAssertEqual(model.state, .offer(offer("b")), "the newest prompt is shown, not the old answer")
    }

    @MainActor func testAPromptTappedDuringAnActivationIsResolvedNextAndTheOlderResultIsNeverShown() async {
        let responder = GatedResponder()
        let model = SuggestionScreenModel(responder: responder)
        let older = Task { @MainActor in await model.open(notificationID: "prompt:one") }
        await waitUntil { responder.pendingActivations(for: "prompt:one") == 1 }
        await model.open(notificationID: "prompt:two")
        XCTAssertEqual(responder.activatedPrompts, ["prompt:one"], "one call to the core at a time")

        responder.finishActivation(prompt: "prompt:one", with: .offer(offer("old")))
        await waitUntil { responder.pendingActivations(for: "prompt:two") == 1 }
        XCTAssertEqual(model.state, .loading, "the superseded result is dropped, not shown")
        responder.finishActivation(prompt: "prompt:two", with: .offer(offer("new")))
        await older.value
        XCTAssertEqual(model.state, .offer(offer("new")))
        XCTAssertFalse(model.isBusy)
    }

    @MainActor func testAThirdPromptTappedWhileTheQueuedPromptActivatesAfterAResponseIsNotDropped() async {
        let responder = GatedResponder()
        let model = SuggestionScreenModel(responder: responder)
        let first = Task { @MainActor in await model.open(notificationID: "prompt:a") }
        await waitUntil { responder.pendingActivations(for: "prompt:a") == 1 }
        responder.finishActivation(prompt: "prompt:a", with: .offer(offer("a")))
        await first.value

        let responding = Task { @MainActor in await model.respond(.done) }
        await waitUntil { responder.hasPendingResponse }
        await model.open(notificationID: "prompt:b")
        responder.finishResponse(with: .success(SuggestionResponseReceipt(replayed: false)))
        await waitUntil { responder.pendingActivations(for: "prompt:b") == 1 }

        await model.open(notificationID: "prompt:c")
        responder.finishActivation(prompt: "prompt:b", with: .offer(offer("b")))
        await waitUntil { responder.pendingActivations(for: "prompt:c") == 1 }
        XCTAssertNotEqual(model.state, .offer(offer("b")), "prompt B was superseded before its result arrived")
        responder.finishActivation(prompt: "prompt:c", with: .offer(offer("c")))
        await responding.value
        XCTAssertEqual(responder.activatedPrompts, ["prompt:a", "prompt:b", "prompt:c"])
        XCTAssertEqual(model.state, .offer(offer("c")), "the newest prompt is shown")
        XCTAssertFalse(model.isBusy)
    }

    @MainActor func testARelockWhileAQueuedPromptActivatesKeepsTheScreenLocked() async {
        let responder = GatedResponder()
        let model = SuggestionScreenModel(responder: responder)
        let opening = Task { @MainActor in await model.open(notificationID: "prompt:one") }
        await waitUntil { responder.pendingActivations(for: "prompt:one") == 1 }
        await model.open(notificationID: "prompt:two")
        model.lock()
        responder.finishActivation(prompt: "prompt:one", with: .offer(offer("one")))
        await opening.value
        XCTAssertEqual(model.state, .locked)
        XCTAssertEqual(responder.activatedPrompts, ["prompt:one"], "nothing more is requested after the relock")
    }
}
