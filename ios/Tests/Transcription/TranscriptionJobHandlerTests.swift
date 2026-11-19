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
        applyAttempts: Int = 3,
        audioFence: TranscriptionAudioFencing = NoTranscriptionAudioFence(),
        recognitionTimeout: TimeInterval = TranscriptionJobHandler.defaultRecognitionTimeout
    ) -> TranscriptionJobHandler {
        TranscriptionJobHandler(
            core: core, recognizer: recognizer, audio: DirectoryAudioLocator(directory: directory),
            audioFence: audioFence, applyAttempts: applyAttempts, applyRetryDelay: 0, recognitionTimeout: recognitionTimeout)
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
        XCTAssertEqual(transcript.transcriptGeneration, 2, "the recognizer's generation travels with its transcript")
        XCTAssertNil(applied.interpretation)
    }

    func testTheGenerationSentIsTheRecognizersNotAConstant() async throws {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(
            outcome: .transcript(text: "older recognizer", confidence: nil), transcriptGeneration: 1)

        _ = await makeHandler(core: core, recognizer: recognizer).run(run)

        guard case .transcript(let transcript) = try XCTUnwrap(core.applied.first).result else {
            return XCTFail("expected a transcript")
        }
        XCTAssertEqual(transcript.transcriptGeneration, 1)
    }

    func testAnAlreadyTranscribedItemIsRecognizedAgainAndTheCoreDecidesWhatItMeans() async throws {
        let source = TranscriptionFixtures.source(audioReference: reference, transcriptionState: "transcribed")
        for disposition: TranscriptionApplyAcknowledgment.Disposition in [.replaced, .unchanged, .kept, .discarded] {
            let core = makeCore(
                source: source, applyResults: [.success(TranscriptionFixtures.acknowledgment(disposition))])
            let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "the whole note", confidence: 0.7))

            let result = await makeHandler(core: core, recognizer: recognizer).run(run)

            XCTAssertEqual(result, .settled, "\(disposition) settles the work")
            XCTAssertEqual(recognizer.callCount, 1, "no native guard skips a transcribed item")
            guard case .transcript(let transcript) = try XCTUnwrap(core.applied.first).result else {
                return XCTFail("expected a transcript")
            }
            XCTAssertEqual(transcript.text, "the whole note")
            XCTAssertEqual(transcript.transcriptGeneration, 2)
        }
    }

    func testAReplacementOfAudioOpenForPlaybackWaitsAndIsRetried() async throws {
        let source = TranscriptionFixtures.source(audioReference: reference, transcriptionState: "transcribed")
        let core = makeCore(source: source)
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "the whole note", confidence: nil))
        // The fence the playback service takes a lease in while the item's audio is open.
        let fence = PlaybackFence()
        let lease = try fence.admit(
            itemID: TranscriptionFixtures.itemID, url: audioURL, generation: fence.generation(of: TranscriptionFixtures.itemID),
            verify: {}, end: { _ in })

        let result = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

        XCTAssertEqual(result, .transientFailure(reason: "audio_in_use"))
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertTrue(core.applied.isEmpty, "nothing is applied while the audio is open for playback")

        fence.release(lease)
        let retried = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

        XCTAssertEqual(retried, .settled)
        XCTAssertEqual(recognizer.callCount, 1)
        XCTAssertEqual(core.applied.count, 1)
    }

    func testARemovalWaitsWhileTheJobReadsTheAudioAndTheJobSettlesBeforeItRuns() async throws {
        for isReplacement in [false, true] {
            let source = TranscriptionFixtures.source(
                audioReference: reference, transcriptionState: isReplacement ? "transcribed" : "pending")
            let core = makeCore(source: source)
            let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "the whole note", confidence: nil))
            let fence = PlaybackFence()
            let removalDuringRecognition = RemovalProbe()
            let url: URL = audioURL
            recognizer.setOnRecognize { _ in
                // A retention sweep or the user's removal arriving while the recognizer reads the file.
                do {
                    try fence.withUnheld(url) { removalDuringRecognition.record(ran: true) }
                } catch {
                    removalDuringRecognition.record(ran: false)
                }
            }

            let result = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

            XCTAssertEqual(result, .settled)
            XCTAssertEqual(removalDuringRecognition.outcomes, [false], "the removal is refused while the job reads")
            XCTAssertEqual(core.applied.count, 1)
            // The lease is released when the job ends, so the removal can then go ahead.
            var removedAfter = false
            try fence.withUnheld(url) { removedAfter = true }
            XCTAssertTrue(removedAfter)
        }
    }

    func testADeletionOfTheItemEndsTheLeaseSoItsRemovalIsNotKeptWaiting() async throws {
        let core = makeCore(source: TranscriptionFixtures.source(audioReference: reference))
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "late words", confidence: nil))
        let fence = PlaybackFence()
        let removalDuringRecognition = RemovalProbe()
        let url: URL = audioURL
        recognizer.setOnRecognize { _ in
            // The user deletes the item while it is being recognized: the deletion is not made to wait.
            fence.endPlayback(itemID: TranscriptionFixtures.itemID)
            do {
                try fence.withUnheld(url) { removalDuringRecognition.record(ran: true) }
            } catch {
                removalDuringRecognition.record(ran: false)
            }
        }

        _ = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

        XCTAssertEqual(removalDuringRecognition.outcomes, [true])
    }

    func testAJobForAudioRemovedBeforeItReadsIsAppliedAsMissingNotRecognized() async throws {
        let source = TranscriptionFixtures.source(audioReference: reference, transcriptionState: "transcribed")
        let core = makeCore(source: source)
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "never read", confidence: nil))
        let fence = PlaybackFence()
        try fence.withUnheld(audioURL) { try FileManager.default.removeItem(at: audioURL) }

        let result = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

        XCTAssertEqual(result, .settled, "the core settles the replacement as kept")
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertEqual(core.applied.count, 1)
    }

    func testAFirstTranscriptionDoesNotWaitForPlayback() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "first time", confidence: nil))
        let fence = HeldAudioFence(held: [audioURL])

        let result = await makeHandler(core: core, recognizer: recognizer, audioFence: fence).run(run)

        XCTAssertEqual(result, .settled)
        XCTAssertEqual(recognizer.callCount, 1)
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

    func testARecognitionThatNeverAnswersIsCancelledAndRetriedLater() async {
        let core = makeCore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed, holdsUntilCancelled: true)

        let result = await makeHandler(core: core, recognizer: recognizer, recognitionTimeout: 0.1).run(run)

        XCTAssertEqual(result, .transientFailure(reason: "recognition_timed_out"))
        XCTAssertEqual(recognizer.callCount, 1)
        XCTAssertTrue(core.applied.isEmpty, "a timed-out recognition applies nothing")
        for _ in 0..<200 where !recognizer.wasCancelledWhileRecognizing {
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertTrue(recognizer.wasCancelledWhileRecognizing, "the stalled recognition is told to stop")
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

    func testADeletedItemIsNotRecognizedAndTheCoreRetiresTheJob() async {
        for source in [
            TranscriptionFixtures.source(audioReference: reference, lifecycleState: "deleted")
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

/// Records whether a removal attempted during recognition ran.
private final class RemovalProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [Bool] = []

    var outcomes: [Bool] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(ran: Bool) {
        lock.lock()
        recorded.append(ran)
        lock.unlock()
    }
}

/// An audio fence that holds the files it is given until they are released.
private final class HeldAudioFence: TranscriptionAudioFencing, @unchecked Sendable {
    private let lock = NSLock()
    private var held: Set<String>

    init(held: [URL]) {
        self.held = Set(held.map { $0.standardizedFileURL.path })
    }

    func release(_ url: URL) {
        lock.lock()
        held.remove(url.standardizedFileURL.path)
        lock.unlock()
    }

    func isAudioInUse(at url: URL) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return held.contains(url.standardizedFileURL.path)
    }

    func holdAgainstRemoval(itemID: String, at url: URL) -> (@Sendable () -> Void)? { {} }
}
