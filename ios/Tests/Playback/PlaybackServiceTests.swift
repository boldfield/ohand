import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

final class ScriptedAudioReadCore: RetentionControlCore, @unchecked Sendable {
    private let lock = NSLock()
    private var answer: Result<RetentionAudioAnswer, CoreFailure> = .success(
        RetentionAudioAnswer(itemID: "item-1", audioReference: nil, soleSource: false))
    private var onRead: (() -> Void)?
    private(set) var reads = 0

    func script(_ answer: Result<RetentionAudioAnswer, CoreFailure>) { locked { self.answer = answer } }
    func script(onRead: (() -> Void)?) { locked { self.onRead = onRead } }

    func readStatus(completion: @escaping @Sendable (Result<RetentionStatusAnswer, CoreFailure>) -> Void) {
        completion(.failure(CoreFailure(errorClass: .permanent, code: "unused", message: "unused")))
    }

    func readAudio(
        itemID: String, completion: @escaping @Sendable (Result<RetentionAudioAnswer, CoreFailure>) -> Void
    ) {
        let (result, hook) = locked { () -> (Result<RetentionAudioAnswer, CoreFailure>, (() -> Void)?) in
            reads += 1
            return (answer, onRead)
        }
        hook?()
        completion(result)
    }

    func submit(
        _ command: RetentionControlCommand,
        completion: @escaping @Sendable (Result<RetentionControlAnswer, CoreFailure>) -> Void
    ) {
        completion(.failure(CoreFailure(errorClass: .permanent, code: "unused", message: "unused")))
    }

    private func locked<Value>(_ body: () -> Value) -> Value {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }
}

private final class RecordingPlayer: PlaybackPlayer, @unchecked Sendable {
    private let lock = NSLock()
    private(set) var playCalls = 0
    private(set) var stopCalls = 0
    private(set) var pauseCalls = 0

    func play() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        playCalls += 1
        return true
    }

    func pause() {
        lock.lock()
        pauseCalls += 1
        lock.unlock()
    }

    func stop() {
        lock.lock()
        stopCalls += 1
        lock.unlock()
    }
}

private struct UnplayableAudio: Error {}

/// Lets a player factory reach the service that calls it.
private final class ServiceHolder: @unchecked Sendable {
    var service: PlaybackService?
}

/// Playback of retained audio against real files in a temporary protected-storage tree: what `open` returns, and that an
/// open session holds the file against the retention sweep and deletion cleanup and is ended by deletion and relock.
final class PlaybackServiceTests: XCTestCase {
    private var storage: RetentionStorage!
    private var session: ReadAuthenticationSession!
    private let core = ScriptedAudioReadCore()
    private let fence = PlaybackFence()
    private let player = RecordingPlayer()
    private var unplayable = false

