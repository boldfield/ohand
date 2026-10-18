import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

private final class FakeDetailReader: SourceDetailReading, @unchecked Sendable {
    var result: Result<CoreItemDetail, Error>
    private(set) var reads = 0

    init(_ result: Result<CoreItemDetail, Error>) { self.result = result }

    func detail(itemID: String) async throws -> CoreItemDetail {
        reads += 1
        return try result.get()
    }
}

private final class FakeAudioSession: SourceAudioSession {
    var endReason: PlaybackEndReason?
    var acceptsPlay = true
    private(set) var plays = 0
    private(set) var pauses = 0
    private(set) var closes = 0

    func play() -> Bool {
        plays += 1
        return endReason == nil && acceptsPlay
    }

    func pause() { pauses += 1 }

    func close() {
        closes += 1
        if endReason == nil { endReason = .closed }
    }
}

private final class FakeAudioOpener: SourceAudioOpening, @unchecked Sendable {
    var result: Result<FakeAudioSession, Error>
    private(set) var opens = 0

    init(_ result: Result<FakeAudioSession, Error> = .success(FakeAudioSession())) { self.result = result }

    func openAudio(itemID: String) async throws -> SourceAudioSession {
        opens += 1
        return try result.get()
    }
}

@MainActor
final class SourceDetailModelTests: XCTestCase {
    private static let itemID = "item-synthetic-1"

    private func detail(
        text: String? = "the corrected words", basis: CoreRetrievalTextBasis = .corrected,
        original: String? = "the original words", corrected: String? = "the corrected words",
        audioReference: String? = nil, transcript: CoreItemDetailTranscript? = nil, lifecycle: String = "active"
    ) -> CoreItemDetail {
        CoreItemDetail(
            itemID: Self.itemID, captureID: "capture-1", revision: 2, itemType: "task", scope: "personal",
            sessionTopic: "kitchen", lifecycleState: lifecycle, capturedAt: "2026-10-01T09:30:00Z", text: text,
            textBasis: basis, originalText: original, correctedText: corrected, audioReference: audioReference,
            transcript: transcript,
            history: [CoreItemDetailEvent(
                eventID: "event-1", revision: 2, eventType: "correction", happenedAt: "2026-10-02T09:30:00Z",
                correction: CoreItemDetailCorrection(kind: "text", oldValue: "the original words", newValue: "the corrected words"))],
            historyTruncated: false)
    }

    private func make(
        _ reader: FakeDetailReader, _ opener: FakeAudioOpener = FakeAudioOpener()
    ) -> SourceDetailModel {
        SourceDetailModel(itemID: Self.itemID, reader: reader, opener: opener)
    }

    func testOriginalAndCorrectedTextAreShownSeparatelyAndOnlyFromTheStoredAnswer() async throws {
        let model = make(FakeDetailReader(.success(detail())))

        await model.load()

        let content = try XCTUnwrap(model.content)
        XCTAssertEqual(content.sections.map(\.kind), [.original, .correction])
        XCTAssertEqual(content.sections.map(\.text), ["the original words", "the corrected words"])
        XCTAssertEqual(content.currentText, "the corrected words")
        XCTAssertEqual(content.typeLabel, "Task")
        XCTAssertEqual(content.scopeName, "Personal")
        XCTAssertEqual(content.sessionTopic, "kitchen")
        XCTAssertEqual(content.history.map(\.summary), ["You corrected the text"])
        XCTAssertNotNil(content.capturedAt)
        XCTAssertNil(model.failure)
    }

    func testAnUncorrectedItemHasNoCorrectionSection() async throws {
        let model = make(FakeDetailReader(.success(detail(
            text: "the original words", basis: .original, corrected: nil))))

        await model.load()

        XCTAssertEqual(try XCTUnwrap(model.content).sections.map(\.kind), [.original])
    }

