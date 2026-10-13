import Security
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives `ResetService` through the production `CoreResetClient` and `CoreDeletionCleanupClient` into the real Rust core
/// over a file store, with real protected-storage files and the production ingress service. A relaunch closes the core
/// and opens a new one over the same disk, so nothing but the files survives, as after a kill. Native code keeps no
/// generation: every generation asserted here is read back from the core.
final class ResetCoreIntegrationTests: XCTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var worlds: [RealCoreResetWorld] = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDown() {
        worlds.forEach { $0.tearDown() }
        worlds = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    private func makeWorld() throws -> RealCoreResetWorld {
        let world = try RealCoreResetWorld()
        worlds.append(world)
        return world
    }

    /// Two imported voice captures and one staged text capture that was never imported.
    private func seedCaptures(_ world: RealCoreResetWorld) throws -> [String: String] {
        let voiceOne = try world.storage.importVoiceCapture(captureID: "cap-1", interruptedRecordWrite: true)
        let voiceTwo = try world.storage.importVoiceCapture(captureID: "cap-2")
        try world.stageTextRecord(captureID: "cap-staged")
        return ["cap-1": voiceOne, "cap-2": voiceTwo]
    }

    // MARK: Fence

    func testResetErasesEarlierCapturesAndTheCoreRejectsTheirLateCallbacks() throws {
        let world = try makeWorld()
        let items = try seedCaptures(world)
        let runtime = try world.launch()

        let report = try runtime.reset(.keep)

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertEqual(report.progress?.phase, .completed)
        XCTAssertEqual(report.progress?.generation, 1)
        XCTAssertEqual(report.progress?.itemsAwaitingTombstone, 0)
        XCTAssertEqual(report.progress?.cleanupPending, 0)
        for captureID in ["cap-1", "cap-2", "cap-staged"] {
            XCTAssertEqual(world.storage.remnants(of: captureID), [], "no file of \(captureID) remains")
        }

        let lateCallbacks: [(String, IngressRecord)] = [
            ("cap-1", world.voiceRecord("cap-1")),
            ("cap-2", world.voiceRecord("cap-2")),
            ("cap-staged", world.textRecord("cap-staged")),
        ]
        for (captureID, record) in lateCallbacks {
            XCTAssertEqual(
                runtime.session.importAndWait(record), .failed(.itemDeleted),
                "a late import of \(captureID) is rejected by the reset fence")
        }
        for itemID in items.values {
            XCTAssertEqual(try runtime.generation(ofItem: itemID), 0, "earlier captures stay in generation 0")
        }
        XCTAssertEqual(world.storage.remnants(of: "cap-1"), [], "a rejected callback restores nothing")
    }

    func testPostCompletionCapturesUseAFreshGenerationAndAreNotCleanedByARelaunch() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let first = try world.launch()
        XCTAssertEqual(try first.currentGeneration(), 0, "no reset has run, so captures belong to generation 0")
        XCTAssertEqual(try first.reset(.keep).state, .completed)
        XCTAssertEqual(try first.currentGeneration(), 1)

        let newItem = try first.capture(world.textRecord("cap-new"))
        XCTAssertEqual(try first.generation(ofItem: newItem), 1, "the new capture belongs to the fresh generation")
        first.terminate()

        let second = try world.launch()
        let resumed = try second.resume()

