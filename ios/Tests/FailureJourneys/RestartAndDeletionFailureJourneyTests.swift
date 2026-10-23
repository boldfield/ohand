import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Interrupted transcription, expired background execution, restart and deletion during processing, driven through the
/// app's production wiring over the real core and an on-disk store that survives each restart. Only the speech
/// recognizer, the background-time grant, the provider transport and the system notification center are stand-ins.
/// Provider outage and permission journeys belong to the sibling suite.
@MainActor
final class RestartAndDeletionFailureJourneyTests: FailureJourneyTestCase {
    private static let spokenWords = "call the roofer about the quarterly estimate"

    // MARK: helpers

    private func audioURL(of itemID: String) throws -> URL {
        let reference = scalar(
            "SELECT captures.audio_reference FROM captures JOIN items ON items.capture_id = captures.capture_id "
                + "WHERE items.item_id = '\(itemID)'")
        XCTAssertFalse(reference.isEmpty, "the voice capture keeps an audio reference")
        return storageRoot.appendingPathComponent(reference)
    }

    private func attachmentCount(of itemID: String) -> Int {
        Int(scalar("SELECT COUNT(*) FROM transcript_attachments WHERE item_id = '\(itemID)'")) ?? -1
    }

    private func transcriptionJobStatus(of itemID: String) -> String {
        scalar(
            "SELECT status FROM jobs WHERE item_id = '\(itemID)' AND job_type = 'transcription_attachment' "
                + "ORDER BY created_at DESC LIMIT 1")
    }

    private func reminderRequests() -> [NotificationCenterRequest] {
        center.addedRequests.filter { $0.identifier != PendingProcessingAlert.identifier }
    }

    private func pendingReminderIdentifiers() -> [String] {
        center.pendingIdentifiers.filter { $0 != PendingProcessingAlert.identifier }
    }

