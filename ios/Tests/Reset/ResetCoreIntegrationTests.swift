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

    func testMixedRouteCapturesAreErasedAndTheirLateCallbacksAreRejected() throws {
        let world = try makeWorld()
        let items = try seedCaptures(world)
        try world.addWorkRoute()
        let runtime = try world.launch()
        let personalItem = try runtime.capture(world.textRecord("cap-personal-text"))
        let workItem = try runtime.capture(world.workTextRecord("cap-work-text"))
        try world.stageWorkTextRecord(captureID: "cap-work-staged")

        let report = try runtime.reset(.keep)

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertEqual(report.progress?.generation, 1)
        XCTAssertEqual(report.progress?.cleanupPending, 0)
        let captureIDs = ["cap-1", "cap-2", "cap-staged", "cap-personal-text", "cap-work-text", "cap-work-staged"]
        for captureID in captureIDs {
            XCTAssertEqual(world.storage.remnants(of: captureID), [], "no file of \(captureID) remains")
        }
        let lateCallbacks: [(String, IngressRecord)] = [
            ("cap-personal-text", world.textRecord("cap-personal-text")),
            ("cap-work-text", world.workTextRecord("cap-work-text")),
            ("cap-work-staged", world.workTextRecord("cap-work-staged")),
        ]
        for (captureID, record) in lateCallbacks {
            XCTAssertEqual(
                runtime.session.importAndWait(record), .failed(.itemDeleted),
                "a late import of \(captureID) is rejected by the reset fence on either route")
        }
        for itemID in Array(items.values) + [personalItem, workItem] {
            XCTAssertEqual(try runtime.generation(ofItem: itemID), 0, "earlier captures stay in generation 0")
        }

        let freshWorkItem = try runtime.capture(world.workTextRecord("cap-work-new"))
        XCTAssertEqual(try runtime.generation(ofItem: freshWorkItem), 1, "a work capture after the reset uses the fresh generation")
    }

    func testARecordStagedAfterTheSnapshotIsNotImportedByResetAndBelongsToTheFreshGeneration() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        try world.addWorkRoute()
        let runtime = try world.launch(wrapStaging: { staging in
            SnapshotHookStaging(staging) { try? world.stageWorkTextRecord(captureID: "cap-post-tap") }
        })

        let report = try runtime.reset(.keep)

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertFalse(world.storage.remnants(of: "cap-post-tap").isEmpty, "the post-tap record is still staged")
        guard case .confirmed(let confirmation)? = runtime.session.importAndWait(world.workTextRecord("cap-post-tap")) else {
            return XCTFail("a capture staged after the tap must import")
        }
        XCTAssertFalse(confirmation.alreadyImported, "reset created no item for it")
        XCTAssertEqual(try runtime.generation(ofItem: confirmation.itemID), 1)
    }

    func testALiveCaptureSubmittedBetweenTheSnapshotAndTheCoreStartBelongsToTheFreshGeneration() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        try world.addWorkRoute()
        let reference = RuntimeReference()
        let runtime = try world.launch(wrapStaging: { staging in
            SnapshotHookStaging(staging) {
                reference.runtime?.submitLater(world.workTextRecord("cap-live")) { reference.outcome = $0 }
            }
        })
        reference.runtime = runtime

        let report = try runtime.reset(.keep)

        XCTAssertEqual(report.state, .completed, "blockers: \(report.blockers)")
        XCTAssertTrue(runtime.session.pump(until: { reference.outcome != nil }), "the held capture proceeds once the core started")
        guard case .saved(let acknowledgment)? = reference.outcome else {
            return XCTFail("the capture must be saved, not erased: \(String(describing: reference.outcome))")
        }
        XCTAssertFalse(acknowledgment.alreadyImported)
        XCTAssertEqual(try runtime.generation(ofItem: acknowledgment.itemID), 1, "the capture is fresh-generation content")
        XCTAssertEqual(try runtime.currentGeneration(), 1)
        reference.runtime = nil
        runtime.terminate()

        let relaunched = try world.launch()
        let resumed = try relaunched.resume()
        XCTAssertEqual(resumed.state, .completed)
        XCTAssertEqual(try relaunched.generation(ofItem: acknowledgment.itemID), 1, "a relaunch never erases it")
        guard case .confirmed(let confirmation)? = relaunched.session.importAndWait(world.workTextRecord("cap-live")) else {
            return XCTFail("the live capture must still import")
        }
        XCTAssertEqual(confirmation.itemID, acknowledgment.itemID)
        XCTAssertTrue(confirmation.alreadyImported)
    }

    func testALateVoiceHandoffThroughForegroundIngressLeavesNoRecordOrAudioAfterAReset() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let runtime = try world.launch()
        XCTAssertEqual(try runtime.reset(.keep).state, .completed)
        XCTAssertEqual(world.storage.remnants(of: "cap-1"), [])

        try Data("synthetic audio".utf8).write(to: world.storage.url(.ingressInProgressAudio, "cap-1-recording.m4a"))
        var outcome: IngressOutcome?
        runtime.submitLater(world.voiceRecord("cap-1")) { outcome = $0 }
        XCTAssertTrue(runtime.session.pump(until: { outcome != nil }))

        XCTAssertEqual(outcome, .itemDeleted(captureID: "cap-1"))
        XCTAssertEqual(world.storage.remnants(of: "cap-1"), [], "neither the record nor the audio it finalized survives")
        runtime.terminate()

        let relaunched = try world.launch()
        XCTAssertEqual(try relaunched.resume().state, .completed)
        XCTAssertEqual(world.storage.remnants(of: "cap-1"), [], "a relaunch finds nothing to restore")
    }

    // MARK: Crash after the tap

    func testATapRecordedBeforeTheCoreStartedIsHonoredByTheNextLaunchWithoutEarlyCredentialRemoval() throws {
        let world = try makeWorld()
        let items = try seedCaptures(world)
        let reference = try world.addProfile(version: "profile-a-v1")
        let killed = try world.launch()
        try killed.recordTap(.remove)
        XCTAssertEqual(try killed.currentGeneration(), 0, "the core never started")
        XCTAssertTrue(try world.isCredentialPresent(reference))
        XCTAssertFalse(try killed.markers.isCleanupOutstanding())
        killed.terminate()

        let relaunched = try world.launch()
        let resumed = try relaunched.resume()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertEqual(try relaunched.currentGeneration(), 1)
        XCTAssertTrue(try world.isProfileRevoked("profile-a-v1"))
        XCTAssertFalse(try world.isCredentialPresent(reference))
        XCTAssertNil(try relaunched.markers.request())
        XCTAssertFalse(try relaunched.markers.isCleanupOutstanding())
        for captureID in ["cap-1", "cap-2", "cap-staged"] {
            XCTAssertEqual(world.storage.remnants(of: captureID), [])
        }
        for itemID in items.values {
            XCTAssertEqual(try relaunched.generation(ofItem: itemID), 0)
        }
    }

    func testACaptureMadeAfterATapWhoseProcessDiedBelongsToTheFreshGenerationOfTheResumedReset() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        try world.addWorkRoute()
        let reference = try world.addProfile(version: "profile-a-v1")
        let killed = try world.launch()
        let request = try killed.recordTap(.remove)
        // The seeded voice captures were imported by the core but are still staged, so the tap lists them too.
        XCTAssertEqual(request.stagedBeforeTheTap.captureIDs, ["cap-1", "cap-2", "cap-staged"])
        var heldOutcome: IngressOutcome?
        killed.submitLater(world.workTextRecord("cap-post-tap")) { heldOutcome = $0 }
        XCTAssertTrue(killed.session.pump(until: { !world.storage.remnants(of: "cap-post-tap").isEmpty }))
        _ = killed.session.pump(until: { heldOutcome != nil }, timeout: 0.5)
        XCTAssertNil(heldOutcome, "the post-tap capture is staged and waits at the boundary")
        killed.terminate()
        try world.stageTextRecord(captureID: "cap-while-dead")

        let relaunched = try world.launch()
        try relaunched.recoverIngress()
        _ = relaunched.session.pump(until: { false }, timeout: 0.5)
        XCTAssertEqual(try relaunched.currentGeneration(), 0, "recovery ran first and the core has not started")
        XCTAssertFalse(world.storage.remnants(of: "cap-post-tap").isEmpty, "the relaunched gate holds post-tap captures")
        XCTAssertTrue(try world.isCredentialPresent(reference))

        let resumed = try relaunched.resume()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertEqual(try relaunched.currentGeneration(), 1)
        XCTAssertFalse(try world.isCredentialPresent(reference), "the recorded choice is honored")
        XCTAssertEqual(world.storage.remnants(of: "cap-staged"), [], "the pre-tap staged capture is erased")
        XCTAssertTrue(relaunched.session.pump(until: {
            world.storage.remnants(of: "cap-post-tap").isEmpty && world.storage.remnants(of: "cap-while-dead").isEmpty
        }), "the held captures import once the core recorded the reset")
        for record in [world.workTextRecord("cap-post-tap"), world.textRecord("cap-while-dead")] {
            guard case .confirmed(let confirmation)? = relaunched.session.importAndWait(record) else {
                return XCTFail("\(record.captureID) must be saved, not erased")
            }
            XCTAssertTrue(confirmation.alreadyImported, "\(record.captureID) was imported by recovery")
            XCTAssertEqual(
                try relaunched.generation(ofItem: confirmation.itemID), 1,
                "\(record.captureID) is fresh-generation content")
        }
        XCTAssertNil(try relaunched.markers.request())
    }

    func testATapAfterACompletedResetIsNotMistakenForThatResetByTheNextLaunch() throws {
        let world = try makeWorld()
        _ = try seedCaptures(world)
        let reference = try world.addProfile(version: "profile-a-v1")
        let first = try world.launch()
        XCTAssertEqual(try first.reset(.keep).state, .completed)
        let newItem = try first.capture(world.textRecord("cap-new"))
        try first.recordTap(.remove)
        first.terminate()

        let second = try world.launch()
        XCTAssertTrue(try world.isCredentialPresent(reference), "nothing was removed before the second reset ran")
        let resumed = try second.resume()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertEqual(resumed.progress?.generation, 2, "the new tap opened its own generation")
        XCTAssertEqual(try second.currentGeneration(), 2)
        XCTAssertEqual(world.storage.remnants(of: "cap-new"), [])
        XCTAssertEqual(try second.generation(ofItem: newItem), 1)
        XCTAssertFalse(try world.isCredentialPresent(reference))
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

/// Hands the runtime to a hook that is installed before the runtime exists, and collects the outcome it produces.
private final class RuntimeReference: @unchecked Sendable {
    var runtime: RealCoreResetRuntime?
    var outcome: IngressOutcome?
}