    override func setUpWithError() throws {
        try super.setUpWithError()
        storage = try RetentionStorage()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    override func tearDown() {
        storage.tearDown()
        super.tearDown()
    }

    private func makeService(fileSystem: AudioRetentionFileSystem = FileManagerAudioRetentionFileSystem())
        -> PlaybackService
    {
        PlaybackService(
            core: core, session: session, layout: storage.layout, fence: fence, fileSystem: fileSystem,
            playerFactory: { [unowned self] _ in
                if unplayable { throw UnplayableAudio() }
                return player
            })
    }

    private func retain(audio fileName: String, itemID: String = "item-1") throws {
        try storage.writeAudio(fileName)
        core.script(.success(RetentionAudioAnswer(
            itemID: itemID, audioReference: RetentionFixtures.audioReference(fileName), soleSource: false)))
    }

    private func assertOpenFails(
        _ expected: PlaybackError, _ service: PlaybackService, file: StaticString = #filePath, line: UInt = #line
    ) async {
        do {
            _ = try await service.open(itemID: "item-1")
            XCTFail("expected \(expected)", file: file, line: line)
        } catch {
            XCTAssertEqual(error as? PlaybackError, expected, file: file, line: line)
        }
        XCTAssertFalse(fence.isHeld(storage.audioURL("cap1.m4a")), "a failed open holds nothing", file: file, line: line)
    }

    // MARK: Opening

    func testRetainedAudioOpensAsAPlayableHandle() async throws {
        try retain(audio: "cap1.m4a")
        let opened = try await makeService().open(itemID: "item-1")

        XCTAssertTrue(opened.isOpen)
        XCTAssertTrue(opened.play())
        XCTAssertEqual(player.playCalls, 1)
        XCTAssertTrue(fence.isHeld(storage.audioURL("cap1.m4a")))

        opened.close()
        XCTAssertEqual(opened.endReason, .closed)
        XCTAssertEqual(player.stopCalls, 1)
        XCTAssertFalse(opened.play())
        XCTAssertFalse(fence.isHeld(storage.audioURL("cap1.m4a")))
    }

    func testAnItemWithoutRetainedAudioIsReportedHonestly() async {
        await assertOpenFails(.noAudio, makeService())
    }

    func testACaptureOnlySessionNeverReachesTheCore() async throws {
        try retain(audio: "cap1.m4a")
        session.relock(.explicit)
        await assertOpenFails(.notAuthenticated, makeService())
        XCTAssertEqual(core.reads, 0)
    }

    func testAnUnresolvableReferenceIsRefused() async {
        core.script(.success(RetentionAudioAnswer(itemID: "item-1", audioReference: "../escape.m4a", soleSource: false)))
        await assertOpenFails(.audioReferenceUnresolvable, makeService())
    }

    func testAnUnknownItemIsRefused() async {
        core.script(.failure(CoreFailure(errorClass: .permanent, code: "not_found", message: "no such item")))
        await assertOpenFails(.itemNotFound, makeService())
    }

    func testMissingAudioIsReportedAsMissing() async {
        core.script(.success(RetentionAudioAnswer(
            itemID: "item-1", audioReference: RetentionFixtures.audioReference("cap1.m4a"), soleSource: false)))
        await assertOpenFails(.audioMissing, makeService())
    }

    func testUnreadableAudioIsReportedAndReleasesItsHold() async throws {
        try retain(audio: "cap1.m4a")
        unplayable = true
        await assertOpenFails(.audioUnreadable, makeService())
    }

    func testAFileWhosePresenceCannotBeDeterminedIsUnreadableNotMissing() async throws {
        try retain(audio: "cap1.m4a")
        let fileSystem = FaultingRetentionFileSystem(faults: FaultPlan())
        fileSystem.failPresenceLookups(of: "cap1.m4a")
        await assertOpenFails(.audioUnreadable, makeService(fileSystem: fileSystem))
    }

    func testARelockWhileTheAudioIsBeingReadRefusesTheOpen() async throws {
        try retain(audio: "cap1.m4a")
        core.script(onRead: { [session] in session?.relock(.explicit) })
        await assertOpenFails(.notAuthenticated, makeService())
    }

    func testARelockAndReauthenticationWhileThePlayerIsBuiltRefusesTheOpen() async throws {
        try retain(audio: "cap1.m4a")
        let service = PlaybackService(
            core: core, session: session, layout: storage.layout, fence: fence,
            playerFactory: { [session, player] _ in
                // After admission, before the session exists to be ended: relock, then authenticate again.
                session?.relock(.explicit)
                session?.authenticate(reason: "test") { _ in }
                return player
            })
        XCTAssertEqual(session.scope, .authenticated)
        await assertOpenFails(.notAuthenticated, service)
        XCTAssertEqual(player.stopCalls, 1, "the player built under the revoked epoch is stopped")
    }

    // MARK: Fence against the sweep

    private func makeRetention(core retentionCore: ScriptedRetentionCore) -> AudioRetentionService {
        AudioRetentionService(
            core: retentionCore, layout: storage.layout, pressureProbe: FixedPressureProbe(pressure: .normal),
            playbackFence: fence)
    }

    func testTheRetentionSweepLeavesOpenAudioAloneAndRemovesItAfterwards() async throws {
        try retain(audio: "cap1.m4a")
        let retentionCore = ScriptedRetentionCore()
        retentionCore.setListing([RetentionFixtures.item("item-1", audio: "cap1.m4a")])
        let retention = makeRetention(core: retentionCore)
        let opened = try await makeService().open(itemID: "item-1")

        let during = await retention.sweep()
        XCTAssertEqual(during.items.first?.removal, .retryLater(.audioInPlayback))
        XCTAssertTrue(storage.audioExists("cap1.m4a"))
        XCTAssertTrue(retentionCore.removalRequests.isEmpty, "the core is not asked to approve a removal that cannot run")
        let removalFailureCountIsUnchanged = await retention.sweep().items.first?.removal
        XCTAssertEqual(removalFailureCountIsUnchanged, .retryLater(.audioInPlayback), "playback never exhausts attempts")

        opened.close()
        let after = await retention.sweep()
        XCTAssertEqual(after.items.first?.removal, .removed)
        XCTAssertFalse(storage.audioExists("cap1.m4a"))
    }

    func testAUserRemovalOfOpenAudioIsLeftForLater() async throws {
        try retain(audio: "cap1.m4a")
        let retention = makeRetention(core: ScriptedRetentionCore())
        let opened = try await makeService().open(itemID: "item-1")

        let outcome = await retention.removeAudioForUser(
            itemID: "item-1", audioReference: RetentionFixtures.audioReference("cap1.m4a"))
        XCTAssertEqual(outcome, .retryLater(.audioInPlayback))
        XCTAssertTrue(storage.audioExists("cap1.m4a"))
        opened.close()
    }

    func testTheFenceRefusesARemovalOfHeldAudioEvenWithoutTheSweepsEarlierCheck() async throws {
        try retain(audio: "cap1.m4a")
        let fenced = PlaybackFencedFileSystem(fence: fence)
        let opened = try await makeService().open(itemID: "item-1")

        XCTAssertThrowsError(try fenced.remove(at: storage.audioURL("cap1.m4a"))) {
            XCTAssertEqual($0 as? PlaybackFenceError, .audioInUse)
        }
        XCTAssertTrue(storage.audioExists("cap1.m4a"))
        opened.close()
        XCTAssertNoThrow(try fenced.remove(at: storage.audioURL("cap1.m4a")))
    }

    // MARK: Fence against deletion

    func testDeletionCleanupCannotRemoveOpenAudio() async throws {
        try retain(audio: "cap1.m4a")
        let cleanup = DeletionLocalCleanup(layout: storage.layout, fileSystem: PlaybackFencedFileSystem(fence: fence))
        let opened = try await makeService().open(itemID: "item-1")

        XCTAssertThrowsError(
            try cleanup.removeAudio(captureID: "cap1", audioReference: RetentionFixtures.audioReference("cap1.m4a")))
        XCTAssertTrue(storage.audioExists("cap1.m4a"))

        opened.close()
        XCTAssertNoThrow(
            try cleanup.removeAudio(captureID: "cap1", audioReference: RetentionFixtures.audioReference("cap1.m4a")))
        XCTAssertFalse(storage.audioExists("cap1.m4a"))
    }

    func testADeletionIntentEndsPlaybackFirstSoCleanupCanRemoveTheAudio() async throws {
        try retain(audio: "cap1.m4a")
        let cleanup = DeletionLocalCleanup(layout: storage.layout, fileSystem: PlaybackFencedFileSystem(fence: fence))
        let service = makeService()
        let opened = try await service.open(itemID: "item-1")

        service.endPlayback(itemID: "item-1")

        XCTAssertEqual(opened.endReason, .deletionRequested)
        XCTAssertEqual(player.stopCalls, 1)
        XCTAssertNoThrow(
            try cleanup.removeAudio(captureID: "cap1", audioReference: RetentionFixtures.audioReference("cap1.m4a")))
        XCTAssertFalse(storage.audioExists("cap1.m4a"))
    }

    func testADeletionForAnotherItemLeavesPlaybackAlone() async throws {
        try retain(audio: "cap1.m4a")
        let service = makeService()
        let opened = try await service.open(itemID: "item-1")
        service.endPlayback(itemID: "item-2")
        XCTAssertTrue(opened.isOpen)
        opened.close()
    }

    func testAnOpenThatReadItsReferenceBeforeADeletionRequestTakesNoHold() async throws {
        try retain(audio: "cap1.m4a")
        let service = makeService()
        core.script(onRead: { service.endPlayback(itemID: "item-1") })
        await assertOpenFails(.itemDeleted, service)
    }

    // MARK: Relock

    func testRelockingEndsOpenPlaybackAndReleasesTheHold() async throws {
        try retain(audio: "cap1.m4a")
        let opened = try await makeService().open(itemID: "item-1")

        session.relock(.enteredBackground)

        XCTAssertEqual(opened.endReason, .relocked)
        XCTAssertEqual(player.stopCalls, 1)
        XCTAssertFalse(fence.isHeld(storage.audioURL("cap1.m4a")))
    }

    func testShuttingDownEndsOpenPlayback() async throws {
        try retain(audio: "cap1.m4a")
        let service = makeService()
        let opened = try await service.open(itemID: "item-1")
        service.shutDown()
        XCTAssertEqual(opened.endReason, .relocked)
    }

    func testAShutdownWhileThePlayerIsBuiltRefusesTheOpenAndReleasesTheHold() async throws {
        try retain(audio: "cap1.m4a")
        let holder = ServiceHolder()
        let service = PlaybackService(
            core: core, session: session, layout: storage.layout, fence: fence,
            playerFactory: { [player] _ in
                // After admission, before the session exists to be ended: shutdown changes neither the read epoch
                // nor the item's generation, so only the service's own closed state can refuse this open.
                holder.service?.shutDown()
                return player
            })
        holder.service = service
        await assertOpenFails(.shutDown, service)
        XCTAssertEqual(player.stopCalls, 1, "the player built after shutdown is stopped")
    }

    func testAnOpenAfterShutdownIsRefusedWithoutReadingTheCore() async throws {
        try retain(audio: "cap1.m4a")
        let service = makeService()
        service.shutDown()
        await assertOpenFails(.shutDown, service)
        XCTAssertEqual(core.reads, 0)
    }

    func testAShutdownWhileTheAudioIsBeingReadRefusesTheOpen() async throws {
        try retain(audio: "cap1.m4a")
        let service = makeService()
        core.script(onRead: { service.shutDown() })
        await assertOpenFails(.shutDown, service)
        XCTAssertEqual(player.stopCalls, 0, "no player is built once admission refuses the open")
    }
}
