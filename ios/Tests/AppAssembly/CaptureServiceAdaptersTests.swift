import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

final class CaptureServiceAdaptersTests: XCTestCase {
    private let capturedAt = Date(timeIntervalSince1970: 1_800_000_000)

    func testTextSubmissionReachesTheRootSaveWithItsOwnFields() {
        var saved: [(String, String, Date)] = []
        let ingress = RootTextCaptureIngress(save: { captureID, text, date, completion in
            saved.append((captureID, text, date))
            completion(.keptOnDevice(captureID: captureID))
        })

        var outcome: TextCaptureOutcome?
        ingress.submitText(
            TextCaptureSubmission(captureID: "capture-1", text: "buy milk", capturedAt: capturedAt)
        ) { outcome = $0 }

        XCTAssertEqual(saved.count, 1)
        XCTAssertEqual(saved.first?.0, "capture-1")
        XCTAssertEqual(saved.first?.1, "buy milk")
        XCTAssertEqual(saved.first?.2, capturedAt)
        XCTAssertEqual(outcome, .keptOnDevice(captureID: "capture-1"))
    }

    func testEverySaveOutcomeKeepsItsMeaning() {
        XCTAssertEqual(
            RootTextCaptureIngress.outcome(for: .saved(captureID: "c", itemID: "i", alreadySaved: true)),
            .saved(captureID: "c", itemID: "i", alreadySaved: true))
        XCTAssertEqual(
            RootTextCaptureIngress.outcome(for: .saved(captureID: "c", itemID: "i", alreadySaved: false)),
            .saved(captureID: "c", itemID: "i", alreadySaved: false))
        XCTAssertEqual(RootTextCaptureIngress.outcome(for: .keptOnDevice(captureID: "c")), .keptOnDevice(captureID: "c"))
        XCTAssertEqual(RootTextCaptureIngress.outcome(for: .notStaged(captureID: "c")), .notStaged(captureID: "c"))
        XCTAssertEqual(RootTextCaptureIngress.outcome(for: .itemDeleted(captureID: "c")), .itemDeleted(captureID: "c"))
    }

    // MARK: status

    func testAReadStatusIsCopiedFieldForFieldAndNeverClaimsAReminder() throws {
        let report = try Self.report(
            processing: "processed", transcription: "not_applicable", request: "resolved", schedule: "scheduled",
            reason: nil)

        guard case .status(let status) = RootCaptureProgressReader.answer(for: .success(report)) else {
            return XCTFail("a successful read is a status")
        }

        XCTAssertEqual(status.processingState, "processed")
        XCTAssertEqual(status.transcriptionState, "not_applicable")
        XCTAssertEqual(status.reminderRequestState, "resolved")
        XCTAssertEqual(status.reminderScheduleState, "scheduled")
        XCTAssertNil(status.installedDueInstant)
        XCTAssertEqual(
            status.progress.reminder, .notConfirmed,
            "without an installed due time the acknowledgment must not say a reminder is set")
        XCTAssertEqual(status.progress.interpretation, .processed)
    }

    func testAnUnschedulableReasonIsKept() throws {
        let report = try Self.report(
            processing: "processed", transcription: "not_applicable", request: "unschedulable", schedule: nil,
            reason: "time_in_past")

        guard case .status(let status) = RootCaptureProgressReader.answer(for: .success(report)) else {
            return XCTFail("a successful read is a status")
        }
        XCTAssertEqual(status.progress.reminder, .notScheduled(.timeInPast))
    }

    func testARefusedOrFailedReadIsUnavailable() {
        XCTAssertEqual(RootCaptureProgressReader.answer(for: .failure(.handleClosed)), .unavailable)
    }

    func testAnAnswerFromAnotherQueueIsDeliveredOnTheMainQueue() throws {
        let report = try Self.report(
            processing: "pending", transcription: "not_applicable", request: nil, schedule: nil, reason: nil)
        let reader = RootCaptureProgressReader(readItem: { _, completion in
            DispatchQueue.global().async { completion(.success(report)) }
        })

        let delivered = expectation(description: "answer delivered")
        var onMain = false
        reader.readStatus(itemID: "item-1") { _ in
            onMain = Thread.isMainThread
            delivered.fulfill()
        }
        wait(for: [delivered], timeout: 5)
        XCTAssertTrue(onMain)
    }

    func testTheReaderAsksForTheItemItWasGiven() throws {
        var requested: [String] = []
        let reader = RootCaptureProgressReader(readItem: { itemID, completion in
            requested.append(itemID)
            completion(.failure(.handleClosed))
        })
        var answer: CaptureProgressReadResult?
        reader.readStatus(itemID: "item-7") { answer = $0 }
        XCTAssertEqual(requested, ["item-7"])
        XCTAssertEqual(answer, .unavailable)
    }