    func testATranscriptIsLabelledAsMachineTextWithItsProvenanceAndNeverAsTheUsersOwn() async throws {
        let transcript = CoreItemDetailTranscript(
            transcriptRecordID: "t1", sourceJobID: "j1", text: "machine words", recognizer: "on-device",
            detectedLanguage: "en", confidence: 0.8, transcribedAt: "2026-10-01T09:31:00Z", attachedRevision: 1,
            audioSHA256: String(repeating: "a", count: 64))
        let model = make(FakeDetailReader(.success(detail(
            text: "machine words", basis: .transcribed, original: nil, corrected: nil,
            audioReference: "audio:1", transcript: transcript))))

        await model.load()

        let section = try XCTUnwrap(model.content?.sections.first)
        XCTAssertEqual(model.content?.sections.count, 1)
        XCTAssertEqual(section.kind, .transcript)
        XCTAssertEqual(section.text, "machine words")
        XCTAssertEqual(section.provenance, "Transcribed on this device by on-device, language en, confidence 80%")
    }

    func testAnAudioCaptureWithNoTranscriptYetShowsNoInventedText() async throws {
        let model = make(FakeDetailReader(.success(detail(
            text: nil, basis: .transcribed, original: nil, corrected: nil, audioReference: "audio:1"))))

        await model.load()

        XCTAssertEqual(model.content?.sections, [])
        XCTAssertNil(model.content?.currentText)
        XCTAssertEqual(model.audio, .available)
    }

    func testALockedReadShowsNoTextAndDoesNotOfferPlayback() async {
        for error in [ItemDetailError.notAuthenticated, .relocked] {
            let model = make(FakeDetailReader(.failure(error)))
            await model.load()
            XCTAssertNil(model.content)
            XCTAssertEqual(model.failure, .locked)
            XCTAssertEqual(model.audio, .none)
        }
    }

    func testAMissingOrUnreadableItemIsToldApartFromALockedOne() async {
        let missing = make(FakeDetailReader(.failure(ItemDetailError.notFound)))
        await missing.load()
        XCTAssertEqual(missing.failure, .notFound)
        let broken = make(FakeDetailReader(.failure(ItemDetailError.core(
            CoreFailure(errorClass: .permanent, code: "oops", message: "fixed")))))
        await broken.load()
        XCTAssertEqual(broken.failure, .unreadable)
    }

