import XCTest
@testable import OhAndServices

/// Native reset orchestration against the L05a reset contract (`FakeResetCore`), real protected-storage files and the
/// production per-item cleanup executors over a fake OS notification center. An interruption is an injected failure at
/// one step; a relaunch is a new `ResetService` over the same disk, core, staging and choice. The production core client
/// over the real Rust core is covered by `ResetCoreIntegrationTests`.
final class ResetServiceTests: ResetTestCase {
    private func seedVoiceAndTextCaptures(_ world: ResetWorld) async throws {
        try await world.seedItem(
            "item-1", captureID: "cap-1", finalizedAudio: true, stagedRecord: .audio, interruptedRecordWrite: true,
            reminders: ["reminder-1": [1, 2]])
        try await world.seedItem(
            "item-2", captureID: "cap-2", finalizedAudio: false, stagedRecord: .text, reminders: ["reminder-2": [1]])
    }

    private func assertEverythingErased(_ world: ResetWorld, file: StaticString = #filePath, line: UInt = #line) {
        let deletion = world.deletion
        for captureID in ["cap-1", "cap-2"] {
            XCTAssertEqual(deletion.remnants(of: captureID), [], "no file of \(captureID) remains", file: file, line: line)
        }
        XCTAssertEqual(deletion.pendingNotificationIdentifiers(ofReminder: "reminder-1"), [], file: file, line: line)
        XCTAssertEqual(deletion.pendingNotificationIdentifiers(ofReminder: "reminder-2"), [], file: file, line: line)
        XCTAssertTrue(deletion.core.isDeletionComplete("item-1"), file: file, line: line)
        XCTAssertTrue(deletion.core.isDeletionComplete("item-2"), file: file, line: line)
    }

    // MARK: Cleanup of captured content

