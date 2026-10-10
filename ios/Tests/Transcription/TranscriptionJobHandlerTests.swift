import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// The handler's decisions with a scripted core and recognizer over real temporary audio files: what it applies,
/// what it leaves to retry, and that nothing about it reaches a remote service.
final class TranscriptionJobHandlerTests: XCTestCase {
    private var directory: URL!
    private var audioURL: URL!
    private let reference = "FinalizedAudio/clip-1.m4a"

    override func setUpWithError() throws {
        try super.setUpWithError()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-transcription-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        audioURL = directory.appendingPathComponent("clip-1.m4a")
        try TranscriptionFixtures.audioBytes.write(to: audioURL)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private let run = JobCapabilityRun(jobID: TranscriptionFixtures.jobID, jobType: "transcription_attachment", attempt: 3)

    private func makeHandler(
        core: FakeTranscriptionCore,
        recognizer: FakeOnDeviceRecognizer,
        applyAttempts: Int = 3
    ) -> TranscriptionJobHandler {
        TranscriptionJobHandler(
            core: core, recognizer: recognizer, audio: DirectoryAudioLocator(directory: directory),
            applyAttempts: applyAttempts, applyRetryDelay: 0)
    }

    private func makeCore(
        source: TranscriptionJobSource? = nil,
        applyResults: [Result<TranscriptionApplyAcknowledgment, CoreFailure>] = []
    ) -> FakeTranscriptionCore {
        FakeTranscriptionCore(
            source: .success(source ?? TranscriptionFixtures.source(audioReference: reference)),
            applyResults: applyResults)
    }

    func testHandlesTheTranscriptionJobType() {
        let handler = makeHandler(core: makeCore(), recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        XCTAssertEqual(handler.jobType, "transcription_attachment")
    }

    func testRecognizedTranscriptIsAppliedWithTheAudioDigestAndTheLeaseAttempt() async throws {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "call the roofer", confidence: 0.8))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(recognizer.urls, [audioURL])
        XCTAssertEqual(recognizer.locales, ["en_US"])
        let applied = try XCTUnwrap(core.applied.first)
        XCTAssertEqual(core.applied.count, 1)
        XCTAssertEqual(applied.jobID, TranscriptionFixtures.jobID)
        XCTAssertEqual(applied.leaseAttempt, 3)
        XCTAssertEqual(applied.audioReference, reference)
        guard case .transcript(let transcript) = applied.result else { return XCTFail("expected a transcript") }
        XCTAssertEqual(transcript.text, "call the roofer")
        XCTAssertEqual(transcript.confidence, 0.8)
        XCTAssertEqual(transcript.recognizer, "fake-on-device/1.0")
        XCTAssertEqual(transcript.audioSHA256, TranscriptionFixtures.sha256Hex(TranscriptionFixtures.audioBytes))
        XCTAssertNil(applied.interpretation)
    }

