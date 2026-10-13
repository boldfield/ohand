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
        world.faults.arm("fs.remove:cap-1.m4a")

        let waiting = await world.makeService().reset(credentialChoice: .remove)

        XCTAssertEqual(waiting.state, .pending)
        XCTAssertTrue(waiting.blockers.contains(.coreResetPending), "blockers: \(waiting.blockers)")
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
        for point in ["staging.purge", "staging.purge.unreadable", "reset.start", "reset.status"] {
            let world = try makeResetWorld()
            world.staging.stage("cap-stuck")
            world.staging.onSnapshot = { [staging = world.staging] in staging.liveCapture("cap-live") }
            world.faults.arm(point)

            _ = await world.makeService().reset(credentialChoice: .keep)

            XCTAssertFalse(world.staging.isHolding, "the boundary is released after a fault at \(point)")
            XCTAssertEqual(world.staging.liveImportedCaptureIDs, ["cap-live"], "the waiting capture proceeds: \(point)")
        }
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
        let revokeIndex = try XCTUnwrap(world.log.index(of: "core.revoke:profile-a-v1"))
        let deleteIndex = try XCTUnwrap(world.log.index(of: "credentials.delete:ref-a"))
        XCTAssertLessThan(revokeIndex, deleteIndex, "a profile stops dispatching before its secret goes")
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
}
