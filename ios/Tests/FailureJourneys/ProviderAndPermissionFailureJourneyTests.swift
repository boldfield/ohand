import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Provider outage and denied or revoked permission journeys, driven through the app's production wiring over the real
/// core. Each journey asserts the save, processing and scheduling dimensions separately, through the status screen's
/// model and through the persisted rows. Restart, transcription and deletion journeys belong to the sibling suite.
@MainActor
final class ProviderAndPermissionFailureJourneyTests: FailureJourneyTestCase {
    func testProviderOutageKeepsTheSavedSourceRetryIsSourcePreservingAndCaptureStaysOpen() throws {
        try startRig(providerMode: .failing(.connectionFailed))
        unlock()
        try configureProvider()
        let status = makeStatusModel()

        let itemID = try captureSilently(Self.providerCaptureText)

        // The pinned job reaches the provider, the provider is down, and the job is put back for a later attempt.
        XCTAssertTrue(pump { self.sender.attemptCount > 0 }, "the pinned job was dispatched")
        XCTAssertTrue(
            pump { self.jobColumn("status", of: itemID) == "queued" && !self.jobColumn("failure_reason", of: itemID).isEmpty },
            "the outage left the job waiting: \(jobColumn("status", of: itemID))")
        _ = pump(timeout: 30) { !self.services.root.jobRunner.isDraining }

        // Save, processing and scheduling are reported independently.
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local", "the capture is saved")
        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText, "the saved source is the person's words")
        XCTAssertFalse(
            ["processed", "abstained", "uninterpreted"].contains(itemColumn("processing_state", of: itemID)),
            "processing did not finish: \(itemColumn("processing_state", of: itemID))")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM reminders WHERE item_id = '\(itemID)'"), "0", "no reminder was invented")
        XCTAssertTrue(
            center.addedRequests.allSatisfy { $0.identifier == PendingProcessingAlert.identifier },
            "nothing was scheduled with the OS for an item the provider has not interpreted")