    func testInterpretationRequestIsPassedOnlyWhenTheHostAsksForOne() async throws {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "water the plants", confidence: nil))
        let request = InterpretationJobRequest(jobID: "job-interpret-1", profileVersion: nil, requestVersion: "request-1")
        let handler = TranscriptionJobHandler(
            core: core, recognizer: recognizer, audio: DirectoryAudioLocator(directory: directory),
            interpretation: { _ in request }, applyAttempts: 1, applyRetryDelay: 0)

        _ = await handler.run(run)

        XCTAssertEqual(core.applied.first?.interpretation, request)
    }

    func testUnsupportedLanguageIsAppliedAsUnsupportedAndKeepsTheAudio() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .unsupported)

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(core.applied.map(\.result), [.unsupported])
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
    }

    func testRecognizerFailureIsAppliedAsFailed() async {
        let core = makeCore()

        let result = await makeHandler(core: core, recognizer: FakeOnDeviceRecognizer(outcome: .failed)).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(core.applied.map(\.result), [.failed])
    }

    func testEmptyRecognitionIsHandedToTheCoreToDecide() async throws {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "", confidence: nil))

        _ = await makeHandler(core: core, recognizer: recognizer).run(run)

        guard case .transcript(let transcript)? = core.applied.first?.result else { return XCTFail("expected a transcript") }
        XCTAssertEqual(transcript.text, "")
    }

    func testRecognizerNotReadyYetIsATransientFailureAndAppliesNothing() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .unavailableNow(reason: "Speech Permission-Not Determined!"))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .transientFailure(reason: "speech_permission_not_determined_"))
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testDeniedPermissionIsAPermanentFailureThatLeavesTheItemAlone() async {
        let core = makeCore()

        let result = await makeHandler(core: core, recognizer: FakeOnDeviceRecognizer(outcome: .permissionDenied)).run(run)

        XCTAssertEqual(result, .permanentFailure(reason: "speech_permission_denied"))
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testCancelledRecognitionIsInterruptedAndAppliesNothing() async {
        let core = makeCore()

        let result = await makeHandler(core: core, recognizer: FakeOnDeviceRecognizer(outcome: .cancelled)).run(run)

        XCTAssertEqual(result, .interrupted)
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testCancellingTheTaskWhileRecognizingEndsInterruptedWithoutApplying() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed, holdsUntilCancelled: true)
        let handler = makeHandler(core: core, recognizer: recognizer)
        let jobRun = run

        let task = Task { await handler.run(jobRun) }
        while recognizer.callCount == 0 { try? await Task.sleep(nanoseconds: 5_000_000) }
        task.cancel()
        let result = await task.value

        XCTAssertEqual(result, .interrupted)
        XCTAssertTrue(recognizer.wasCancelledWhileRecognizing)
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testATaskCancelledBeforeStartingDoesNothing() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed)
        let handler = makeHandler(core: core, recognizer: recognizer)
        let jobRun = run

        let task = Task { () -> JobCapabilityResult in
            while !Task.isCancelled { try? await Task.sleep(nanoseconds: 1_000_000) }
            return await handler.run(jobRun)
        }
        task.cancel()

        let result = await task.value
        XCTAssertEqual(result, .interrupted)
        XCTAssertEqual(core.readCount, 0)
        XCTAssertEqual(recognizer.callCount, 0)
    }

    func testAJobWithoutAudioIsAPermanentFailureAndNeverRecognizes() async {
        let core = makeCore(source: TranscriptionFixtures.source(audioReference: nil))
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed)

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .permanentFailure(reason: "no_audio_reference"))
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testDeletedOrAlreadyTranscribedItemsAreNotRecognizedAndTheCoreRetiresTheJob() async {
        for source in [
            TranscriptionFixtures.source(audioReference: reference, lifecycleState: "deleted"),
            TranscriptionFixtures.source(audioReference: reference, transcriptionState: "transcribed"),
        ] {
            let core = makeCore(source: source)
            let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "ignored", confidence: nil))

            let result = await makeHandler(core: core, recognizer: recognizer).run(run)

            XCTAssertEqual(result, .settled)
            XCTAssertEqual(recognizer.callCount, 0)
            XCTAssertEqual(core.applied.map(\.result), [.failed])
        }
    }

    func testMissingAudioFileIsAppliedAsFailedWithoutRecognizing() async {
        try? FileManager.default.removeItem(at: audioURL)
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "ignored", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertEqual(core.applied.map(\.result), [.failed])
    }

    func testAReferenceOutsideTheAudioStoreIsNeverRead() async {
        let core = makeCore(source: TranscriptionFixtures.source(audioReference: "../secrets/clip-1.m4a"))
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "ignored", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertEqual(core.applied.map(\.result), [.failed])
    }

    func testAudioThatChangesWhileRecognizingIsRetriedNotAttached() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "stale", confidence: nil))
        let changedURL = audioURL!
        recognizer.setOnRecognize { _ in try? Data("different bytes".utf8).write(to: changedURL) }

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .transientFailure(reason: "audio_changed"))
        XCTAssertTrue(core.applied.isEmpty)
    }

    func testAStaleLeaseOnReadIsInterruptedAndNeverRecognizes() async {
        let core = FakeTranscriptionCore(source: .failure(TranscriptionFixtures.failure("transcription_lease_not_held")))
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed)

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .interrupted)
        XCTAssertEqual(recognizer.callCount, 0)
    }

    func testOtherReadFailuresAreClassifiedByTheirCoreClass() async {
        let transient = FakeTranscriptionCore(source: .failure(TranscriptionFixtures.failure("store_busy", .transient)))
        let permanent = FakeTranscriptionCore(source: .failure(TranscriptionFixtures.failure("transcription_wrong_job_type")))
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed)

        let transientResult = await makeHandler(core: transient, recognizer: recognizer).run(run)
        let permanentResult = await makeHandler(core: permanent, recognizer: recognizer).run(run)

        XCTAssertEqual(transientResult, .transientFailure(reason: "store_busy"))
        XCTAssertEqual(permanentResult, .permanentFailure(reason: "transcription_wrong_job_type"))
        XCTAssertEqual(recognizer.callCount, 0)
    }

    func testAnApplyTheCoreCouldNotCommitIsRepeatedWithTheSameOutcomeUntilItConverges() async {
        let core = makeCore(applyResults: [
            .failure(TranscriptionFixtures.failure("transcription_not_committed", .transient)),
            .failure(TranscriptionFixtures.failure("transcription_commit_unknown", .transient)),
            .success(TranscriptionFixtures.acknowledgment(.alreadyAttached)),
        ])
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "same words", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(core.applied.count, 3)
        XCTAssertTrue(core.applied.allSatisfy { $0 == core.applied[0] }, "every repeat carries the identical outcome")
        XCTAssertEqual(recognizer.callCount, 1, "recognition is not repeated")
    }

    func testAnApplyThatNeverConfirmsIsATransientFailure() async {
        let core = makeCore(applyResults: [.failure(TranscriptionFixtures.failure("transcription_not_committed", .transient))])
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "words", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer, applyAttempts: 2).run(run)

        XCTAssertEqual(result, .transientFailure(reason: "apply_unconfirmed"))
        XCTAssertEqual(core.applied.count, 2)
    }

    func testAStaleLeaseOnApplyIsInterruptedAndNotRepeated() async {
        let core = makeCore(applyResults: [.failure(TranscriptionFixtures.failure("transcription_lease_not_held"))])
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "words", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .interrupted)
        XCTAssertEqual(core.applied.count, 1)
    }

    func testARefusedApplyIsAPermanentFailureLabelledByItsCode() async {
        let core = makeCore(applyResults: [.failure(TranscriptionFixtures.failure("transcription_conflicting_result"))])
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "words", confidence: nil))

        let result = await makeHandler(core: core, recognizer: recognizer).run(run)

        XCTAssertEqual(result, .permanentFailure(reason: "transcription_conflicting_result"))
        XCTAssertEqual(core.applied.count, 1)
    }

    func testFailureLabelsAreShortLowercaseAndNeverContent() {
        XCTAssertEqual(TranscriptionJobHandler.label("Call The Roofer, 9am"), "call_the_roofer__9am")
        XCTAssertEqual(TranscriptionJobHandler.label(""), "unknown")
        XCTAssertEqual(TranscriptionJobHandler.label(String(repeating: "a", count: 200)).count, 64)
    }
}