    func testARelockedReloadDropsTheTextAndEndsOpenPlayback() async throws {
        let reader = FakeDetailReader(.success(detail(audioReference: "audio:1")))
        let session = FakeAudioSession()
        let model = make(reader, FakeAudioOpener(.success(session)))
        await model.load()
        await model.play()
        XCTAssertEqual(model.audio, .playing)

        reader.result = .failure(ItemDetailError.relocked)
        await model.load()

        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .locked)
        XCTAssertEqual(session.closes, 1)
        XCTAssertEqual(model.audio, .none)
    }

    func testRetainedAudioPlaysPausesResumesAndIsReleasedWhenTheScreenGoes() async {
        let session = FakeAudioSession()
        let opener = FakeAudioOpener(.success(session))
        let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), opener)
        await model.load()
        XCTAssertTrue(model.canPlay)

        await model.play()
        XCTAssertEqual(model.audio, .playing)
        model.pause()
        XCTAssertEqual(model.audio, .paused)
        await model.play()
        XCTAssertEqual(model.audio, .playing)
        model.close()

        XCTAssertEqual(opener.opens, 1, "resuming reuses the open audio")
        XCTAssertEqual(session.plays, 2)
        XCTAssertEqual(session.closes, 1)
        XCTAssertEqual(model.audio, .available)
    }

    func testAnItemWithoutAnAudioReferenceOffersNoPlaybackAndOpensNothing() async {
        let opener = FakeAudioOpener()
        let model = make(FakeDetailReader(.success(detail())), opener)
        await model.load()

        await model.play()

        XCTAssertEqual(model.audio, .none)
        XCTAssertFalse(model.canPlay)
        XCTAssertEqual(opener.opens, 0)
    }

    func testADeletedItemShowsRemovedAudioAndNeverOpensIt() async {
        let opener = FakeAudioOpener()
        let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1", lifecycle: "deleted"))), opener)
        await model.load()

        await model.play()

        XCTAssertEqual(model.audio, .removed)
        XCTAssertFalse(model.canPlay)
        XCTAssertEqual(opener.opens, 0)
    }

    func testRemovedExpiredOrMissingAudioDisablesPlaybackWithTheReason() async {
        let cases: [(PlaybackError, SourceAudioState)] = [
            (.noAudio, .removed), (.itemDeleted, .removed), (.itemNotFound, .removed),
            (.audioMissing, .unplayable), (.audioUnreadable, .unplayable), (.audioReferenceUnresolvable, .unplayable),
            (.notAuthenticated, .locked), (.shutDown, .locked),
        ]
        for (error, expected) in cases {
            let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), FakeAudioOpener(.failure(error)))
            await model.load()
            await model.play()
            XCTAssertEqual(model.audio, expected, "\(error)")
            XCTAssertFalse(model.canPlay, "\(error)")
            XCTAssertFalse(model.canPause, "\(error)")
            XCTAssertNotNil(SourceDetailView.audioNotice(for: model.audio), "\(error)")
        }
    }

    func testALockedPlaybackRefusalAlsoClearsTheText() async {
        let model = make(
            FakeDetailReader(.success(detail(audioReference: "audio:1"))), FakeAudioOpener(.failure(PlaybackError.notAuthenticated)))
        await model.load()
        await model.play()
        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .locked)
    }

    func testAudioRemovedByRetentionAfterTheFirstReadTurnsOffOnReload() async {
        let reader = FakeDetailReader(.success(detail(audioReference: "audio:1")))
        let session = FakeAudioSession()
        let model = make(reader, FakeAudioOpener(.success(session)))
        await model.load()
        await model.play()

        reader.result = .success(detail(audioReference: nil))
        await model.load()

        XCTAssertEqual(model.audio, .removed)
        XCTAssertEqual(session.closes, 1)
        XCTAssertFalse(model.canPlay)
    }

    func testPlaybackEndedByDeletionOrRelockIsNoticedAndTheControlTurnsOff() async {
        let deleted = FakeAudioSession()
        let deletedModel = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), FakeAudioOpener(.success(deleted)))
        await deletedModel.load()
        await deletedModel.play()
        deleted.endReason = .deletionRequested
        deletedModel.checkSession()
        XCTAssertEqual(deletedModel.audio, .removed)

        let relocked = FakeAudioSession()
        let relockedModel = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), FakeAudioOpener(.success(relocked)))
        await relockedModel.load()
        await relockedModel.play()
        relocked.endReason = .relocked
        relockedModel.checkSession()
        XCTAssertEqual(relockedModel.audio, .locked)
        XCTAssertNil(relockedModel.content, "no stale text after a relock")
    }

    func testAPlayerThatRefusesToStartTurnsTheControlOffInsteadOfSpinning() async {
        let session = FakeAudioSession()
        session.acceptsPlay = false
        let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), FakeAudioOpener(.success(session)))
        await model.load()

        await model.play()

        XCTAssertEqual(model.audio, .unplayable)
        XCTAssertEqual(session.closes, 1)
    }

    func testALockedReadScopeDropsTextEvenWhenNothingIsPlaying() async {
        var scope = CompositionReadScope.authenticated
        let model = SourceDetailModel(
            itemID: Self.itemID, reader: FakeDetailReader(.success(detail())), opener: FakeAudioOpener(),
            readScope: { scope })
        await model.load()
        XCTAssertNotNil(model.content)

        scope = .captureOnly
        model.checkSession()

        XCTAssertNil(model.content)
        XCTAssertEqual(model.failure, .locked)
    }

    func testAClipThatFinishedTurnsBackIntoAPlayableControlAndReplaysThroughAFreshOpen() async {
        let first = FakeAudioSession()
        let opener = FakeAudioOpener(.success(first))
        let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), opener)
        await model.load()
        await model.play()
        XCTAssertEqual(model.audio, .playing)

        first.endReason = .finished
        model.checkSession()
        XCTAssertEqual(model.audio, .available)

        let second = FakeAudioSession()
        opener.result = .success(second)
        await model.play()
        XCTAssertEqual(opener.opens, 2)
        XCTAssertEqual(second.plays, 1)
        XCTAssertEqual(model.audio, .playing)
    }

    func testPressingPlayAfterAPausedSessionWasClosedStartsPlaybackInTheSamePress() async {
        let first = FakeAudioSession()
        let opener = FakeAudioOpener(.success(first))
        let model = make(FakeDetailReader(.success(detail(audioReference: "audio:1"))), opener)
        await model.load()
        await model.play()
        model.pause()
        first.endReason = .closed
        let second = FakeAudioSession()
        opener.result = .success(second)

        await model.play()

        XCTAssertEqual(model.audio, .playing)
        XCTAssertEqual(second.plays, 1)
    }
}