        // The saved source is retrievable while processing waits.
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).get())
        let row = try XCTUnwrap(page.rows.first { $0.reference.value == itemID })
        XCTAssertEqual(row.text, Self.providerCaptureText)
        XCTAssertEqual(row.basis, .original)

        // The status screen says processing is behind the provider, not that anything was lost or is on the person.
        refresh(status)
        let processing = try fact(.processing, in: status)
        XCTAssertNotEqual(processing.headline, StatusText.processingIdleHeadline, "waiting work is not shown as idle")
        XCTAssertNotEqual(processing.headline, StatusText.processingUnknownHeadline)

        // Capture is available without triaging the waiting work.
        let secondID = try captureSilently("synthetic second note while the provider is down")
        XCTAssertNotEqual(secondID, itemID)
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")
        let capture = try fact(.capture, in: status)
        XCTAssertEqual(capture.tone, .good)
        XCTAssertEqual(capture.headline, StatusText.captureHeadline)

        // The provider comes back and the waiting job is due; retry runs it again from the same saved source.
        try sender.set(.replying(providerReply()))
        execute("UPDATE jobs SET next_attempt_at = '2020-01-01T00:00:00Z' WHERE status = 'queued'")
        let capturesBefore = scalar("SELECT COUNT(*) FROM captures")
        let itemsBefore = scalar("SELECT COUNT(*) FROM items")

        retry(status)
        XCTAssertTrue(status.retryRequested)
        XCTAssertTrue(
            pump { self.itemColumn("processing_state", of: itemID) == "processed" },
            "the retried job applied the provider's answer: \(itemColumn("processing_state", of: itemID))")

        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText, "retry never rewrote the saved source")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM captures"), capturesBefore, "retry created no capture")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM items"), itemsBefore, "retry created no item")
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
    }

    func testAProviderThatRefusesTheCredentialLeavesTheSourceAndAsksForProviderSettingsNotTriage() throws {
        try startRig(providerMode: .failing(.credentialUnavailable))
        unlock()
        try configureProvider()
        let status = makeStatusModel()

        let itemID = try captureSilently(Self.providerCaptureText)

        XCTAssertTrue(
            pump { self.jobColumn("failure_reason", of: itemID) == "unauthorized" },
            "the job ended for want of authorization: \(jobColumn("failure_reason", of: itemID))")
        _ = pump(timeout: 30) { !self.services.root.jobRunner.isDraining }

        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText)
        XCTAssertNotEqual(itemColumn("processing_state", of: itemID), "processed")
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "roofer")).get())
        XCTAssertEqual(page.rows.first { $0.reference.value == itemID }?.text, Self.providerCaptureText)

        // Recovery is in provider settings; the screen does not offer to re-drain an unchanged configuration.
        refresh(status)
        let processing = try fact(.processing, in: status)
        XCTAssertTrue(processing.actions.contains(.openProviderSettings), "headline: \(processing.headline)")
        XCTAssertFalse(processing.actions.contains(.retry))

        // Capture needs nothing resolved first, and the failed work stays untouched by the new save.
        let secondID = try captureSilently("synthetic note after the credential was refused")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")
        XCTAssertEqual(jobColumn("failure_reason", of: itemID), "unauthorized")
        XCTAssertEqual(savedText(of: itemID), Self.providerCaptureText)
        XCTAssertEqual(try fact(.capture, in: status).tone, .good)
    }

    func testDeniedNotificationPermissionFailsSchedulingHonestlyWhileSaveAndProcessingStayIntact() throws {
        try startRig(providerMode: .failing(.connectionFailed))
        unlock()
        center.authorizationStatus = .denied
        permissions.set(status: .denied, change: .revoked)
        let status = makeStatusModel()

        // The offline grammar reads this explicit reminder with no provider, a long way ahead of any real clock.
        let decadesOut = utcCalendar.date(byAdding: .year, value: 25, to: fixedNow)!
        let due = utcCalendar.date(bySettingHour: 9, minute: 0, second: 0, of: decadesOut)!
        let formatter = DateFormatter()
        formatter.calendar = utcCalendar
        formatter.timeZone = utcCalendar.timeZone
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss"
        let words = "remind me \(formatter.string(from: due)) to call mom"

        let itemID = try captureSilently(words)
        XCTAssertTrue(
            pump { self.itemColumn("item_type", of: itemID) == "action" }, "the saved capture is classified without a provider")

        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .foreground) }
        XCTAssertTrue(
            pump { self.scalar("SELECT schedule_state FROM reminders WHERE item_id = '\(itemID)'") == "schedule_failed" },
            "scheduling failed without permission: "
                + scalar("SELECT schedule_state FROM reminders WHERE item_id = '\(itemID)'"))

        // Three dimensions, three answers: saved, interpreted, and not scheduled.
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(savedText(of: itemID), words)
        XCTAssertEqual(itemColumn("item_type", of: itemID), "action", "interpretation is unaffected by the permission")
        XCTAssertEqual(scalar("SELECT request_state FROM reminders WHERE item_id = '\(itemID)'"), "resolved")
        XCTAssertTrue(
            center.addedRequests.allSatisfy { $0.identifier == PendingProcessingAlert.identifier },
            "nothing reached the OS without permission")
        XCTAssertEqual(sender.attemptCount, 0, "no provider was needed")

        refresh(status)
        let notifications = try fact(.notifications, in: status)
        XCTAssertEqual(notifications.headline, StatusText.notificationsRevokedHeadline)
        XCTAssertTrue(notifications.actions.contains(.openSettings))
        XCTAssertEqual(try fact(.capture, in: status).headline, StatusText.captureHeadline, "capture stays available")

        // The words stay retrievable and capture stays open while notifications are off.
        let page = try XCTUnwrap(search(RetrievalSearchRequest(query: "call mom")).get())
        XCTAssertEqual(page.rows.first { $0.reference.value == itemID }?.text, words)
        let secondID = try captureSilently("synthetic note while notifications are off")
        XCTAssertEqual(itemColumn("save_state", of: secondID), "saved_local")

        // Permission returns: the same reminder is scheduled once, and no other dimension moved.
        center.authorizationStatus = .authorized
        permissions.set(status: .granted, change: .restored)
        runMain { _ = try? await self.services.root.reconcileReminders(trigger: .settingsReturn) }
        XCTAssertTrue(
            pump { self.scalar("SELECT schedule_state FROM reminders WHERE item_id = '\(itemID)'") == "scheduled" },
            "the reminder was scheduled once permission returned")
        XCTAssertEqual(
            center.addedRequests.filter { $0.identifier != PendingProcessingAlert.identifier }.count, 1,
            "the reminder was installed exactly once")
        XCTAssertEqual(itemColumn("save_state", of: itemID), "saved_local")
        XCTAssertEqual(savedText(of: itemID), words)
        refresh(status)
        XCTAssertNotEqual(try fact(.notifications, in: status).headline, StatusText.notificationsRevokedHeadline)
    }
}