    /// The item is saved, its audio is on disk, and no transcript has been attached.
    private func assertSourceIntactAndUntranscribed(_ itemID: String, file: StaticString = #filePath, line: UInt = #line) throws {
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local", file: file, line: line)
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: try audioURL(of: itemID).path), "the audio is kept", file: file,
            line: line)
        XCTAssertEqual(attachmentCount(of: itemID), 0, "no transcript was attached", file: file, line: line)
        XCTAssertFalse(
            ["transcribed", "transcription_failed"].contains(itemColumn("transcription_state", of: itemID)),
            "transcription is not reported as finished: \(itemColumn("transcription_state", of: itemID))",
            file: file, line: line)
        XCTAssertEqual(savedText(of: itemID), "", "the person's capture holds no invented text", file: file, line: line)
    }

    // MARK: interrupted transcription, expired background time and restart

    func testExpiredBackgroundTimeStopsTranscriptionWithTheRecordingKeptAndARestartAttachesItOnce() throws {
        try startRig(providerMode: .failing(.connectionFailed), withRecognizer: true, withBackgroundGrant: true)
        unlock()
        let status = makeStatusModel()

        let itemID = try recordVoiceCapture()
        let firstRecognizer = try XCTUnwrap(recognizer)
        XCTAssertTrue(pump { firstRecognizer.callCount == 1 }, "recognition started on the saved recording")
        XCTAssertGreaterThan(try XCTUnwrap(grant).beginCount, 0, "the save asked for background time")

        // The person leaves the app; the open completion window lets the drain carry on, until the system takes the
        // background time back while the recognizer is still working.
        enterBackground()
        XCTAssertEqual(firstRecognizer.cancellationCount, 0, "the granted background time lets recognition continue")
        try XCTUnwrap(grant).fireExpiration()
        XCTAssertTrue(pump { firstRecognizer.cancellationCount == 1 }, "the recognizer was stopped")
        XCTAssertTrue(pump { !self.services.root.jobRunner.isDraining }, "the drain stopped at a checkpoint")

        // Interrupted work is back in the queue and the source is untouched.
        XCTAssertTrue(
            pump { self.transcriptionJobStatus(of: itemID) == "queued" },
            "the job waits for a later drain: \(transcriptionJobStatus(of: itemID))")
        try assertSourceIntactAndUntranscribed(itemID)
        XCTAssertTrue(reminderRequests().isEmpty, "no reminder effect came from unfinished processing")

        // The status screen says work is waiting, and capture needs nothing cleaned up first.
        refresh(status)
        let processing = try fact(.processing, in: status)
        XCTAssertNotEqual(processing.headline, StatusText.processingIdleHeadline, "waiting work is not shown as idle")
        let secondID = try captureSilently("synthetic note after the background time ran out")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")
        XCTAssertEqual(try fact(.capture, in: status).headline, StatusText.captureHeadline)

        // The app is relaunched and recognition now succeeds: exactly one transcript is attached to the same item.
        try restartRig(providerMode: .failing(.connectionFailed), withRecognizer: true)
        try XCTUnwrap(recognizer).answer(with: .transcript(text: Self.spokenWords, confidence: 0.9))
        unlock()
        XCTAssertTrue(
            pump { self.itemColumn("transcription_state", of: itemID) == "transcribed" },
            "the restart finished the transcription: \(itemColumn("transcription_state", of: itemID))")

        XCTAssertEqual(attachmentCount(of: itemID), 1, "the transcript is attached once")
        XCTAssertEqual(
            scalar("SELECT text FROM transcript_attachments WHERE item_id = '\(itemID)'"), Self.spokenWords)
        XCTAssertEqual(savedText(of: itemID), "", "the transcript never overwrites the capture")
        XCTAssertTrue(FileManager.default.fileExists(atPath: try audioURL(of: itemID).path), "the audio is preserved")
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local", "the other capture survived the restart")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM items"), "2", "the restart created no item")
        XCTAssertEqual(try XCTUnwrap(recognizer).callCount, 1, "the recording was recognized once after the restart")
    }

    func testAProcessKilledMidTranscriptionLeavesALeaseThatTheNextLaunchReclaims() throws {
        try startRig(providerMode: .failing(.connectionFailed), withRecognizer: true)
        unlock()

        let itemID = try recordVoiceCapture()
        let firstRecognizer = try XCTUnwrap(recognizer)
        XCTAssertTrue(pump { firstRecognizer.callCount == 1 }, "recognition started on the saved recording")
        XCTAssertEqual(transcriptionJobStatus(of: itemID), "running")

        // A kill gives the process no chance to return its job: the row is left leased, and the lease runs out.
        stopRig()
        execute(
            "UPDATE jobs SET status = 'running', lease_expires_at = '2020-01-01T00:00:00Z' "
                + "WHERE item_id = '\(itemID)' AND job_type = 'transcription_attachment'")
        XCTAssertEqual(attachmentCount(of: itemID), 0)
        XCTAssertEqual(savedText(of: itemID), "")

        try launchRig(providerMode: .failing(.connectionFailed), withRecognizer: true)
        try XCTUnwrap(recognizer).answer(with: .transcript(text: Self.spokenWords, confidence: 0.9))
        unlock()

        XCTAssertTrue(
            pump { self.itemColumn("transcription_state", of: itemID) == "transcribed" },
            "the expired lease was reclaimed: \(itemColumn("transcription_state", of: itemID))")
        XCTAssertEqual(attachmentCount(of: itemID), 1)
        XCTAssertEqual(transcriptionJobStatus(of: itemID), "completed")
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertTrue(FileManager.default.fileExists(atPath: try audioURL(of: itemID).path))
    }

    func testAcknowledgedTextSurvivesARestartDuringAProviderOutageAndIsProcessedFromTheSameSource() throws {
        try startRig(providerMode: .failing(.connectionFailed))
        unlock()
        try configureProvider()
        let itemID = try captureSilently(Self.providerCaptureText)
        XCTAssertTrue(
            pump { self.jobColumn("status", of: itemID) == "queued" && !self.jobColumn("failure_reason", of: itemID).isEmpty },
            "the outage left the job waiting")

        // Restart with the provider still down: nothing is lost, and capture is open at once.
        try restartRig(providerMode: .failing(.connectionFailed))
        unlock()
        let status = makeStatusModel()
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText)
        XCTAssertNotEqual(itemColumn("processing_state", of: itemID), "processed")
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).get())
        XCTAssertEqual(page.rows.first { $0.reference.value == itemID }?.text, Self.providerCaptureText)
        refresh(status)
        XCTAssertNotEqual(try fact(.processing, in: status).headline, StatusText.processingIdleHeadline)
        let secondID = try captureSilently("synthetic note right after the restart")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")

        // The provider returns and the job is due: the next launch processes the original capture, not a copy.
        stopRig()
        execute("UPDATE jobs SET next_attempt_at = '2020-01-01T00:00:00Z' WHERE status = 'queued'")
        try launchRig(providerMode: .replying(providerReply()))
        unlock()
        XCTAssertTrue(
            pump { self.itemColumn("processing_state", of: itemID) == "processed" },
            "the restarted app processed the saved capture: \(itemColumn("processing_state", of: itemID))")
        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText)
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM captures WHERE text = '\(Self.providerCaptureText)'"), "1")
    }

    // MARK: deletion during processing

    func testADeletionDuringTranscriptionIsNotRestoredByTheLateTranscript() throws {
        try startRig(providerMode: .failing(.connectionFailed), withRecognizer: true)
        unlock()

        let itemID = try recordVoiceCapture()
        let activeRecognizer = try XCTUnwrap(recognizer)
        XCTAssertTrue(pump { activeRecognizer.callCount == 1 }, "recognition started on the saved recording")
        let audio = try audioURL(of: itemID)
        let revision = try XCTUnwrap(Int(itemColumn("revision", of: itemID)))

        let root = services.root
        _ = try call { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: revision) }
        XCTAssertEqual(itemColumn("lifecycle_state", of: itemID), "deleted")
        XCTAssertTrue(pump { !FileManager.default.fileExists(atPath: audio.path) }, "the audio was removed with the item")

        // The recognizer answers after the deletion: its late result must find nothing to attach to.
        activeRecognizer.answer(with: .transcript(text: Self.spokenWords, confidence: 0.9))
        settleBackgroundWork()
        XCTAssertTrue(
            pump { !["queued", "running"].contains(self.transcriptionJobStatus(of: itemID)) },
            "the late job was retired: \(transcriptionJobStatus(of: itemID))")

        XCTAssertEqual(itemColumn("lifecycle_state", of: itemID), "deleted", "the late transcript did not restore the item")
        XCTAssertEqual(attachmentCount(of: itemID), 0, "no transcript was attached to a deleted item")
        XCTAssertEqual(savedText(of: itemID), "")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM transcript_attachments WHERE text LIKE '%roofer%'"), "0")
        XCTAssertFalse(FileManager.default.fileExists(atPath: audio.path))
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).get())
        XCTAssertFalse(page.rows.contains { $0.reference.value == itemID }, "the deleted words are not retrievable")
        XCTAssertTrue(reminderRequests().isEmpty)

        // A restart does not bring it back either, and the next recording is unaffected.
        try restartRig(providerMode: .failing(.connectionFailed), withRecognizer: true)
        unlock()
        settleBackgroundWork()
        XCTAssertEqual(itemColumn("lifecycle_state", of: itemID), "deleted")
        XCTAssertEqual(attachmentCount(of: itemID), 0)
        XCTAssertEqual(try XCTUnwrap(recognizer).callCount, 0, "a deleted recording is never recognized again")
        let secondID = try captureSilently("synthetic note after the deletion")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")
    }

    func testADeletedItemsScheduledReminderIsCancelledAndNeverReinstalledByLaterPasses() throws {
        try startRig(providerMode: .failing(.connectionFailed))
        unlock()
        let decadesOut = utcCalendar.date(byAdding: .year, value: 25, to: fixedNow)!
        let due = utcCalendar.date(bySettingHour: 9, minute: 0, second: 0, of: decadesOut)!
        let formatter = DateFormatter()
        formatter.calendar = utcCalendar
        formatter.timeZone = utcCalendar.timeZone
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        let words = "remind me \(formatter.string(from: due)) to call mom"

        let itemID = try captureSilently(words)
        XCTAssertTrue(pump { self.itemColumn("item_type", of: itemID) == "action" }, "the capture is classified")
        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .foreground) }
        XCTAssertTrue(
            pump { self.scalar("SELECT schedule_state FROM reminders WHERE item_id = '\(itemID)'") == "scheduled" },
            "the reminder was scheduled")
        XCTAssertEqual(pendingReminderIdentifiers().count, 1)
        let installedBeforeDeletion = reminderRequests().count

        let root = services.root
        let revision = try XCTUnwrap(Int(itemColumn("revision", of: itemID)))
        _ = try call { try await root.itemDeletion.delete(itemID: itemID, expectedRevision: revision) }
        XCTAssertEqual(itemColumn("lifecycle_state", of: itemID), "deleted")
        XCTAssertTrue(pump { self.pendingReminderIdentifiers().isEmpty }, "the pending notification was removed")

        // Late passes, in this run and after a restart, must not put the notification back.
        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .foreground) }
        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .explicit) }
        settleBackgroundWork()
        XCTAssertTrue(pendingReminderIdentifiers().isEmpty, "a late pass did not reinstall the reminder")
        XCTAssertEqual(reminderRequests().count, installedBeforeDeletion, "no new request was handed to the OS")

        try restartRig(providerMode: .failing(.connectionFailed))
        unlock()
        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .launch) }
        settleBackgroundWork()
        XCTAssertTrue(pendingReminderIdentifiers().isEmpty, "the restart did not reinstall the reminder")
        XCTAssertTrue(reminderRequests().isEmpty, "the new run installed nothing for the deleted item")
        XCTAssertEqual(itemColumn("lifecycle_state", of: itemID), "deleted")
        XCTAssertEqual(savedText(of: itemID), "", "the deleted words are erased")
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "call mom")).get())
        XCTAssertFalse(page.rows.contains { $0.reference.value == itemID })
        let secondID = try captureSilently("synthetic note after the reminder was deleted")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")
    }
}