    // MARK: voice

    func testEveryRootRecordingOutcomeMapsToTheRecordersHonestResult() {
        let saved = CompositionRecordingOutcome.saved(
            captureID: "voice-1", itemID: "item-1", savedAt: "2026-10-14T09:00:00Z", alreadySaved: true)
        XCTAssertEqual(
            RootVoiceHandoff.result(for: saved),
            .saved(itemID: "item-1", savedAt: "2026-10-14T09:00:00Z", alreadyImported: true))
        XCTAssertEqual(RootVoiceHandoff.result(for: .keptOnDevice(captureID: "voice-1")), .keptForRetry)
        XCTAssertEqual(RootVoiceHandoff.result(for: .notStaged(captureID: "voice-1")), .notStaged)
        XCTAssertEqual(
            RootVoiceHandoff.result(for: .audioUnavailable(captureID: "voice-1")), .stagedAudioUnavailable,
            "missing staged audio is never reported as kept")
        XCTAssertEqual(RootVoiceHandoff.result(for: .itemDeleted(captureID: "voice-1")), .itemDeleted)
    }

    func testTheVoiceHandoffPassesTheRecordingToTheRootUnchanged() {
        var received: [CompositionRecordingHandoff] = []
        let handoff = RootVoiceHandoff(save: { recording, completion in
            received.append(recording)
            completion(.keptOnDevice(captureID: recording.captureID))
        })
        let startedAt = Date(timeIntervalSince1970: 1_790_000_000)

        var result: VoiceHandoffResult?
        handoff.handOff(
            VoiceRecordingHandoff(
                captureID: "voice-2", inProgressFileName: "voice-2.wav", finalizedFileName: "voice-2.wav",
                startedAt: startedAt, durationSeconds: 3, fileSizeBytes: 96_000, end: .stoppedByUser)
        ) { result = $0 }

        XCTAssertEqual(
            received,
            [CompositionRecordingHandoff(
                captureID: "voice-2", inProgressFileName: "voice-2.wav", finalizedFileName: "voice-2.wav",
                startedAt: startedAt)])
        XCTAssertEqual(result, .keptForRetry)
    }

    func testRecoveryKeepsUnknownOwnershipUnknown() {
        var unknown = CompositionRecoveryReport()
        unknown.unclaimedInProgressFileNames = nil
        XCTAssertNil(RootVoiceRecovery.report(for: unknown).unclaimedInProgressFileNames)
        XCTAssertNil(
            RootVoiceRecovery.report(for: nil).unclaimedInProgressFileNames,
            "a stopped root inspected nothing, so nothing is claimed to be absent")

        var known = CompositionRecoveryReport()
        known.unclaimedInProgressFileNames = ["voice-3.wav"]
        known.confirmedCaptureIDs = ["voice-4"]
        known.deletedItemCaptureIDs = ["voice-5"]
        let report = RootVoiceRecovery.report(for: known)
        XCTAssertEqual(report.unclaimedInProgressFileNames, ["voice-3.wav"])
        XCTAssertEqual(report.confirmedCaptureIDs, ["voice-4"])
        XCTAssertEqual(report.deletedItemCaptureIDs, ["voice-5"])
    }

    func testRecoveryRunsTheRootsPassAndReportsInTheRecordersTerms() {
        let ingress = RootVoiceRecovery(recover: { completion in
            var report = CompositionRecoveryReport()
            report.unclaimedInProgressFileNames = []
            completion(report)
        })
        var report: VoiceIngressRecoveryReport?
        ingress.recoverStagedCaptures { report = $0 }
        XCTAssertEqual(report?.unclaimedInProgressFileNames, [])
    }

    // MARK: support

    private static func report(
        processing: String, transcription: String, request: String?, schedule: String?, reason: String?
    ) throws -> ItemStatusReport {
        func json(_ value: String?) -> String { value.map { "\"\($0)\"" } ?? "null" }
        let document = """
            {"operation_id":1,"item_id":"item-1","save_state":"saved","sync_state":"synced",
            "processing_state":"\(processing)","transcription_state":"\(transcription)",
            "processing_job_status":null,"reminder_request_state":\(json(request)),
            "reminder_schedule_state":\(json(schedule)),"reminder_delivery_state":null,
            "reminder_acknowledgment_state":null,"unschedulable_reason":\(json(reason))}
            """
        return try JSONDecoder().decode(ItemStatusReport.self, from: Data(document.utf8))
    }
}
