import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

private final class StubDeletionCore: ItemDeletionCore, @unchecked Sendable {
    private let markAnswer: Result<ItemDeletionAnswer, CoreFailure>

    init(_ markAnswer: Result<ItemDeletionAnswer, CoreFailure>) { self.markAnswer = markAnswer }

    func submit(
        _ command: ItemDeletionCommand,
        completion: @escaping @Sendable (Result<ItemDeletionAnswer, CoreFailure>) -> Void
    ) {
        switch command {
        case .markDeleted: completion(markAnswer)
        default:
            completion(.success(ItemDeletionAnswer(
                outcome: "progress", itemID: "item-1", progress: ItemDeletionProgressPayload(state: "complete"))))
        }
    }
}

/// Reports what playback looked like at the moment cleanup was asked to run.
private final class PlaybackObservingCleanup: ItemDeletionCleanup, @unchecked Sendable {
    private let lock = NSLock()
    private let opened: PlaybackSession
    private var endReasonWhenAsked: PlaybackEndReason?

    init(opened: PlaybackSession) { self.opened = opened }

    var endReasonAtCleanup: PlaybackEndReason? {
        lock.lock()
        defer { lock.unlock() }
        return endReasonWhenAsked
    }

    func cleanUp(itemID: String) async -> DeletionItemResult {
        lock.lock()
        endReasonWhenAsked = opened.endReason
        lock.unlock()
        return DeletionItemResult(itemID: itemID, listingFailure: nil, steps: [])
    }
}

private final class SilentPlayer: PlaybackPlayer, @unchecked Sendable {
    func play() -> Bool { true }
    func pause() {}
    func stop() {}
}

/// Playback against the production `ItemDeletionService`: only a recorded deletion ends it, and it ends before cleanup.
final class PlaybackDeletionSeamTests: XCTestCase {
    private var storage: RetentionStorage!
    private var session: ReadAuthenticationSession!
    private let fence = PlaybackFence()

    override func setUpWithError() throws {
        try super.setUpWithError()
        storage = try RetentionStorage()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
    }

    override func tearDown() {
        storage.tearDown()
        super.tearDown()
    }

    private func openPlayback() async throws -> (PlaybackService, PlaybackSession) {
        try storage.writeAudio("cap1.m4a")
        let core = ScriptedAudioReadCore()
        core.script(.success(RetentionAudioAnswer(
            itemID: "item-1", audioReference: RetentionFixtures.audioReference("cap1.m4a"), soleSource: false)))
        let playback = PlaybackService(
            core: core, session: session, layout: storage.layout, fence: fence, playerFactory: { _ in SilentPlayer() })
        return (playback, try await playback.open(itemID: "item-1"))
    }

    private func recorded(replayed: Bool) -> ItemDeletionAnswer {
        ItemDeletionAnswer(
            outcome: "deletion_recorded", itemID: "item-1", deletionRevision: 2, replayed: replayed,
            progress: ItemDeletionProgressPayload(state: "in_progress", pending: ["remove_audio"]))
    }

    private func makeDeletion(
        _ answer: Result<ItemDeletionAnswer, CoreFailure>, playback: PlaybackService
    ) -> ItemDeletionService {
        let deletion = ItemDeletionService(core: StubDeletionCore(answer), session: session)
        deletion.setIntentObserver { itemID in playback.endPlayback(itemID: itemID) }
        return deletion
    }

    func testARecordedDeletionEndsPlaybackBeforeCleanupRuns() async throws {
        let (playback, opened) = try await openPlayback()
        let deletion = makeDeletion(.success(recorded(replayed: false)), playback: playback)
        let cleanup = PlaybackObservingCleanup(opened: opened)
        deletion.setCleanupProvider { cleanup }

        _ = try await deletion.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(cleanup.endReasonAtCleanup, .deletionRequested)
        XCTAssertFalse(fence.isHeld(storage.audioURL("cap1.m4a")))
    }

    func testAReplayedDeletionAlsoEndsPlayback() async throws {
        let (playback, opened) = try await openPlayback()
        let deletion = makeDeletion(.success(recorded(replayed: true)), playback: playback)

        _ = try await deletion.delete(itemID: "item-1", expectedRevision: 1)

        XCTAssertEqual(opened.endReason, .deletionRequested)
    }

    func testARefusedDeletionLeavesPlaybackOpen() async throws {
        let (playback, opened) = try await openPlayback()
        let stale = CoreFailure(errorClass: .permanent, code: "stale_revision", message: "stale")
        let deletion = makeDeletion(.failure(stale), playback: playback)

        do {
            _ = try await deletion.delete(itemID: "item-1", expectedRevision: 0)
            XCTFail("expected the refused deletion to throw")
        } catch {}

        XCTAssertTrue(opened.isOpen)
        XCTAssertTrue(fence.isHeld(storage.audioURL("cap1.m4a")))
        opened.close()
    }
}
