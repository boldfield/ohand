import XCTest
@testable import OhAndServices

/// Behavior of per-item deletion cleanup against a durable fake core, real protected-storage files and the production
/// `NotificationBridge` over a fake OS center. Each interruption is an injected failure at one step; "reopening" builds a
/// new service and bridge over the same disk, core and notification state. These tests exercise `DeletionCleanupCore`
/// through a fake that follows the L01 contract, so each fault point can be injected exactly; the production client over
/// the real Rust core is covered by `DeletionCleanupCoreIntegrationTests`.
final class DeletionCleanupServiceTests: DeletionTestCase {
    private let itemID = "item-1"
    private let captureID = "cap-1"

    private func seedFullItem(_ world: DeletionWorld) async throws {
        try await world.seedItem(
            itemID, captureID: captureID, finalizedAudio: true, stagedRecord: .audio, interruptedRecordWrite: true,
            reminders: ["reminder-1": [1, 2, 3]])
        world.core.recordDeletion(itemID)
    }

    private func seedBystander(_ world: DeletionWorld) async throws {
        try await world.seedItem(
            "item-2", captureID: "cap-2", finalizedAudio: true, stagedRecord: .text, reminders: ["reminder-2": [1]])
    }

    private func assertNoRemnants(_ world: DeletionWorld, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(world.remnants(of: captureID), [], "no file of the deleted capture remains", file: file, line: line)
        XCTAssertEqual(
            world.pendingNotificationIdentifiers(ofReminder: "reminder-1"), [],
            "no notification of the deleted item stays active", file: file, line: line)
        XCTAssertFalse(world.core.isSearchable(itemID), file: file, line: line)
        XCTAssertNil(world.core.readableText(itemID), file: file, line: line)
    }

