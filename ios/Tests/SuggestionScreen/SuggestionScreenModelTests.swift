import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class ScriptedResponder: SuggestionResponding, @unchecked Sendable {
    private let lock = NSLock()
    private var activations: [Result<SuggestionActivation, Error>]
    private var responses: [Result<SuggestionResponseReceipt, Error>]
    private(set) var activatedPrompts: [String] = []
    private(set) var sentCommandIDs: [String] = []
    private(set) var sentRevisions: [Int] = []

    init(
        activations: [Result<SuggestionActivation, Error>] = [],
        responses: [Result<SuggestionResponseReceipt, Error>] = []
    ) {
        self.activations = activations
        self.responses = responses
    }

    func activate(notificationID: String, scope: SuggestionScope?) async throws -> SuggestionActivation {
        lock.lock(); defer { lock.unlock() }
        activatedPrompts.append(notificationID)
        guard !activations.isEmpty else { return .nothingToOffer }
        return try activations.removeFirst().get()
    }

    func respond(
        commandID: String, itemID: String, expectedRevision: Int, response: SuggestionResponseKind
    ) async throws -> SuggestionResponseReceipt {
        lock.lock(); defer { lock.unlock() }
        sentCommandIDs.append(commandID)
        sentRevisions.append(expectedRevision)
        guard !responses.isEmpty else { return SuggestionResponseReceipt(replayed: false) }
        return try responses.removeFirst().get()
    }
}

final class SuggestionScreenModelTests: XCTestCase {
    private static let promptID = "prompt:2026-10-08"

    private static func offer(
        itemID: String = "item-1", revision: Int = 3, text: String? = "synthetic action"
    ) -> SuggestionOffer {
        SuggestionOffer(
            itemID: itemID, revision: revision, text: text, scope: .personal,
            responses: [.notNow, .stopSuggesting, .done])
    }

    private func makeModel(_ responder: ScriptedResponder) -> SuggestionScreenModel {
        var counter = 0
        return SuggestionScreenModel(responder: responder, makeCommandID: {
            counter += 1
            return "cmd-\(counter)"
        })
    }

    @MainActor func testOpenShowsTheOfferedItem() async {
        let responder = ScriptedResponder(activations: [.success(.offer(Self.offer()))])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        XCTAssertEqual(model.state, .offer(Self.offer()))
        XCTAssertEqual(responder.activatedPrompts, [Self.promptID])
    }

    @MainActor func testAnEmptyEligibleSetIsCalmAndWithoutAPrompt() async {
        let responder = ScriptedResponder(activations: [.success(.nothingToOffer)])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        XCTAssertEqual(model.state, .nothingToOffer)
        XCTAssertNil(model.notice)

        await model.open(notificationID: nil)
        XCTAssertEqual(model.state, .nothingToOffer)
        XCTAssertEqual(responder.activatedPrompts, [Self.promptID], "no prompt means nothing is asked")
        for line in [SuggestionView.nothingToOfferLine] {
            XCTAssertFalse(line.contains(where: \.isNumber), "no count: \(line)")
            XCTAssertFalse(line.lowercased().contains("inbox"))
        }
    }

    @MainActor func testACaptureOnlySessionShowsNothingPrivate() async {
        let responder = ScriptedResponder(activations: [.failure(SuggestionResponseError.notAuthenticated)])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        XCTAssertEqual(model.state, .locked)
    }

    @MainActor func testLockDropsTheOfferAndBlocksResponses() async {
        let responder = ScriptedResponder(activations: [.success(.offer(Self.offer()))])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        model.lock()
        XCTAssertEqual(model.state, .locked)
        await model.respond(.done)
        XCTAssertTrue(responder.sentCommandIDs.isEmpty)
    }

    @MainActor func testEachResponseStatesTheShownRevisionAndEndsCalmly() async {
        for response in SuggestionResponseKind.allCases {
            let responder = ScriptedResponder(activations: [.success(.offer(Self.offer(revision: 7)))])
            let model = makeModel(responder)
            await model.open(notificationID: Self.promptID)
            await model.respond(response)
            XCTAssertEqual(responder.sentRevisions, [7])
            XCTAssertEqual(model.state, .responded(response))
            XCTAssertEqual(responder.activatedPrompts.count, 1, "no further item is pulled in")
        }
    }

    @MainActor func testARetryAfterAFailureReusesTheCommandID() async {
        let responder = ScriptedResponder(
            activations: [.success(.offer(Self.offer()))],
            responses: [.failure(SuggestionResponseError.unreadable), .success(SuggestionResponseReceipt(replayed: false))])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        await model.respond(.done)
        XCTAssertEqual(model.notice, .notSaved)
        XCTAssertEqual(model.state, .offer(Self.offer()), "the offer stays so the answer can be retried")
        await model.respond(.done)
        XCTAssertEqual(responder.sentCommandIDs, ["cmd-1", "cmd-1"])
        XCTAssertEqual(model.state, .responded(.done))
    }

    @MainActor func testAStaleRevisionIsResolvedAgainWithTheCurrentOffer() async {
        let responder = ScriptedResponder(
            activations: [.success(.offer(Self.offer(revision: 3))), .success(.offer(Self.offer(revision: 4)))],
            responses: [.failure(SuggestionResponseError.staleRevision(currentRevision: 4))])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        await model.respond(.notNow)
        XCTAssertEqual(model.state, .offer(Self.offer(revision: 4)))
        XCTAssertEqual(model.notice, .changed)
        await model.respond(.notNow)
        XCTAssertEqual(responder.sentRevisions, [3, 4])
        XCTAssertEqual(responder.sentCommandIDs, ["cmd-1", "cmd-2"], "a new revision is a new command")
    }

    @MainActor func testACompletedOrDeletedItemResolvesToWhatIsEligibleNow() async {
        let responder = ScriptedResponder(
            activations: [.success(.offer(Self.offer())), .success(.nothingToOffer)],
            responses: [.failure(SuggestionResponseError.noLongerCurrent)])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        await model.respond(.done)
        XCTAssertEqual(model.state, .nothingToOffer)
        XCTAssertEqual(model.notice, .changed)
    }

    @MainActor func testARelockDuringAResponseLocksTheScreen() async {
        let responder = ScriptedResponder(
            activations: [.success(.offer(Self.offer()))],
            responses: [.failure(SuggestionResponseError.relockedBeforeDelivery)])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        await model.respond(.stopSuggesting)
        XCTAssertEqual(model.state, .locked)
    }

    @MainActor func testACoreFailureWhileOpeningChangesNothing() async {
        let responder = ScriptedResponder(activations: [.failure(SuggestionResponseError.unreadable)])
        let model = makeModel(responder)
        await model.open(notificationID: Self.promptID)
        XCTAssertEqual(model.state, .unavailable)
        XCTAssertTrue(responder.sentCommandIDs.isEmpty)
    }
}