        XCTAssertEqual(resumed.state, .completed)
        XCTAssertEqual(try second.generation(ofItem: newItem), 1)
        XCTAssertEqual(try second.currentGeneration(), 1, "a restart opens no generation of its own")
        guard case .confirmed(let confirmation)? = second.session.importAndWait(world.textRecord("cap-new")) else {
            return XCTFail("the new capture must still import")
        }
        XCTAssertEqual(confirmation.itemID, newItem, "the new capture was never tombstoned")
        XCTAssertTrue(confirmation.alreadyImported)
    }

    func testSecondResetOpensTheNextGeneration() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let runtime = try world.launch()
        XCTAssertEqual(try runtime.reset(.keep).progress?.generation, 1)
        let newItem = try runtime.capture(world.textRecord("cap-new"))

        let second = try runtime.reset(.keep)

        XCTAssertEqual(second.state, .completed, "blockers: \(second.blockers)")
        XCTAssertEqual(second.progress?.generation, 2)
        XCTAssertEqual(try runtime.generation(ofItem: newItem), 1, "the capture keeps the generation it was made in")
        XCTAssertEqual(runtime.session.importAndWait(world.textRecord("cap-new")), .failed(.itemDeleted))
    }

    // MARK: Interruption

    func testAnInterruptedResetStaysPendingThenFinishesAfterARelaunchWithoutRestoringContent() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let first = try world.launch()
        world.storage.faults.arm("core.complete.remove_audio")

        let interrupted = try first.reset(.keep)

        XCTAssertEqual(interrupted.state, .pending)
        XCTAssertFalse(interrupted.isComplete)
        XCTAssertEqual(interrupted.progress?.phase, .pending, "the core, not native code, decides that the reset is unfinished")
        XCTAssertTrue(interrupted.blockers.contains(.itemCleanupIncomplete))
        XCTAssertEqual(try first.currentGeneration(), 1)
        first.terminate()

        let second = try world.launch()
        let resumed = try second.resume()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertEqual(try second.currentGeneration(), 1, "the retry resumed the pending generation")
        for captureID in ["cap-1", "cap-2", "cap-staged"] {
            XCTAssertEqual(world.storage.remnants(of: captureID), [])
        }
        XCTAssertEqual(second.session.importAndWait(world.voiceRecord("cap-1")), .failed(.itemDeleted))
    }

    // MARK: Credentials

    func testKeepingCredentialsLeavesProfilesAndSecretsAlone() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let reference = try world.addProfile(version: "profile-a-v1")
        let runtime = try world.launch()

        let report = try runtime.reset(.keep)
        runtime.terminate()

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertFalse(try world.isProfileRevoked("profile-a-v1"))
        XCTAssertTrue(try world.isCredentialPresent(reference))
        XCTAssertFalse(try runtime.markers.isRemovalRequested())
    }

    func testRemovingCredentialsRevokesEveryProfileAndDeletesItsSecret() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let referenceA = try world.addProfile(version: "profile-a-v1")
        let referenceB = try world.addProfile(version: "profile-b-v1")
        let runtime = try world.launch()

        let report = try runtime.reset(.remove)
        runtime.terminate()

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertTrue(try world.isProfileRevoked("profile-a-v1"))
        XCTAssertTrue(try world.isProfileRevoked("profile-b-v1"))
        XCTAssertFalse(try world.isCredentialPresent(referenceA))
        XCTAssertFalse(try world.isCredentialPresent(referenceB))
        XCTAssertFalse(try runtime.markers.isRemovalRequested(), "the choice is spent once carried out")
        XCTAssertFalse(try runtime.markers.isCleanupOutstanding())
    }

    func testAnInterruptedRemovalIsFinishedByTheNextLaunchBecauseTheChoiceSurvives() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let reference = try world.addProfile(version: "profile-a-v1")
        let first = try world.launch()
        world.keychain.forcedStatus[.remove] = errSecIO

        let interrupted = try first.reset(.remove)

        XCTAssertEqual(interrupted.state, .pending)
        XCTAssertEqual(interrupted.blockers, [.credentialRemovalIncomplete])
        XCTAssertEqual(interrupted.progress?.phase, .completed, "the core has nothing more to do; native removal is outstanding")
        XCTAssertTrue(try world.isCredentialPresent(reference))
        first.terminate()

        world.keychain.forcedStatus[.remove] = nil
        let second = try world.launch()
        XCTAssertTrue(try second.markers.isRemovalRequested(), "the explicit choice is durable across the restart")
        let resumed = try second.resume()
        second.terminate()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertTrue(try world.isProfileRevoked("profile-a-v1"))
        XCTAssertFalse(try world.isCredentialPresent(reference))
        XCTAssertFalse(try second.markers.isRemovalRequested())
    }
}