    private func assertBystanderUntouched(_ world: DeletionWorld, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-2.m4a"), file: file, line: line)
        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-2.json"), file: file, line: line)
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: "reminder-2"), ["reminder-2#1"], file: file, line: line)
    }

    // MARK: Happy path

    func testCleanupRemovesEveryEffectCompletesDeletionAndLeavesOtherItemsAlone() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        try await seedBystander(world)
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: "reminder-1").count, 3)

        let report = await world.makeService().reconcile()

        XCTAssertTrue(report.converged)
        XCTAssertEqual(report.items.map { $0.itemID }, [itemID])
        XCTAssertEqual(
            report.items.first?.steps,
            [
                DeletionStepResult(kind: .cancelNotifications, outcome: .completed),
                DeletionStepResult(kind: .clearIngress, outcome: .completed),
                DeletionStepResult(kind: .removeAudio, outcome: .completed),
            ])
        XCTAssertTrue(world.core.isDeletionComplete(itemID))
        XCTAssertNil(world.core.audioReference(itemID), "the audio reference is released once removal is recorded")
        assertNoRemnants(world)
        assertBystanderUntouched(world)
    }

    func testRepeatedReconcileAfterConvergenceDoesNothing() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        let service = world.makeService()
        _ = await service.reconcile()
        let removalsBefore = world.center.removedIdentifiers.count
        let mutationsBefore = world.core.mutationCount

        let second = await service.reconcile()
        let reopened = await world.makeService().reconcile()

        XCTAssertEqual(second, DeletionReconcileReport(listingFailure: nil, items: []))
        XCTAssertEqual(reopened, DeletionReconcileReport(listingFailure: nil, items: []))
        XCTAssertEqual(world.center.removedIdentifiers.count, removalsBefore)
        XCTAssertEqual(world.core.mutationCount, mutationsBefore)
    }

    func testItemWithoutAudioOrReminderCompletesItsRemainingRows() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false, stagedRecord: .text)
        world.core.recordDeletion(itemID)

        let result = await world.makeService().cleanUp(itemID: itemID)

        XCTAssertTrue(result.isSettled)
        XCTAssertEqual(result.steps.map { $0.kind }, [.cancelNotifications, .clearIngress])
        XCTAssertTrue(world.core.isDeletionComplete(itemID))
        assertNoRemnants(world)
    }

    // MARK: Interruptions

    private static let interruptionPoints = [
        "core.itemsWithPendingCleanup",
        "core.pendingCleanup",
        "core.reminderIdentifiers",
        "notifier.cancel",
        "notifier.cancel.afterEffect",
        "core.recordCancelled",
        "notifier.pending",
        "core.complete.cancel_notifications",
        "core.cleanupTarget",
        "fs.read:cap-1.json",
        "fs.remove:cap-1-recording.m4a",
        "fs.remove:cap-1.m4a",
        "fs.remove:cap-1.json.incomplete",
        "fs.remove:cap-1.json",
        "core.complete.clear_ingress",
        "fs.entryNames",
        "core.complete.remove_audio",
    ]

    func testReopenAfterEachInterruptionConvergesWithoutRemnants() async throws {
        for point in Self.interruptionPoints {
            let world = try makeWorld()
            try await seedFullItem(world)
            try await seedBystander(world)
            world.faults.arm(point)

            let interrupted = await world.makeService().reconcile()

            XCTAssertEqual(world.faults.firedPoints, [point], "the interruption at \(point) happened")
            XCTAssertFalse(interrupted.converged, "an interrupted pass is not reported as converged: \(point)")
            XCTAssertFalse(world.core.isDeletionComplete(itemID), "deletion is not complete while a step is undone: \(point)")
            XCTAssertFalse(world.core.isSearchable(itemID), "never searchable while cleanup is interrupted: \(point)")
            XCTAssertNil(world.core.readableText(itemID), point)

            let reopened = await world.makeService().reconcile()

            XCTAssertTrue(reopened.converged, "reopen converges after \(point)")
            XCTAssertTrue(world.core.isDeletionComplete(itemID), point)
            assertNoRemnants(world)
            assertBystanderUntouched(world)
        }
    }

    func testAConsecutiveSeriesOfInterruptionsNeverExposesContentAndStillConverges() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        try await seedBystander(world)

        for point in Self.interruptionPoints {
            world.faults.arm(point)
            _ = await world.makeService().reconcile()
            XCTAssertFalse(world.core.isSearchable(itemID), "never searchable after an interruption at \(point)")
            XCTAssertNil(world.core.readableText(itemID), point)
        }

        world.faults.disarmAll()
        let finalReport = await world.makeService().reconcile()
        XCTAssertTrue(finalReport.converged)
        XCTAssertTrue(world.core.isDeletionComplete(itemID))
        assertNoRemnants(world)
        assertBystanderUntouched(world)
    }

    func testFailedNotificationCancelLeavesOnlyThatRowPending() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        world.faults.arm("notifier.cancel")

        let first = await world.makeService().reconcile()

        XCTAssertEqual(
            first.items.first?.steps,
            [
                DeletionStepResult(kind: .cancelNotifications, outcome: .retryLater(.notificationUnavailable)),
                DeletionStepResult(kind: .clearIngress, outcome: .completed),
                DeletionStepResult(kind: .removeAudio, outcome: .completed),
            ])
        XCTAssertEqual(world.core.status(itemID, .cancelNotifications), .pending)
        XCTAssertEqual(world.core.status(itemID, .clearIngress), .completed)

        let reopened = await world.makeService().reconcile()
        XCTAssertTrue(reopened.converged)
        assertNoRemnants(world)
    }

    func testCancelNotRecordedInTheCoreKeepsTheNotificationRowPending() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        world.faults.arm("core.recordCancelled")

        let first = await world.makeService().reconcile()
        XCTAssertEqual(first.items.first?.steps.first, DeletionStepResult(kind: .cancelNotifications, outcome: .retryLater(.coreUnavailable)))
        XCTAssertEqual(world.core.status(itemID, .cancelNotifications), .pending, "the core refuses completion until cancels are recorded")

        let reopened = await world.makeService().reconcile()
        XCTAssertTrue(reopened.converged)
        assertNoRemnants(world)
    }

    func testLateInstallUnderTheItemsReminderIsCancelledButOtherRequestsAreLeftAlone() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        try await seedBystander(world)
        world.center.installForeignRequest(identifier: "reminder-1#9", dueInstant: world.currentTime.addingTimeInterval(7200))
        world.center.installForeignRequest(identifier: "not-a-core-identifier", dueInstant: world.currentTime.addingTimeInterval(7200))

        let report = await world.makeService().reconcile()

        XCTAssertTrue(report.converged)
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: "reminder-1"), [])
        XCTAssertTrue(world.center.pendingIdentifiers.contains("not-a-core-identifier"))
        assertBystanderUntouched(world)
    }

    // MARK: Files

    func testLockedStagingRecordKeepsItsAudioUntilItCanBeRead() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        world.faults.arm("fs.read:cap-1.json")

        _ = await world.makeService().reconcile()

        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-1.json"), "an unreadable record is not treated as absent")
        XCTAssertTrue(world.exists(.ingressInProgressAudio, "cap-1-recording.m4a"))

        let reopened = await world.makeService().reconcile()
        XCTAssertTrue(reopened.converged)
        assertNoRemnants(world)
    }

    func testRecordIsRemovedLastSoAnInterruptedRecordStillNamesItsAudio() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        world.faults.arm("fs.remove:cap-1.json")

        _ = await world.makeService().reconcile()

        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-1.json"))
        XCTAssertFalse(world.exists(.ingressInProgressAudio, "cap-1-recording.m4a"))
        XCTAssertFalse(world.exists(.finalizedAudio, "cap-1.m4a"))

        let reopened = await world.makeService().reconcile()
        XCTAssertTrue(reopened.converged)
        assertNoRemnants(world)
    }

    func testCorruptStagingRecordIsStillRemoved() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false)
        try Data("not json".utf8).write(to: world.url(.ingressStagingRecords, "cap-1.json"))
        world.core.recordDeletion(itemID)

        let report = await world.makeService().reconcile()

        XCTAssertTrue(report.converged)
        XCTAssertEqual(world.remnants(of: captureID), [])
    }

    func testAudioBoundToTheCaptureIsRemovedBeyondTheReferencedFile() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: true)
        try Data("synthetic audio".utf8).write(to: world.url(.finalizedAudio, "cap-1.wav"))
        try Data("synthetic audio".utf8).write(to: world.url(.finalizedAudio, "cap-10.m4a"))
        world.core.recordDeletion(itemID)

        let report = await world.makeService().reconcile()

        XCTAssertTrue(report.converged)
        XCTAssertFalse(world.exists(.finalizedAudio, "cap-1.m4a"))
        XCTAssertFalse(world.exists(.finalizedAudio, "cap-1.wav"))
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-10.m4a"), "a different capture's audio is never matched")
    }

    func testUnresolvableAudioReferenceFailsTerminallyAfterBoundFilesAreRemoved() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: true)
        world.core.addItem(itemID, captureID: captureID, audioReference: "Elsewhere/cap-1.m4a")
        world.core.recordDeletion(itemID)

        let service = world.makeService()
        let report = await service.reconcile()

        XCTAssertFalse(report.converged)
        XCTAssertEqual(
            report.items.first?.steps.last,
            DeletionStepResult(kind: .removeAudio, outcome: .failedPermanently(.audioReferenceUnresolvable)))
        XCTAssertEqual(world.core.status(itemID, .removeAudio), .failed)
        XCTAssertEqual(world.core.status(itemID, .clearIngress), .completed)
        XCTAssertFalse(world.exists(.finalizedAudio, "cap-1.m4a"))

        XCTAssertEqual(report.terminalFailures, [DeletionTerminalFailure(itemID: itemID, kind: .removeAudio)])

        let mutationsBefore = world.core.mutationCount
        for attempt in ["same service", "reopened service"] {
            let again = await (attempt == "same service" ? service : world.makeService()).reconcile()
            XCTAssertEqual(again.items, [], "a terminal failure is not retried: \(attempt)")
            XCTAssertEqual(
                again.terminalFailures, [DeletionTerminalFailure(itemID: itemID, kind: .removeAudio)],
                "a durable failure stays visible: \(attempt)")
            XCTAssertFalse(again.converged, "a durable failure never reports convergence: \(attempt)")
        }
        XCTAssertEqual(world.core.mutationCount, mutationsBefore)
    }

    func testUnreadableTerminalFailureListingIsUnknownNeverConverged() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        world.faults.arm("core.terminalFailures")

        let interrupted = await world.makeService().reconcile()

        XCTAssertEqual(world.faults.firedPoints, ["core.terminalFailures"])
        XCTAssertEqual(interrupted.listingFailure, .coreUnavailable)
        XCTAssertFalse(interrupted.converged, "an unreadable failure list is unknown, not empty")
        XCTAssertTrue(world.core.isDeletionComplete(itemID), "every effect finished before the listing failed")

        let reopened = await world.makeService().reconcile()
        XCTAssertTrue(reopened.converged)
    }

    func testCorruptStagingRecordDoesNotHideItsInProgressAudio() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false)
        try Data("not a record".utf8).write(to: world.url(.ingressStagingRecords, "cap-1.json"))
        let audio = Data("synthetic audio".utf8)
        for name in ["cap-1.wav", "cap-1-continued.wav", "cap-1-joined.wav", "cap-1-2.wav", "cap-10.wav"] {
            try audio.write(to: world.url(.ingressInProgressAudio, name))
        }
        world.core.recordDeletion(itemID)

        let report = await world.makeService().reconcile()

        XCTAssertTrue(report.converged)
        XCTAssertEqual(
            world.remnants(of: captureID).filter { !$0.contains("cap-1-2") && !$0.contains("cap-10") }, [],
            "the corrupt record and every in-progress file bound to the capture are gone")
        XCTAssertTrue(world.exists(.ingressInProgressAudio, "cap-1-2.wav"), "another capture's recording is never matched")
        XCTAssertTrue(world.exists(.ingressInProgressAudio, "cap-10.wav"), "another capture's recording is never matched")
    }

    func testAudioNamedOnlyByAnUnreadableRecordIsRemovedAcrossInterruptions() async throws {
        let damagedRecords: [(label: String, json: String)] = [
            ("missing context", """
            {"formatVersion":1,"captureID":"cap-1","audio":{"inProgressFileName":"opaque-take.m4a","finalizedFileName":"cap-1.m4a"}}
            """),
            ("unsupported version", """
            {"formatVersion":99,"captureID":"cap-1","audio":{"inProgressFileName":"opaque-take.m4a","finalizedFileName":"cap-1.m4a"},"context":{"captureInstant":"2026-10-08T09:30:00Z","timezoneID":"UTC","utcOffsetMinutes":0,"locale":"en_US","calendar":"gregorian","itemScope":"personal","routeID":"route-personal","entryLocked":false,"createdAt":"2026-10-08T09:30:01Z"}}
            """),
        ]
        for damaged in damagedRecords {
            for point in ["fs.remove:opaque-take.m4a", "fs.remove:cap-1.json"] {
                let world = try makeWorld()
                try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false)
                try Data(damaged.json.utf8).write(to: world.url(.ingressStagingRecords, "cap-1.json"))
                try Data("synthetic audio".utf8).write(to: world.url(.ingressInProgressAudio, "opaque-take.m4a"))
                world.core.recordDeletion(itemID)
                world.faults.arm(point)

                let interrupted = await world.makeService().reconcile()
                XCTAssertFalse(interrupted.converged, "\(damaged.label) at \(point)")

                let reopened = await world.makeService().reconcile()

                XCTAssertTrue(reopened.converged, "\(damaged.label) at \(point)")
                XCTAssertFalse(world.exists(.ingressInProgressAudio, "opaque-take.m4a"), "\(damaged.label) at \(point)")
                XCTAssertEqual(world.remnants(of: captureID), [], "\(damaged.label) at \(point)")
            }
        }
    }

    func testDamagedRecordNeverPointsCleanupAtAnotherCapturesAudio() async throws {
        let context = """
        "context":{"captureInstant":"2026-10-08T09:30:00Z","timezoneID":"UTC","utcOffsetMinutes":0,"locale":"en_US",\
        "calendar":"gregorian","itemScope":"personal","routeID":"route-personal","entryLocked":false,\
        "createdAt":"2026-10-08T09:30:01Z"}
        """
        let damagedRecords: [(label: String, json: (String) -> String)] = [
            ("invalid: finalized name bound to another capture", { inProgress in """
            {"formatVersion":1,"captureID":"cap-1","audio":{"inProgressFileName":"\(inProgress)",\
            "finalizedFileName":"cap-2.m4a"},\(context)}
            """ }),
            ("corrupt: body names another capture", { inProgress in """
            {"formatVersion":1,"captureID":"cap-2","audio":{"inProgressFileName":"\(inProgress)",\
            "finalizedFileName":"cap-2.m4a"},\(context)}
            """ }),
            ("missing context", { inProgress in """
            {"formatVersion":1,"captureID":"cap-1","audio":{"inProgressFileName":"\(inProgress)",\
            "finalizedFileName":"cap-2.m4a"}}
            """ }),
        ]
        // A live recording bound to cap-2, the audio cap-2's own record claims, and a recording voice recovery
        // attributes to another capture.
        let bystanderInProgress = ["cap-2.wav", "cap-2-recording.m4a", "opaque-take.wav"]
        for damaged in damagedRecords {
            for named in bystanderInProgress {
                let label = "\(damaged.label) naming \(named)"
                let world = try makeWorld()
                try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false)
                try await world.seedItem("item-2", captureID: "cap-2", finalizedAudio: true, stagedRecord: .audio)
                for name in ["cap-2.wav", "opaque-take.wav"] {
                    try Data("synthetic audio".utf8).write(to: world.url(.ingressInProgressAudio, name))
                }
                try Data(damaged.json(named).utf8).write(to: world.url(.ingressStagingRecords, "cap-1.json"))
                world.core.recordDeletion(itemID)

                let report = await world.makeService().reconcile()

                XCTAssertTrue(report.converged, label)
                XCTAssertEqual(world.remnants(of: captureID), [], label)
                XCTAssertTrue(world.exists(.finalizedAudio, "cap-2.m4a"), "another capture's finalized audio: \(label)")
                XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-2.json"), label)
                for name in bystanderInProgress {
                    XCTAssertTrue(world.exists(.ingressInProgressAudio, name), "\(name) survives: \(label)")
                }
            }
        }
    }

    func testUnreadableOtherRecordKeepsAnUnboundNameUntilItsClaimIsKnown() async throws {
        let world = try makeWorld()
        try await world.seedItem(itemID, captureID: captureID, finalizedAudio: false)
        try await world.seedItem("item-2", captureID: "cap-2", finalizedAudio: false, stagedRecord: .text)
        try Data("""
        {"formatVersion":1,"captureID":"cap-1","audio":{"inProgressFileName":"opaque-take.m4a","finalizedFileName":"cap-1.m4a"}}
        """.utf8).write(to: world.url(.ingressStagingRecords, "cap-1.json"))
        try Data("synthetic audio".utf8).write(to: world.url(.ingressInProgressAudio, "opaque-take.m4a"))
        world.core.recordDeletion(itemID)
        world.faults.arm("fs.read:cap-2.json")

        let interrupted = await world.makeService().reconcile()

        XCTAssertFalse(interrupted.converged, "an unknown claim is not treated as no claim")
        XCTAssertTrue(world.exists(.ingressInProgressAudio, "opaque-take.m4a"))
        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-1.json"), "the record still names the audio left to remove")

        let reopened = await world.makeService().reconcile()

        XCTAssertTrue(reopened.converged)
        XCTAssertFalse(world.exists(.ingressInProgressAudio, "opaque-take.m4a"))
        XCTAssertEqual(world.remnants(of: captureID), [])
        XCTAssertTrue(world.exists(.ingressStagingRecords, "cap-2.json"))
    }

    func testCaptureIdentifierThatCouldNamePathIsNeverUsedForFileOperations() async throws {
        let world = try makeWorld()
        let sentinel = world.layout.rootDirectory.appendingPathComponent("x.json")
        try Data("outside the stores".utf8).write(to: sentinel)
        world.core.addItem(itemID, captureID: "../x", audioReference: "FinalizedAudio/x.m4a")
        world.core.recordDeletion(itemID)

        let report = await world.makeService().reconcile()

        let steps = try XCTUnwrap(report.items.first?.steps)
        XCTAssertTrue(steps.contains(DeletionStepResult(kind: .clearIngress, outcome: .failedPermanently(.unsafeCaptureIdentifier))))
        XCTAssertTrue(steps.contains(DeletionStepResult(kind: .removeAudio, outcome: .failedPermanently(.unsafeCaptureIdentifier))))
        XCTAssertTrue(FileManager.default.fileExists(atPath: sentinel.path))
    }

    // MARK: Scoping and concurrency

    func testCleanUpHandlesOnlyTheNamedItem() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        try await world.seedItem(
            "item-2", captureID: "cap-2", finalizedAudio: true, stagedRecord: .text, reminders: ["reminder-2": [1]])
        world.core.recordDeletion("item-2")

        let result = await world.makeService().cleanUp(itemID: itemID)

        XCTAssertTrue(result.isSettled)
        XCTAssertTrue(world.core.isDeletionComplete(itemID))
        XCTAssertFalse(world.core.isDeletionComplete("item-2"))
        XCTAssertTrue(world.exists(.finalizedAudio, "cap-2.m4a"))
        XCTAssertEqual(world.pendingNotificationIdentifiers(ofReminder: "reminder-2"), ["reminder-2#1"])
    }

    func testConcurrentPassesAreSerializedSoNoEffectRunsTwice() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        let service = world.makeService()

        async let first = service.reconcile()
        async let second = service.reconcile()
        let reports = await [first, second]

        XCTAssertEqual(reports.filter { !$0.items.isEmpty }.count, 1, "the second pass finds nothing left to do")
        XCTAssertTrue(reports.allSatisfy { $0.converged })
        let removed = world.center.removedIdentifiers
        XCTAssertEqual(Set(removed).count, removed.count, "no request was removed twice")
        assertNoRemnants(world)
    }

    func testCancelledPassLeavesEveryRowPendingAndTheNextPassConverges() async throws {
        let world = try makeWorld()
        try await seedFullItem(world)
        let service = world.makeService()

        let task = Task { () -> DeletionReconcileReport in
            while !Task.isCancelled { await Task.yield() }
            return await service.reconcile()
        }
        task.cancel()
        let cancelled = await task.value

        XCTAssertEqual(cancelled.listingFailure, .cancelled)
        XCTAssertFalse(cancelled.converged)
        XCTAssertTrue(world.center.removedIdentifiers.isEmpty, "a cancelled pass performs no effect")
        XCTAssertFalse(world.core.isSearchable(itemID))

        let next = await world.makeService().reconcile()
        XCTAssertTrue(next.converged)
        assertNoRemnants(world)
    }
}
