import XCTest
import OhAndCoreBridge
import OhAndServices
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

    func testTheVoiceStandInRefusesToStartAndRecordsNothing() {
        let voice = VoiceRecorderUnavailable()
        XCTAssertEqual(voice.state, .idle)

        var result: Result<VoiceRecordingStarted, VoiceStartFailure>?
        voice.start { result = $0 }
        XCTAssertEqual(result, .failure(.recorderUnavailable))
        XCTAssertEqual(voice.state, .idle)
        voice.stop()
        voice.cancel()
        XCTAssertEqual(voice.state, .idle)
    }

    func testStartingTheVoiceStandInTellsTheUserNothingWasRecorded() {
        let model = CaptureEntryModel(
            textIngress: RootTextCaptureIngress(save: { id, _, _, completion in completion(.notStaged(captureID: id)) }),
            voice: VoiceRecorderUnavailable(), defaultMode: .voice,
            access: { CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: false) })

        model.startRecording()

        XCTAssertEqual(model.recorderState, .idle)
        XCTAssertEqual(model.voiceNotice, CaptureEntryModel.startFailureMessage(.recorderUnavailable))
        XCTAssertNil(model.acknowledgment, "no recording means no acknowledgment of a save")
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