    func testResetErasesMixedCapturesAndReportsCompletionOnlyAfterTheCoreDoes() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(report.state, .completed)
        XCTAssertTrue(report.isComplete)
        XCTAssertEqual(report.blockers, [])
        XCTAssertEqual(report.progress?.phase, .completed)
        XCTAssertEqual(report.progress?.generation, 1)
        XCTAssertEqual(report.itemCleanup?.converged, true)
        assertEverythingErased(world)
        XCTAssertEqual(world.exports.removeCalls, 1)
    }

    func testCleanupIsAcknowledgedBeforeTheCoreIsAskedToComplete() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)

        let initial = try await world.core.start()
        XCTAssertEqual(initial.phase, .pending, "tombstoning alone is not completion")
        XCTAssertGreaterThan(initial.cleanupPending, 0)
        XCTAssertFalse(world.deletion.core.isDeletionComplete("item-1"))

        let report = await world.makeService().resume()

        XCTAssertTrue(report.isComplete)
        assertEverythingErased(world)
    }

    func testARequiredFailureKeepsTheResetPendingAndARetryAfterRelaunchConverges() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("fs.remove:cap-1.m4a")

        let first = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(first.state, .pending)
        XCTAssertFalse(first.isComplete)
        XCTAssertTrue(first.blockers.contains(.itemCleanupIncomplete))
        XCTAssertTrue(first.blockers.contains(.coreResetPending))
        XCTAssertEqual(first.progress?.phase, .pending)
        XCTAssertEqual(world.faults.firedPoints, ["fs.remove:cap-1.m4a"])
        XCTAssertFalse(world.deletion.core.isDeletionComplete("item-1"), "the core records only cleanup that completed")
        XCTAssertEqual(
            world.deletion.pendingNotificationIdentifiers(ofReminder: "reminder-1"), [],
            "effects that did complete are not undone by the failure")

        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete)
        XCTAssertEqual(world.core.startCalls, 1, "a retry resumes the pending generation and never starts another")
        XCTAssertEqual(world.core.currentGeneration, 1)
        assertEverythingErased(world)
    }

    func testACrashBetweenCancellingANotificationAndRecordingItCannotRestoreTheReminder() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("notifier.cancel.afterEffect")

        let first = await world.makeService().reset(credentialChoice: .keep)
        XCTAssertFalse(first.isComplete)

        let scheduledBefore = world.deletion.center.pendingIdentifiers
        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete)
        XCTAssertTrue(
            Set(world.deletion.center.pendingIdentifiers).isSubset(of: Set(scheduledBefore)),
            "a retry never schedules a reminder")
        assertEverythingErased(world)
    }

    func testEveryInterruptionPointConvergesOnTheNextLaunch() async throws {
        let points = [
            "staging.snapshot", "staging.import", "staging.purge", "reset.start", "core.itemsWithPendingCleanup",
            "core.pendingCleanup", "notifier.cancel", "core.recordCancelled", "core.complete.cancel_notifications",
            "fs.remove:cap-1-recording.m4a", "fs.remove:cap-1.json", "core.complete.remove_audio", "exports.remove",
            "reset.advance",
        ]
        for point in points {
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            world.faults.arm(point)

            _ = await world.makeService().reset(credentialChoice: .keep)
            let converged = await world.makeService().reset(credentialChoice: .keep)

            XCTAssertTrue(converged.isComplete, "converges after a fault at \(point): \(converged.blockers)")
            assertEverythingErased(world)
        }
    }

    // MARK: Native ingress staging

    func testPreTapStagingIsImportedOrPurgedBeforeTheCoreStartsAndPostTapCapturesAreKept() async throws {
        let world = try makeResetWorld()
        world.makeImportedItemOnImport()
        world.staging.importable = ["cap-imported", "cap-post-tap"]
        world.staging.stage("cap-imported")
        world.staging.stage("cap-stuck")
        world.staging.onSnapshot = { [staging = world.staging] in staging.stage("cap-post-tap") }

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(report.isComplete)
        XCTAssertEqual(
            world.staging.importedCaptureIDs, ["cap-imported"],
            "only the snapshot is imported, even though the post-tap capture is a valid, importable record")
        XCTAssertEqual(world.staging.purgedCaptureIDs, ["cap-stuck"])
        XCTAssertEqual(world.staging.staged, ["cap-post-tap"], "a capture that landed after the tap is fresh-generation content")
        let snapshotIndex = try XCTUnwrap(world.log.index(of: "staging.snapshot"))
        let purgeIndex = try XCTUnwrap(world.log.index(of: "staging.purge"))
        let startIndex = try XCTUnwrap(world.log.index(of: "core.start"))
        XCTAssertLessThan(snapshotIndex, purgeIndex)
        XCTAssertLessThan(purgeIndex, startIndex, "staging is drained before the core records the reset")
        XCTAssertTrue(world.deletion.core.isDeletionComplete("item-cap-imported"), "an imported capture is tombstoned and cleaned")
    }

    func testStagingThatCannotBePurgedKeepsTheCoreFromStartingAndIsRetried() async throws {
        let world = try makeResetWorld()
        world.staging.stage("cap-stuck")
        world.faults.arm("staging.purge")

        let blocked = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(blocked.state, .notStarted)
        XCTAssertEqual(blocked.blockers, [.stagingPurgeIncomplete])
        XCTAssertEqual(world.core.startCalls, 0, "a leftover staged capture could land above the boundary after the reset")
        XCTAssertEqual(world.staging.staged, ["cap-stuck"])

        let retried = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(retried.isComplete)
        XCTAssertEqual(world.staging.staged, [])
    }

    func testUnreadableStagingIsAnUnknownAndNeverNothingToDo() async throws {
        let world = try makeResetWorld()
        world.faults.arm("staging.snapshot")
        let snapshotFailed = await world.makeService().reset(credentialChoice: .keep)
        XCTAssertEqual(snapshotFailed.blockers, [.stagingUnreadable])
        XCTAssertEqual(world.core.startCalls, 0)

        world.faults.arm("staging.purge.unreadable")
        let purgeUnknown = await world.makeService().reset(credentialChoice: .keep)
        XCTAssertEqual(purgeUnknown.blockers, [.stagingUnreadable])
        XCTAssertEqual(world.core.startCalls, 0)
    }

    func testASecondTapWhileAnEarlierResetIsPendingFinishesThatOneThenErasesWhatExistedAtTheSecondTap() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("fs.remove:cap-1.m4a")
        let first = await world.makeService().reset(credentialChoice: .keep)
        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(world.core.currentGeneration, 1)

        try await world.seedItem("item-between", captureID: "cap-between", stagedRecord: .text)
        world.staging.stage("cap-staged-between")
        let second = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(second.isComplete, "blockers: \(second.blockers)")
        XCTAssertEqual(second.progress?.generation, 2, "the second tap opened its own generation once the first completed")
        XCTAssertEqual(world.core.currentGeneration, 2)
        assertEverythingErased(world)
        XCTAssertTrue(world.deletion.core.isDeletionComplete("item-between"), "a capture that existed at the second tap is erased")
        XCTAssertEqual(world.deletion.remnants(of: "cap-between"), [])
        XCTAssertEqual(world.staging.purgedCaptureIDs, ["cap-staged-between"], "staging listed at the second tap is drained")
        XCTAssertEqual(world.staging.staged, [])
        XCTAssertNil(world.choice.recordedRequest)
        XCTAssertFalse(world.staging.isHolding)
        let events = world.log.events
        let advances = events.indices.filter { events[$0] == "core.advance" }
        XCTAssertEqual(advances.count, 3, "the first tap's, the earlier generation's completion and the second tap's")
        let secondImport = try XCTUnwrap(events.lastIndex(of: "staging.import"))
        XCTAssertLessThan(advances[1], secondImport, "the earlier generation completed before the second tap's staging ran")
    }

    func testASecondTapWaitsForAnEarlierResetThatCannotCompleteWithoutReportingSuccess() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("fs.remove:cap-1.m4a")
        _ = await world.makeService().reset(credentialChoice: .keep)
        world.staging.stage("cap-staged-between")
        // The earlier generation's cleanup now finishes, but the core cannot be asked to complete it.
        world.faults.arm("reset.advance")

        let waiting = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(waiting.state, .pending)
        XCTAssertEqual(waiting.blockers, [.coreUnavailable])
        XCTAssertEqual(waiting.progress?.generation, 1)
        XCTAssertEqual(world.core.startCalls, 1, "no generation is started for the second tap while the first is pending")
        XCTAssertEqual(world.choice.recordedRequest?.credentialChoice, .remove, "the second tap stays recorded")
        XCTAssertEqual(world.choice.recordedRequest?.awaitsEarlierReset, true)
        XCTAssertEqual(world.staging.staged, ["cap-staged-between"], "nothing of the second tap is drained yet")
        XCTAssertTrue(world.staging.isHolding, "the second tap keeps its boundary held")
        XCTAssertEqual(world.credentials.removedReferences, [], "the second tap's choice is not acted on before its generation")

        world.relaunch()
        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "blockers: \(resumed.blockers)")
        XCTAssertEqual(resumed.progress?.generation, 2)
        XCTAssertEqual(world.staging.purgedCaptureIDs, ["cap-staged-between"])
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertNil(world.choice.recordedRequest)
        assertEverythingErased(world)
    }

    func testARestartWithASecondTapRecordedWhileTheEarlierResetWasPendingNeverTakesThatResetForItsOwn() async throws {
        for point in ["none", "staging.import", "staging.purge", "before.start", "after.start"] {
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            world.faults.arm("fs.remove:cap-1.m4a")
            _ = await world.makeService().reset(credentialChoice: .keep)
            try await world.seedItem("item-between", captureID: "cap-between", stagedRecord: .text)
            world.staging.stage("cap-staged-between")
            let context = "crash at \(point)"

            if point == "none" {
                // A kill right after the tap was recorded: the earlier generation is still pending.
                let snapshot = try await world.staging.snapshot()
                try world.choice.recordRequest(ResetRequest(
                    credentialChoice: .keep, stagedBeforeTheTap: snapshot, awaitsEarlierReset: true))
            } else {
                await crashDuringReset(world, at: point, choice: .keep)
                XCTAssertNotNil(world.choice.recordedRequest, context)
            }
            world.relaunch()
            let resumed = await world.makeService().resume()

            XCTAssertTrue(resumed.isComplete, "\(context): \(resumed.blockers)")
            XCTAssertEqual(resumed.progress?.generation, 2, context)
            XCTAssertTrue(world.deletion.core.isDeletionComplete("item-between"), context)
            XCTAssertEqual(world.staging.staged, [], context)
            XCTAssertNil(world.choice.recordedRequest, context)
            assertEverythingErased(world)
        }
    }

    // MARK: Ingress boundary

    func testACaptureMadeAfterTheTapWaitsForTheCoreAndBelongsToTheFreshGeneration() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.staging.onSnapshot = { [staging = world.staging] in staging.liveCapture("cap-live") }
        world.staging.onLiveImport = { [deletion = world.deletion, core = world.core] captureID in
            deletion.core.addItem("item-\(captureID)", captureID: captureID, audioReference: nil)
            core.register("item-\(captureID)")
        }

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(report.isComplete, "blockers: \(report.blockers)")
        XCTAssertEqual(world.staging.liveImportedCaptureIDs, ["cap-live"])
        XCTAssertEqual(world.staging.purgedCaptureIDs, [], "a capture made after the tap is never purged")
        let startIndex = try XCTUnwrap(world.log.index(of: "core.start"))
        let importIndex = try XCTUnwrap(world.log.index(of: "staging.live.import:cap-live"))
        let releaseIndex = try XCTUnwrap(world.log.index(of: "staging.release"))
        XCTAssertLessThan(startIndex, releaseIndex, "the boundary is held until the core recorded the reset")
        XCTAssertLessThan(releaseIndex, importIndex)
        XCTAssertNil(world.deletion.core.status("item-cap-live", .removeAudio), "the fresh capture is not tombstoned")
        XCTAssertFalse(world.staging.isHolding)
    }

    func testTheBoundaryIsReleasedOnEveryPathThatEndsBeforeTheCoreStarts() async throws {
        for point in ["staging.purge", "staging.purge.unreadable", "reset.start"] {
            let world = try makeResetWorld()
            world.staging.stage("cap-stuck")
            world.staging.onSnapshot = { [staging = world.staging] in staging.liveCapture("cap-live") }
            world.faults.arm(point)

            _ = await world.makeService().reset(credentialChoice: .keep)

            XCTAssertFalse(world.staging.isHolding, "the boundary is released after a fault at \(point)")
            XCTAssertEqual(world.staging.liveImportedCaptureIDs, ["cap-live"], "the waiting capture proceeds: \(point)")
        }

        // The core is asked before the tap is taken, so a core that cannot be asked holds nothing in the first place.
        let world = try makeResetWorld()
        world.staging.stage("cap-stuck")
        world.faults.arm("reset.status")

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [.coreUnavailable])
        XCTAssertEqual(world.staging.snapshotCalls, 0, "no tap is taken when the core cannot be asked")
        XCTAssertFalse(world.staging.isHolding)
        world.staging.liveCapture("cap-live")
        XCTAssertEqual(world.staging.liveImportedCaptureIDs, ["cap-live"], "a capture made afterwards proceeds at once")
    }

    func testACancelledResetKeepsTheRecordedTapAndReportsItAsPendingNeverNotStarted() async throws {
        let world = try makeResetWorld()
        configureProfiles(world)
        world.staging.stage("cap-stuck")
        world.staging.onImportSnapshot = { withUnsafeCurrentTask { $0?.cancel() } }
        let service = world.makeService()

        let report = await Task { await service.reset(credentialChoice: .remove) }.value

        XCTAssertEqual(report.state, .pending)
        XCTAssertEqual(report.blockers, [.cancelled])
        XCTAssertEqual(world.choice.requested, .remove, "the recorded tap is what the next launch runs")
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertTrue(world.staging.isHolding, "while the tap is recorded no capture may land below its boundary")

        world.staging.onImportSnapshot = nil
        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "blockers: \(resumed.blockers)")
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertFalse(world.staging.isHolding)
    }

    func testAnUnwithdrawableRequestIsReportedAndRunByTheNextLaunchNeverHiddenBehindNotStarted() async throws {
        let world = try makeResetWorld()
        configureProfiles(world)
        world.staging.stage("cap-stuck")
        world.faults.arm("staging.purge")
        world.faults.arm("intent.clear")

        let blocked = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(blocked.state, .pending, "the request is still recorded, so the reset is not 'not started'")
        XCTAssertEqual(blocked.blockers, [.stagingPurgeIncomplete, .markerNotCleared])
        XCTAssertEqual(world.choice.requested, .remove)
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.credentials.removedReferences, [], "nothing is removed before the core started")
        XCTAssertTrue(world.staging.isHolding, "the request that remains keeps the boundary held")

        world.relaunch()
        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete, "blockers: \(relaunched.blockers)")
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertNil(world.choice.requested)
        XCTAssertFalse(world.staging.isHolding)
    }

    // MARK: Fresh generation

    func testCapturesAfterCompletionBelongToTheNextGenerationAndAreNeverCleaned() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        let completed = await world.makeService().reset(credentialChoice: .keep)
        XCTAssertTrue(completed.isComplete)

        try await world.seedItem("item-new", captureID: "cap-new", stagedRecord: .text, reminders: ["reminder-new": [1]])
        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete)
        XCTAssertEqual(world.core.startCalls, 1, "resume never starts a reset")
        XCTAssertTrue(world.deletion.exists(.finalizedAudio, "cap-new.m4a"))
        XCTAssertTrue(world.deletion.exists(.ingressStagingRecords, "cap-new.json"))
        XCTAssertEqual(world.deletion.pendingNotificationIdentifiers(ofReminder: "reminder-new"), ["reminder-new#1"])
        XCTAssertNil(world.deletion.core.status("item-new", .removeAudio), "no cleanup was queued for the new capture")
    }

    func testASecondResetAfterCompletionOpensTheNextGeneration() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        _ = await world.makeService().reset(credentialChoice: .keep)
        try await world.seedItem("item-new", captureID: "cap-new", stagedRecord: .text)

        let second = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(second.isComplete)
        XCTAssertEqual(second.progress?.generation, 2)
        XCTAssertEqual(world.deletion.remnants(of: "cap-new"), [])
    }

    func testResumeWithNothingToDoStartsNothing() async throws {
        let world = try makeResetWorld()

        let report = await world.makeService().resume()

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [])
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.staging.snapshotCalls, 0)
    }

    func testAnUnreachableCoreIsReportedAndStartsNothing() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("reset.start")

        let failed = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(failed.state, .notStarted)
        XCTAssertEqual(failed.blockers, [.coreUnavailable])
        XCTAssertTrue(world.deletion.exists(.finalizedAudio, "cap-1.m4a"), "no content is removed before the core records the reset")
        XCTAssertEqual(world.deletion.pendingNotificationIdentifiers(ofReminder: "reminder-1").count, 2)
    }

    func testExportTemporariesThatRemainKeepTheResetPending() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("exports.remove")

        let first = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(first.blockers, [.exportTemporariesRemain])
        XCTAssertEqual(first.progress?.phase, .completed, "the core has nothing more to do; the native step is outstanding")

        XCTAssertTrue(world.choice.isOutstanding, "the unfinished native step is remembered across a restart")

        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete, "resume finishes the native step the core cannot see: \(relaunched.blockers)")
        XCTAssertEqual(world.exports.removeCalls, 2)
        XCTAssertFalse(world.choice.isOutstanding)
        XCTAssertEqual(world.core.startCalls, 1)

        let settled = await world.makeService().resume()
        XCTAssertEqual(settled.state, .completed)
        XCTAssertEqual(world.exports.removeCalls, 2, "a finished reset leaves later exports alone")
    }

    func testTheOutstandingMarkerIsClearedOnlyByACompletedReport() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("outstanding.clear")

        let first = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(first.blockers, [.markerNotCleared])
        XCTAssertTrue(world.choice.isOutstanding)

        let second = await world.makeService().resume()
        XCTAssertTrue(second.isComplete)
        XCTAssertFalse(world.choice.isOutstanding)
    }

    func testABlockedResetLeavesNoMarkerForARestartToActOn() async throws {
        let world = try makeResetWorld()
        configureProfiles(world)
        world.staging.stage("cap-stuck")
        world.faults.arm("staging.purge")

        let blocked = await world.makeService().reset(credentialChoice: .remove)
        XCTAssertEqual(blocked.blockers, [.stagingPurgeIncomplete])
        XCTAssertFalse(world.choice.isRequested)
        XCTAssertFalse(world.choice.isOutstanding)
        XCTAssertNil(world.choice.requested, "a tap the user was told did not start is not kept for a restart")

        let relaunched = await world.makeService().resume()
        XCTAssertEqual(relaunched.state, .notStarted)
        XCTAssertEqual(world.credentials.removedReferences, [], "credentials are not removed by a reset that never started")
        XCTAssertEqual(world.core.revokedProfileVersions, [])
    }

    // MARK: Credential choice

    private func configureProfiles(_ world: ResetWorld) {
        world.core.setProfiles([
            ResetProfileCredential(profileVersion: "profile-a-v1", credentialReference: "ref-a", isRevoked: false),
            ResetProfileCredential(profileVersion: "profile-b-v1", credentialReference: "ref-b", isRevoked: true),
            ResetProfileCredential(profileVersion: "profile-c-v1", credentialReference: nil, isRevoked: false),
        ])
    }

    func testKeepingCredentialsTouchesNoProfileAndNoSecret() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(report.isComplete)
        XCTAssertEqual(world.core.revokedProfileVersions, [])
        XCTAssertEqual(world.credentials.removedReferences, [])
        XCTAssertFalse(world.choice.isRequested)
    }

    func testRemovingCredentialsRevokesProfilesAndDeletesTheirSecrets() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertTrue(report.isComplete)
        XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"], "already revoked profiles are not revoked again")
        XCTAssertEqual(world.credentials.removedReferences, ["ref-a", "ref-b"])
        XCTAssertFalse(world.choice.isRequested, "the choice is spent once it was carried out")
        let claimIndex = try XCTUnwrap(world.log.index(of: "core.claim"))
        let revokeIndex = try XCTUnwrap(world.log.index(of: "core.revoke:profile-a-v1"))
        let deleteIndex = try XCTUnwrap(world.log.index(of: "credentials.delete:ref-a"))
        let releaseIndex = try XCTUnwrap(world.log.index(of: "core.claims.release"))
        XCTAssertLessThan(claimIndex, revokeIndex, "the references are claimed before anything is touched")
        XCTAssertLessThan(revokeIndex, deleteIndex, "a profile stops dispatching before its secret goes")
        XCTAssertLessThan(deleteIndex, releaseIndex, "the claims are released only once every secret is gone")
        XCTAssertEqual(world.core.credentialClaimSet, [])
    }

    func testAFailedSecretRemovalKeepsTheResetPendingAndTheChoiceSurvivesARelaunch() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("credentials.delete")

        let first = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete])
        XCTAssertEqual(first.progress?.phase, .completed)
        XCTAssertTrue(world.choice.isRequested)

        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete)
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertFalse(world.choice.isRequested)
    }

    private static let laterProfile = ResetProfileCredential(
        profileVersion: "profile-d-v1", credentialReference: "ref-d", isRevoked: false)

    func testARetriedRemovalNeverTouchesAProfileConfiguredAfterTheTap() async throws {
        for retriedBy in ["resume", "keep tap"] {
            let context = "retried by \(retriedBy)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            world.faults.arm("credentials.delete")
            let first = await world.makeService().reset(credentialChoice: .remove)
            XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete], context)
            XCTAssertEqual(first.progress?.phase, .completed, context)
            XCTAssertEqual(
                world.choice.removal?.profileVersions, ["profile-a-v1", "profile-b-v1", "profile-c-v1"],
                "the owed removal binds the profiles that existed at the tap: \(context)")

            // In the fresh generation, with the removal still owed, the user configures a new profile and its secret.
            try world.core.addProfile(Self.laterProfile)
            let report: ResetReport
            if retriedBy == "resume" {
                report = await world.makeService().resume()
            } else {
                report = await world.makeService().reset(credentialChoice: .keep)
            }

            XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
            XCTAssertEqual(
                world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"],
                "only the profiles bound at the tap are revoked, each once: \(context)")
            XCTAssertEqual(
                Set(world.credentials.removedReferences), ["ref-a", "ref-b"],
                "only the secrets bound at the tap are removed: \(context)")
            XCTAssertEqual(world.core.isProfileRevoked("profile-d-v1"), false, context)
            XCTAssertFalse(world.choice.isRequested, "the removal is spent once what it binds is gone: \(context)")
            XCTAssertNil(world.choice.recordedRequest, context)
        }
    }

    func testARetriedRemovalLeavesASecretEnteredAfterTheTapBehindABoundReferenceAlone() async throws {
        for retriedBy in ["resume", "keep tap"] {
            let context = "retried by \(retriedBy)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            world.faults.arm("credentials.delete")
            let first = await world.makeService().reset(credentialChoice: .remove)
            XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete], context)
            XCTAssertEqual(
                world.choice.removal?.secrets.map(\.credentialReference), ["ref-a", "ref-b"],
                "the owed removal binds the secrets stored at the tap: \(context)")

            // With the removal still owed, the user enters a new secret behind the bound reference ref-a (the reference
            // is stable and mutable), as the first step of editing the profile; ref-b is left as it was at the tap.
            world.credentials.update("ref-a")
            let report: ResetReport
            if retriedBy == "resume" {
                report = await world.makeService().resume()
            } else {
                report = await world.makeService().reset(credentialChoice: .keep)
            }

            XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
            XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"], context)
            XCTAssertEqual(world.credentials.removedReferences, ["ref-b"], "only the secret still stored as at the tap goes: \(context)")
            XCTAssertEqual(world.credentials.keptReferences, ["ref-a"], "the secret entered since stays: \(context)")
            XCTAssertTrue(world.credentials.isPresent("ref-a"), context)
            XCTAssertFalse(world.choice.isRequested, "the removal is spent: nothing of what it bound remains: \(context)")
            XCTAssertNil(world.choice.recordedRequest, context)
        }
    }

    func testARetriedRemovalLeavesABoundSecretThatAProfileConfiguredBeforeTheClaimHoldsAlone() async throws {
        for retriedBy in ["resume", "keep tap"] {
            let context = "retried by \(retriedBy)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            // The process dies right after the core started the remove tap's reset: the choice is recorded with what it
            // binds, and nothing is claimed yet.
            await crashDuringReset(world, at: "after.start", choice: .remove)
            world.relaunch()
            XCTAssertEqual(world.core.credentialClaimSet, [], context)

            // Before the removal claims anything, the user edits the revoked profile a: the edit mints a new version that
            // holds the same reference, and the secret behind it is unchanged.
            try world.core.addProfile(ResetProfileCredential(profileVersion: "profile-a-v2", credentialReference: "ref-a", isRevoked: false))
            let report: ResetReport
            if retriedBy == "resume" {
                report = await world.makeService().resume()
            } else {
                report = await world.makeService().reset(credentialChoice: .keep)
            }

            XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
            XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"], context)
            XCTAssertEqual(world.core.isProfileRevoked("profile-a-v2"), false, "the later version is not bound: \(context)")
            XCTAssertEqual(
                world.credentials.removedReferences, ["ref-b"],
                "a reference a profile outside the choice holds is never claimed, so its secret stays: \(context)")
            XCTAssertTrue(world.credentials.isPresent("ref-a"), context)
            XCTAssertEqual(world.core.credentialClaimSet, [], "the claims are released once the removal is carried out: \(context)")
            XCTAssertFalse(world.choice.isRequested, context)
        }
    }

    func testAProfileThatWouldTakeAClaimedReferenceIsRefusedBetweenTheClaimAndTheDeletion() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        let laterVersion = ResetProfileCredential(profileVersion: "profile-a-v2", credentialReference: "ref-a", isRevoked: false)
        // The edit that mints a new version of profile a without re-entering its secret, attempted at every moment
        // between the claim and the deletion.
        let attempt: @Sendable (String) -> Void = { [core = world.core, log = world.log] moment in
            do {
                try core.addProfile(laterVersion)
                log.append("profile.written:\(moment)")
            } catch is FakeResetCore.ProfileRefused {
                log.append("profile.refused:\(moment)")
            } catch {
                log.append("profile.failed:\(moment)")
            }
        }
        world.core.afterClaim = { [core = world.core] in
            attempt("after the claim")
            core.afterProfileCredentials = { attempt("after the profiles were listed") }
        }
        world.credentials.beforeDelete = { attempt("before deleting \($0)") }

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertTrue(report.isComplete, "\(report.blockers)")
        XCTAssertEqual(
            world.log.events.filter { $0.hasPrefix("profile.") },
            [
                "profile.refused:after the claim", "profile.refused:after the profiles were listed",
                "profile.refused:before deleting ref-a", "profile.refused:before deleting ref-b",
            ],
            "storage refuses the profile at every moment while the reference is claimed")
        XCTAssertNil(world.core.isProfileRevoked("profile-a-v2"), "the refused profile does not exist")
        XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"])
        XCTAssertEqual(world.credentials.removedReferences, ["ref-a", "ref-b"])
        XCTAssertEqual(world.core.credentialClaimSet, [])
        // The fence is lifted with the claims: the reference is free again (its secret is gone; the user enters one).
        try world.core.addProfile(laterVersion)
    }

    func testACrashBetweenTheDeletionAndTheReleaseIsFinishedFromTheClaimSetWithoutDeletingAgain() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        let laterVersion = ResetProfileCredential(profileVersion: "profile-a-v2", credentialReference: "ref-a", isRevoked: false)
        await crashDuringReset(world, at: "before.claims.release", choice: .remove)

        XCTAssertEqual(world.credentials.removedReferences, ["ref-a", "ref-b"], "every claimed secret was deleted before the release")
        XCTAssertEqual(world.core.credentialClaimSet, ["ref-a", "ref-b"], "the claim set survives the process")
        XCTAssertTrue(world.choice.isRequested, "the removal is still owed")
        world.relaunch()
        XCTAssertThrowsError(try world.core.addProfile(laterVersion), "the fence holds until the removal is finished") {
            XCTAssertTrue($0 is FakeResetCore.ProfileRefused)
        }

        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "\(resumed.blockers)")
        XCTAssertEqual(world.credentials.removedReferences, ["ref-a", "ref-b"], "the retry works from the claim set and deletes nothing twice")
        XCTAssertEqual(world.core.credentialClaimSet, [])
        XCTAssertFalse(world.choice.isRequested)
        try world.core.addProfile(laterVersion)
    }

    func testAReleaseThatFailsKeepsTheRemovalOwedAndIsRetried() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("reset.claims.release")

        let first = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete])
        XCTAssertEqual(world.core.credentialClaimSet, ["ref-a", "ref-b"])
        XCTAssertTrue(world.choice.isRequested, "the removal is spent only once the claims are released")

        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "\(resumed.blockers)")
        XCTAssertEqual(world.credentials.removedReferences, ["ref-a", "ref-b"])
        XCTAssertEqual(world.core.credentialClaimSet, [])
        XCTAssertFalse(world.choice.isRequested)
    }

    func testARemoveTapBindsNoSecretBehindAReferenceThatHoldsNoneAndASecretEnteredThereLaterStays() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.credentials.markAbsent("ref-b")
        world.faults.arm("credentials.delete")
        let first = await world.makeService().reset(credentialChoice: .remove)
        XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete])
        XCTAssertEqual(world.choice.removal?.secrets.map(\.credentialReference), ["ref-a"], "ref-b held nothing at the tap")
        // After the tap, the user enters a secret behind the (revoked) profile b's reference.
        world.credentials.update("ref-b")

        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "blockers: \(resumed.blockers)")
        XCTAssertEqual(world.credentials.removedReferences, ["ref-a"])
        XCTAssertTrue(world.credentials.isPresent("ref-b"), "a secret entered after the tap is fresh-generation state")
        XCTAssertFalse(world.choice.isRequested)
    }

    func testARemoveTapWhoseSecretsCannotBeIdentifiedStartsNothingAndIsRetried() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("credentials.identity")

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [.credentialsUnreadable])
        XCTAssertNil(world.choice.recordedRequest, "a tap whose choice cannot be bound is not recorded")
        XCTAssertFalse(world.staging.isHolding, "the boundary is released with the withdrawn tap")
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.core.revokedProfileVersions, [])
        XCTAssertEqual(world.credentials.removedReferences, [])

        let retried = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertTrue(retried.isComplete, "blockers: \(retried.blockers)")
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
    }

    func testATapThatWaitsForARestartBindsOnlyTheProfilesThatExistedAtTheTap() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        await crashDuringReset(world, at: "before.start", choice: .remove)
        XCTAssertEqual(
            world.choice.recordedRequest?.credentialRemoval?.profileVersions, ["profile-a-v1", "profile-b-v1", "profile-c-v1"],
            "the recorded tap carries the profiles its choice applies to")
        XCTAssertFalse(world.choice.isRequested, "nothing is armed before the core started")

        try world.core.addProfile(Self.laterProfile)
        world.relaunch()
        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "blockers: \(resumed.blockers)")
        XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"])
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertEqual(world.core.isProfileRevoked("profile-d-v1"), false, "a profile configured after the tap is not bound by it")
        XCTAssertFalse(world.choice.isRequested)
    }

    func testARemoveTapWhoseProfilesCannotBeListedStartsNothingAndIsRetried() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("reset.profiles")

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [.coreUnavailable])
        XCTAssertNil(world.choice.recordedRequest, "a tap whose choice cannot be bound is not recorded")
        XCTAssertFalse(world.staging.isHolding, "the boundary is released with the withdrawn tap")
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.credentials.removedReferences, [])

        let retried = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertTrue(retried.isComplete, "blockers: \(retried.blockers)")
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
    }

    func testARemoveTapBindsBeforeTheBoundaryAndWhatIsWrittenInBetweenIsLaterState() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        let requestedInTheWindow = ResetEventLog()
        // Between the binding and the tap boundary: an edit mints a new version of profile a that holds its reference
        // without re-entering the secret, and the user enters a new secret behind profile b's reference.
        world.staging.beforeSnapshot = { [world] in
            requestedInTheWindow.append(world.choice.recordedRequest == nil ? "no request yet" : "request recorded")
            try? world.core.addProfile(
                ResetProfileCredential(profileVersion: "profile-a-v2", credentialReference: "ref-a", isRevoked: false))
            world.credentials.update("ref-b")
        }

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertTrue(report.isComplete, "blockers: \(report.blockers)")
        XCTAssertEqual(requestedInTheWindow.events, ["no request yet"], "the window precedes the recorded tap")
        let profilesIndex = try XCTUnwrap(world.log.index(of: "core.profiles"))
        let identityIndex = try XCTUnwrap(world.log.index(of: "credentials.identity:ref-a"))
        let snapshotIndex = try XCTUnwrap(world.log.index(of: "staging.snapshot"))
        XCTAssertLessThan(profilesIndex, snapshotIndex, "the profiles are bound before the boundary")
        XCTAssertLessThan(identityIndex, snapshotIndex, "the secret identities are bound before the boundary")
        XCTAssertEqual(world.core.revokedProfileVersions, ["profile-a-v1", "profile-c-v1"], "the bound profiles are revoked")
        XCTAssertEqual(world.core.isProfileRevoked("profile-a-v2"), false, "a profile written after the binding is later state")
        XCTAssertEqual(world.credentials.removedReferences, [], "neither secret is the one the choice bound")
        XCTAssertTrue(world.credentials.isPresent("ref-a"), "ref-a is held by the later profile and was never claimed")
        XCTAssertTrue(world.credentials.isPresent("ref-b"), "the secret entered after the binding stays")
        XCTAssertEqual(world.credentials.keptReferences, ["ref-b"], "ref-b was claimed, but its identity no longer matches")
        XCTAssertFalse(world.choice.isRequested)
        XCTAssertNil(world.choice.recordedRequest)
    }

    func testAKeepTapAfterAnUnfinishedRemovalCarriesThatRemovalOutBeforeItsOwnGeneration() async throws {
        for removalFailsAgain in [false, true] {
            let context = "removal fails again: \(removalFailsAgain)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            world.faults.arm("credentials.delete")
            let first = await world.makeService().reset(credentialChoice: .remove)
            XCTAssertEqual(first.blockers, [.credentialRemovalIncomplete], context)
            XCTAssertEqual(first.progress?.phase, .completed, context)
            try await world.seedItem("item-between", captureID: "cap-between", stagedRecord: .text)
            world.staging.stage("cap-staged-between")
            if removalFailsAgain { world.faults.arm("credentials.delete") }

            var report = await world.makeService().reset(credentialChoice: .keep)

            if removalFailsAgain {
                XCTAssertEqual(report.state, .pending, context)
                XCTAssertEqual(report.blockers, [.credentialRemovalIncomplete], context)
                XCTAssertEqual(report.progress?.generation, 1, context)
                XCTAssertEqual(world.core.startCalls, 1, "no generation is opened for the keep tap while the removal is owed")
                XCTAssertTrue(world.choice.isRequested, "the earlier remove choice stays armed: \(context)")
                XCTAssertEqual(world.choice.recordedRequest?.credentialChoice, .keep, context)
                XCTAssertEqual(world.choice.recordedRequest?.awaitsEarlierReset, true, context)
                XCTAssertEqual(world.staging.staged, ["cap-staged-between"], "nothing of the keep tap is drained yet")
                XCTAssertTrue(world.staging.isHolding, context)

                world.relaunch()
                report = await world.makeService().resume()
            }

            XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
            XCTAssertEqual(report.progress?.generation, 2, context)
            XCTAssertEqual(
                Set(world.credentials.removedReferences), ["ref-a", "ref-b"],
                "the earlier remove choice was carried out, not cancelled by the keep tap: \(context)")
            XCTAssertFalse(world.choice.isRequested, context)
            XCTAssertFalse(world.choice.isOutstanding, context)
            XCTAssertNil(world.choice.recordedRequest, context)
            XCTAssertTrue(world.deletion.core.isDeletionComplete("item-between"), context)
            XCTAssertEqual(world.staging.staged, [], context)
            assertEverythingErased(world)
        }
    }

    func testKeepAfterAnEarlierRemoveRequestWithdrawsIt() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        await crashDuringReset(world, at: "before.start", choice: .remove)
        XCTAssertEqual(world.choice.requested, .remove)

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertTrue(report.isComplete)
        XCTAssertEqual(world.credentials.removedReferences, [])
        XCTAssertEqual(world.core.revokedProfileVersions, [])
    }

    func testAKeepTapAfterARemoveTapWhoseProcessDiedRightAfterTheCoreStartedCarriesTheRemovalOutFirst() async throws {
        for replacementRecorded in [true, false] {
            let context = "keep tap recorded: \(replacementRecorded)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            await crashDuringReset(world, at: "after.start", choice: .remove)
            XCTAssertEqual(world.core.currentGeneration, 1, context)
            XCTAssertEqual(world.choice.requested, .remove, "the recorded tap is the only durable copy of the choice: \(context)")
            XCTAssertFalse(world.choice.isRequested, context)
            XCTAssertFalse(world.choice.isOutstanding, context)
            world.relaunch()
            // Made in the fresh generation of the started reset, before the keep tap.
            try await world.seedItem("item-between", captureID: "cap-between")
            if !replacementRecorded { world.faults.arm("intent.record") }

            let report = await world.makeService().reset(credentialChoice: .keep)

            if replacementRecorded {
                XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
                XCTAssertEqual(report.progress?.generation, 2, "the keep tap's own generation ran after the earlier reset: \(context)")
                XCTAssertTrue(world.deletion.core.isDeletionComplete("item-between"), "pre-tap content of the keep tap: \(context)")
            } else {
                XCTAssertEqual(report.state, .pending, "an earlier reset is still owed, so this is not 'not started': \(context)")
                XCTAssertEqual(report.blockers, [.markerNotRecorded], context)
                XCTAssertNil(
                    world.choice.recordedRequest,
                    "the started tap was carried over before it was withdrawn, so withdrawing it loses nothing: \(context)")
                XCTAssertEqual(world.choice.removal?.profileVersions, ["profile-a-v1", "profile-b-v1", "profile-c-v1"], context)
                XCTAssertTrue(world.choice.isOutstanding, context)
                XCTAssertEqual(world.credentials.removedReferences, [], "nothing is removed by a tap that was not recorded: \(context)")
                XCTAssertFalse(world.staging.isHolding, context)

                let resumed = await world.makeService().resume()

                XCTAssertTrue(resumed.isComplete, "\(context): \(resumed.blockers)")
                XCTAssertEqual(resumed.progress?.generation, 1, "no generation was opened for the keep tap: \(context)")
                XCTAssertNil(world.deletion.core.status("item-between", .removeAudio), "fresh content of the started reset: \(context)")
            }
            XCTAssertEqual(
                Set(world.credentials.removedReferences), ["ref-a", "ref-b"],
                "the earlier remove choice was carried out, not cancelled by the keep tap: \(context)")
            XCTAssertEqual(Set(world.core.revokedProfileVersions), ["profile-a-v1", "profile-c-v1"], context)
            XCTAssertFalse(world.choice.isRequested, context)
            XCTAssertFalse(world.choice.isOutstanding, context)
            XCTAssertNil(world.choice.recordedRequest, context)
            assertEverythingErased(world)
        }
    }

    func testAKeepTapAfterARemoveTapWhoseChoiceCouldNotBeCarriedOverCarriesItOutFirst() async throws {
        for carryOverFailsAgain in [false, true] {
            let context = "carry-over fails again: \(carryOverFailsAgain)"
            let world = try makeResetWorld()
            try await seedVoiceAndTextCaptures(world)
            configureProfiles(world)
            world.faults.arm("choice.record")
            let interrupted = await world.makeService().reset(credentialChoice: .remove)
            XCTAssertEqual(interrupted.blockers, [.markerNotRecorded], context)
            XCTAssertEqual(world.choice.requested, .remove, "the request still carries the choice: \(context)")
            XCTAssertEqual(world.core.currentGeneration, 1, context)
            if carryOverFailsAgain { world.faults.arm("choice.record") }

            var report = await world.makeService().reset(credentialChoice: .keep)

            if carryOverFailsAgain {
                XCTAssertEqual(report.state, .pending, context)
                XCTAssertEqual(report.blockers, [.markerNotRecorded], context)
                XCTAssertEqual(world.choice.requested, .remove, "the started remove tap stays recorded: \(context)")
                XCTAssertEqual(world.credentials.removedReferences, [], context)
                XCTAssertEqual(world.staging.snapshotCalls, 1, "the keep tap holds nothing new before the earlier tap is safe: \(context)")

                report = await world.makeService().resume()
                XCTAssertEqual(report.progress?.generation, 1, "the resumed request finishes its own generation: \(context)")
            } else {
                XCTAssertEqual(report.progress?.generation, 2, "the keep tap's own generation ran after the earlier reset: \(context)")
            }
            XCTAssertTrue(report.isComplete, "\(context): \(report.blockers)")
            XCTAssertEqual(
                Set(world.credentials.removedReferences), ["ref-a", "ref-b"],
                "the earlier remove choice was carried out, not replaced by the keep tap: \(context)")
            XCTAssertFalse(world.choice.isRequested, context)
            XCTAssertFalse(world.choice.isOutstanding, context)
            XCTAssertNil(world.choice.recordedRequest, context)
            assertEverythingErased(world)
        }
    }

    func testATapThatCannotAskTheCoreKeepsAnEarlierRecordedTap() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        await crashDuringReset(world, at: "after.start", choice: .remove)
        world.relaunch()
        world.faults.arm("reset.status")

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(report.state, .pending, "whether the recorded tap started is unknown, so it is kept")
        XCTAssertEqual(report.blockers, [.coreUnavailable])
        XCTAssertEqual(world.choice.requested, .remove)
        XCTAssertTrue(world.staging.isHolding)
        XCTAssertEqual(world.staging.snapshotCalls, 1, "the keep tap takes no snapshot of its own; only the earlier tap did")

        let resumed = await world.makeService().resume()

        XCTAssertTrue(resumed.isComplete, "blockers: \(resumed.blockers)")
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
    }

    func testAChoiceThatCannotBeRecordedStartsNothing() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        world.faults.arm("intent.record")

        let report = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [.markerNotRecorded])
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertFalse(world.staging.isHolding, "a tap that was not recorded holds nothing back")
        XCTAssertEqual(world.staging.purgedCaptureIDs, [])
    }

    func testATapThatCannotBeRecordedWithdrawsAnEarlierTapSoNoOlderChoiceStaysArmed() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        await crashDuringReset(world, at: "before.start", choice: .remove)
        world.relaunch()
        XCTAssertEqual(world.choice.requested, .remove)
        world.faults.arm("intent.record")

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(report.state, .notStarted)
        XCTAssertEqual(report.blockers, [.markerNotRecorded])
        XCTAssertNil(world.choice.recordedRequest, "the earlier remove tap is withdrawn with the failed keep tap")
        XCTAssertFalse(world.staging.isHolding)

        let relaunched = await world.makeService().resume()

        XCTAssertEqual(relaunched.state, .notStarted)
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.credentials.removedReferences, [])
        XCTAssertEqual(world.core.revokedProfileVersions, [])
    }

    func testATapThatCannotBeRecordedNorWithdrawnIsReportedAsStillArmed() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        await crashDuringReset(world, at: "before.start", choice: .remove)
        world.relaunch()
        world.faults.arm("intent.record")
        world.faults.arm("intent.clear")

        let report = await world.makeService().reset(credentialChoice: .keep)

        XCTAssertEqual(report.state, .pending, "a tap that a restart would run is never hidden behind 'not started'")
        XCTAssertEqual(report.blockers, [.markerNotRecorded, .markerNotCleared])
        XCTAssertEqual(world.choice.requested, .remove)
        XCTAssertTrue(world.staging.isHolding)
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.credentials.removedReferences, [])
    }

    func testAChoiceThatCannotBeCarriedOverAfterTheCoreStartedIsRetriedBeforeAnythingIsRemoved() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.faults.arm("choice.record")

        let first = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(first.state, .pending)
        XCTAssertEqual(first.blockers, [.markerNotRecorded])
        XCTAssertEqual(world.choice.requested, .remove, "the request stays until the choice is carried over")
        XCTAssertEqual(world.credentials.removedReferences, [], "an unrecorded choice is never acted on")

        let relaunched = await world.makeService().resume()

        XCTAssertTrue(relaunched.isComplete)
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
        XCTAssertNil(world.choice.requested)
        XCTAssertEqual(world.core.currentGeneration, 1, "the pending generation was resumed, not replaced")
    }

    // MARK: Crash before the core confirms the reset

    /// Runs a reset until the armed crash point and abandons it there: nothing after that point runs, including the error
    /// paths that would clean up, exactly as in a killed process.
    private func crashDuringReset(
        _ world: ResetWorld, at point: String, choice: ResetCredentialChoice
    ) async {
        world.faults.arm("crash:" + point)
        let reached = expectation(description: "crash at \(point)")
        world.crash.notifyOnNextCrash { reached.fulfill() }
        let service = world.makeService()
        Task { _ = await service.reset(credentialChoice: choice) }
        await fulfillment(of: [reached], timeout: 30)
    }

    func testACrashBeforeTheCoreConfirmsTheResetIsFinishedInFullByTheNextLaunch() async throws {
        for priorGeneration in [false, true] {
            for point in ["staging.import", "staging.purge", "before.start", "after.start"] {
                let world = try makeResetWorld()
                try await seedVoiceAndTextCaptures(world)
                configureProfiles(world)
                if priorGeneration {
                    let prior = await world.makeService().reset(credentialChoice: .keep)
                    XCTAssertTrue(prior.isComplete)
                    try await world.seedItem("item-new", captureID: "cap-new", stagedRecord: .text)
                }
                let context = "crash at \(point), prior generation \(priorGeneration)"
                let generationBefore: Int64 = priorGeneration ? 1 : 0

                await crashDuringReset(world, at: point, choice: .remove)

                XCTAssertEqual(world.choice.requested, .remove, "the tap is recorded: \(context)")
                if point != "after.start" {
                    XCTAssertEqual(world.core.currentGeneration, generationBefore, "the core never started: \(context)")
                    XCTAssertFalse(world.choice.isOutstanding, "no native cleanup is outstanding for a reset that did not begin: \(context)")
                    XCTAssertFalse(world.choice.isRequested, "the credential choice is not armed before the core started: \(context)")
                    XCTAssertEqual(world.credentials.removedReferences, [], context)
                    XCTAssertEqual(world.core.revokedProfileVersions, [], context)
                }

                let resumed = await world.makeService().resume()

                XCTAssertEqual(resumed.state, .completed, "\(context): \(resumed.blockers)")
                XCTAssertEqual(
                    resumed.progress?.generation, priorGeneration ? 2 : 1,
                    "a prior completed generation is never reported as this reset: \(context)")
                XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"], context)
                XCTAssertNil(world.choice.requested, context)
                XCTAssertFalse(world.choice.isRequested, context)
                XCTAssertFalse(world.choice.isOutstanding, context)
                assertEverythingErased(world)
                if priorGeneration {
                    XCTAssertEqual(world.deletion.remnants(of: "cap-new"), [], "captures since the prior reset are erased: \(context)")
                }
            }
        }
    }

    func testACaptureMadeAfterTheTapSurvivesACrashBeforeTheCoreConfirmedTheReset() async throws {
        for seeded in [true, false] {
            for point in ["staging.import", "staging.purge", "before.start", "after.start"] {
                let world = try makeResetWorld()
                if seeded { try await seedVoiceAndTextCaptures(world) }
                world.makeImportedItemOnImport()
                world.staging.importable = ["cap-pre-tap", "cap-live", "cap-after-crash"]
                world.staging.stage("cap-pre-tap")
                world.staging.stage("cap-stuck")
                world.staging.onSnapshot = { [staging = world.staging] in staging.liveCapture("cap-live") }
                world.staging.onLiveImport = { [deletion = world.deletion, core = world.core] captureID in
                    deletion.core.addItem("item-\(captureID)", captureID: captureID, audioReference: nil)
                    core.register("item-\(captureID)")
                }
                let context = "crash at \(point), seeded \(seeded)"

                await crashDuringReset(world, at: point, choice: .keep)
                world.staging.onSnapshot = nil
                world.relaunch()
                XCTAssertTrue(world.staging.isHolding, "the relaunched gate holds while the tap is recorded: \(context)")
                world.staging.liveCapture("cap-after-crash")
                XCTAssertEqual(world.staging.liveImportedCaptureIDs, [], "nothing fresh imports before the core started: \(context)")

                let resumed = await world.makeService().resume()

                XCTAssertEqual(resumed.state, .completed, "\(context): \(resumed.blockers)")
                XCTAssertEqual(world.staging.snapshotCalls, 1, "resume uses the recorded list, never a new one: \(context)")
                XCTAssertEqual(
                    world.staging.heldSnapshots, [ResetStagingSnapshot(captureIDs: ["cap-pre-tap", "cap-stuck"])], context)
                XCTAssertFalse(world.staging.purgedCaptureIDs.contains("cap-live"), context)
                XCTAssertFalse(world.staging.importedCaptureIDs.contains("cap-live"), context)
                XCTAssertEqual(
                    Set(world.staging.liveImportedCaptureIDs), ["cap-live", "cap-after-crash"],
                    "post-tap captures import after the core recorded the reset: \(context)")
                for captureID in ["cap-live", "cap-after-crash"] {
                    XCTAssertNil(
                        world.deletion.core.status("item-\(captureID)", .removeAudio),
                        "\(captureID) is fresh-generation content, never tombstoned: \(context)")
                }
                XCTAssertTrue(world.deletion.core.isDeletionComplete("item-cap-pre-tap"), context)
                XCTAssertEqual(world.staging.staged, [], context)
                XCTAssertFalse(world.staging.isHolding, context)
                if seeded { assertEverythingErased(world) }
            }
        }
    }

    func testARestartAfterTheCoreCompletedAtStartNeverTombstonesACaptureMadeAfterTheTap() async throws {
        let world = try makeResetWorld()
        world.staging.onSnapshot = { [staging = world.staging] in staging.liveCapture("cap-live") }
        world.staging.onLiveImport = { [deletion = world.deletion, core = world.core] captureID in
            deletion.core.addItem("item-\(captureID)", captureID: captureID, audioReference: nil)
            core.register("item-\(captureID)")
        }

        await crashDuringReset(world, at: "after.start", choice: .keep)
        XCTAssertEqual(world.core.currentGeneration, 1)
        XCTAssertEqual(world.staging.liveImportedCaptureIDs, [], "the boundary is still held while the tap is recorded")
        world.relaunch()

        let resumed = await world.makeService().resume()

        XCTAssertEqual(resumed.state, .completed, "blockers: \(resumed.blockers)")
        XCTAssertEqual(world.staging.liveImportedCaptureIDs, ["cap-live"])
        XCTAssertNil(
            world.deletion.core.status("item-cap-live", .removeAudio),
            "starting the core again for the recorded tap finds no fresh capture below its boundary")
    }

    func testAResumeThatCannotPrepareStagingKeepsTheRecordedTapForTheNextLaunch() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        configureProfiles(world)
        world.staging.stage("cap-stuck")
        await crashDuringReset(world, at: "staging.import", choice: .remove)
        world.faults.arm("staging.purge")

        let blocked = await world.makeService().resume()

        XCTAssertEqual(blocked.state, .pending, "the recorded tap will run again, so it is not reported as not started")
        XCTAssertEqual(blocked.blockers, [.stagingPurgeIncomplete])
        XCTAssertEqual(world.choice.requested, .remove, "the user never saw a result, so the tap is kept")
        XCTAssertEqual(world.core.startCalls, 0)
        XCTAssertEqual(world.credentials.removedReferences, [])

        let retried = await world.makeService().resume()

        XCTAssertTrue(retried.isComplete)
        XCTAssertEqual(world.staging.staged, [])
        XCTAssertEqual(Set(world.credentials.removedReferences), ["ref-a", "ref-b"])
    }

    func testResumeAfterACrashBeforeTheCoreStartedDoesNotRemoveCredentialsOfAResetThatNeverRan() async throws {
        let world = try makeResetWorld()
        configureProfiles(world)
        await crashDuringReset(world, at: "before.start", choice: .remove)
        world.faults.arm("reset.start")

        let report = await world.makeService().resume()

        XCTAssertEqual(report.state, .pending, "the recorded tap is still armed for the next launch")
        XCTAssertEqual(report.blockers, [.coreUnavailable])
        XCTAssertEqual(world.choice.requested, .remove)
        XCTAssertEqual(world.credentials.removedReferences, [])
        XCTAssertEqual(world.core.revokedProfileVersions, [])
    }

    // MARK: Serialization

    func testOverlappingResetsAreSerialized() async throws {
        let world = try makeResetWorld()
        try await seedVoiceAndTextCaptures(world)
        let service = world.makeService()

        async let first = service.reset(credentialChoice: .keep)
        async let second = service.reset(credentialChoice: .keep)
        let reports = await [first, second]

        XCTAssertTrue(reports.allSatisfy { $0.isComplete })
        XCTAssertEqual(world.core.currentGeneration, 2, "the second call ran after the first finished, never interleaved with it")
        assertEverythingErased(world)
    }

    // MARK: Change signal

    func testOnlyACompletedResetPublishesAllItems() async throws {
        let blocked = try makeResetWorld()
        try await seedVoiceAndTextCaptures(blocked)
        blocked.faults.arm("fs.remove:cap-1.m4a")
        let pendingPublisher = RecordingChangePublisher()

        let pending = await blocked.makeService(changes: pendingPublisher).reset(credentialChoice: .keep)

        XCTAssertEqual(pending.state, .pending)
        XCTAssertTrue(pendingPublisher.changes.isEmpty, "a pending reset has not erased anything yet")

        let completedPublisher = RecordingChangePublisher()
        let completed = await blocked.makeService(changes: completedPublisher).resume()

        XCTAssertEqual(completed.state, .completed)
        XCTAssertEqual(completedPublisher.changes, [.allItems])
    }
}
