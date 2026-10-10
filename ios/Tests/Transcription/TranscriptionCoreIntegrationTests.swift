import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A real core over a seeded store with the job runner, the transcription client and the real handler attached.
/// The recognizer is the only fake: it stands in for the speech framework, which a simulator test cannot make
/// produce speech. Events arrive on the main queue, where XCTest runs synchronous tests, so waiting pumps the run
/// loop.
private final class TranscriptionSession {
    let core: CoreHandle
    let client: TranscriptionCoreClient
    let service: JobRunnerService
    let sender = GatedSender(forwarding: nil, open: true)
    private(set) var otherEvents: [CoreEvent] = []
    private var isClosed = false

    init(storePath: String, layout: ProtectedStorageLayout, recognizer: OnDeviceRecognizing) throws {
        core = try CoreHandle(path: storePath)
        client = TranscriptionCoreClient(core: core)
        let handler = TranscriptionJobHandler(core: client, recognizer: recognizer, layout: layout)
        service = try JobRunnerService.attach(
            to: core, storePath: storePath, sender: sender, capabilities: [handler], isReachable: true, retryDelay: 3600)
        try core.setEventHandler { [unowned self] event in
            XCTAssertTrue(Thread.isMainThread, "events are delivered on the main thread")
            if !self.service.handle(event) && !self.client.handle(event) {
                self.otherEvents.append(event)
            }
        }
    }

    func pump(until condition: () -> Bool, timeout: TimeInterval = 20) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    func pumpFor(_ seconds: TimeInterval) {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    /// Runs one async client call to completion while the main run loop delivers the core's events.
    func runToCompletion<Value>(_ operation: @escaping @Sendable () async -> Value) throws -> Value {
        let box = ResultBox<Value>()
        Task { box.set(await operation()) }
        XCTAssertTrue(pump(until: { box.value != nil }), "the core never answered")
        return try XCTUnwrap(box.value)
    }

    func close() {
        guard !isClosed else { return }
        isClosed = true
        client.shutDown()
        service.shutDown()
        core.close()
    }
}

private final class ResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Value?

    var value: Value? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Value) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// Drives the real handler through the real job runner into the real Rust core over the protected store layout:
/// a queued durable recording is recognized on the device and its outcome applied through the guarded core
/// attachment. Whether the speech framework recognizes real speech offline is device evidence; none of it is
/// claimed here.
final class TranscriptionCoreIntegrationTests: IngressStorageTestCase {
    private let reference = "FinalizedAudio/clip-1.m4a"
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var sessions: [AnyObject] = []
    private var closers: [() -> Void] = []

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDown() {
        closers.forEach { $0() }
        closers = []
        sessions = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: helpers

    private var audioURL: URL { finalizedAudioURL("clip-1.m4a") }
    private var audioDigest: String { TranscriptionFixtures.sha256Hex(TranscriptionFixtures.audioBytes) }

    private func seededStore(
        lifecycleState: String = "active",
        transcriptionState: String = "audio_pending",
        jobStatus: String? = "queued",
        attemptCount: Int = 0,
        leaseExpiresAt: String? = nil
    ) throws -> TranscriptionStore {
        try TranscriptionFixtures.audioBytes.write(to: audioURL)
        let store = try TranscriptionStore.make(at: layout.databaseURL.path)
        try store.seedVoiceCapture(
            audioReference: reference, lifecycleState: lifecycleState, transcriptionState: transcriptionState,
            jobStatus: jobStatus, attemptCount: attemptCount, leaseExpiresAt: leaseExpiresAt)
        return store
    }

    fileprivate func makeSession(
        _ store: TranscriptionStore,
        recognizer: OnDeviceRecognizing
    ) throws -> TranscriptionSession {
        let session = try TranscriptionSession(storePath: store.path, layout: layout, recognizer: recognizer)
        sessions.append(session)
        closers.append { session.close() }
        return session
    }

    private func outcomeRequest(
        attempt: Int,
        result: TranscriptionResultRequest,
        interpretation: InterpretationJobRequest? = nil
    ) -> TranscriptionOutcomeRequest {
        TranscriptionOutcomeRequest(
            jobID: TranscriptionFixtures.jobID, leaseAttempt: attempt, audioReference: reference, result: result,
            interpretation: interpretation)
    }

    private func transcript(_ text: String, digest: String? = nil) -> TranscriptionResultRequest {
        .transcript(RecognizedTranscriptRequest(
            text: text, confidence: 0.9, detectedLanguage: nil, recognizer: "fixture/1", audioSHA256: digest ?? audioDigest))
    }

    // MARK: queued job to attached transcript

    func testAQueuedRecordingIsRecognizedAndItsTranscriptAttachedThroughTheCore() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "call the roofer at nine", confidence: 0.9))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(session.pump(until: { store.job?.status == "completed" }), "the job never completed")
        XCTAssertEqual(recognizer.callCount, 1)
        XCTAssertEqual(recognizer.urls, [audioURL])
        XCTAssertEqual(recognizer.locales, ["en_US"])
        XCTAssertEqual(store.item?.transcriptionState, "transcribed")
        XCTAssertEqual(store.attachments.count, 1)
        XCTAssertEqual(store.attachments.first?.text, "call the roofer at nine")
        XCTAssertEqual(store.attachments.first?.audioSHA256, audioDigest)
        XCTAssertEqual(store.attachments.first?.recognizer, "fake-on-device/1.0")
        XCTAssertEqual(store.attachments.first?.sourceJobID, TranscriptionFixtures.jobID)
        XCTAssertNil(store.captureText, "the capture's own text is never overwritten")
        XCTAssertEqual(try Data(contentsOf: audioURL), TranscriptionFixtures.audioBytes, "the audio is preserved")
        XCTAssertEqual(session.sender.requestCount, 0, "nothing left the device")
    }

    func testAnUnsupportedLanguageKeepsTheItemPendingAndTheAudio() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .unsupported)
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(session.pump(until: { store.job?.status == "failed" }), "the job never settled")
        XCTAssertEqual(store.job?.failureReason, "transcription_unsupported")
        XCTAssertEqual(store.item?.transcriptionState, "transcription_unsupported")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(try Data(contentsOf: audioURL), TranscriptionFixtures.audioBytes)
        XCTAssertEqual(session.sender.requestCount, 0, "there is no remote fallback")
    }

    func testARecognizerFailureIsRecordedAndTheAudioKept() throws {
        let store = try seededStore()
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))

        session.service.activate()

        XCTAssertTrue(session.pump(until: { store.job?.status == "failed" }), "the job never settled")
        XCTAssertEqual(store.job?.failureReason, "transcription_failed")
        XCTAssertEqual(store.item?.transcriptionState, "transcription_failed")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertTrue(exists(audioURL))
    }

    func testRecordingWithNoSpeechAttachesNothing() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "", confidence: nil))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(session.pump(until: { store.job?.status == "failed" }), "the job never settled")
        XCTAssertEqual(store.job?.failureReason, "no_speech_detected")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertNil(store.captureText)
    }

    func testADeletedItemIsNotRecognizedAndItsJobIsRetired() throws {
        let store = try seededStore(lifecycleState: "deleted")
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "should not appear", confidence: nil))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(
            session.pump(until: { store.job.map { $0.status != "queued" && $0.status != "running" } ?? false }),
            "the job was never retired")
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(store.item?.lifecycleState, "deleted")
    }

    func testAMissingAudioFileIsFailedWithoutRecognizing() throws {
        let store = try seededStore()
        try FileManager.default.removeItem(at: audioURL)
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "should not appear", confidence: nil))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(
            session.pump(until: { store.job.map { $0.status != "queued" && $0.status != "running" } ?? false }),
            "the job was never retired")
        XCTAssertEqual(recognizer.callCount, 0)
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertNotEqual(store.item?.transcriptionState, "transcribed")
    }

    // MARK: interruption, retry and cancellation

    func testAnInterruptedRecognitionConvergesToExactlyOneAttachment() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .failed, holdsUntilCancelled: true)
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()
        XCTAssertTrue(session.pump(until: { recognizer.callCount == 1 }), "recognition never started")
        session.service.suspend()
        XCTAssertTrue(session.pump(until: { recognizer.wasCancelledWhileRecognizing }), "the recognizer was not cancelled")
        XCTAssertTrue(session.pump(until: { !session.service.isDraining }), "the drain never stopped")

        XCTAssertTrue(store.attachments.isEmpty, "nothing was attached while interrupted")
        XCTAssertNotEqual(store.job?.status, "failed")
        XCTAssertNotEqual(store.job?.status, "completed")

        recognizer.setOutcome(.transcript(text: "second try", confidence: nil))
        session.service.activate()

        XCTAssertTrue(session.pump(until: { store.job?.status == "completed" }), "the retry never completed")
        XCTAssertEqual(store.attachments.map(\.text), ["second try"])
        XCTAssertEqual(store.item?.transcriptionState, "transcribed")
    }

    func testRecognizerNotReadyYetLeavesTheItemPendingForALaterRetry() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .unavailableNow(reason: "recognizer_unavailable"))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(session.pump(until: { recognizer.callCount >= 1 }), "recognition never started")
        XCTAssertTrue(session.pump(until: { !session.service.isDraining }), "the drain never stopped")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(store.item?.transcriptionState, "audio_pending")
        XCTAssertNotEqual(store.job?.status, "completed")
        XCTAssertEqual(session.sender.requestCount, 0)
    }

    // MARK: guarded transport at the client

    func testAStaleLeaseIsRefusedOnReadAndApplyAndWritesNothing() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 2, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client
        let stale = outcomeRequest(attempt: 1, result: transcript("from a superseded attempt"))

        let read = try session.runToCompletion { await client.readSource(jobID: TranscriptionFixtures.jobID, attempt: 1) }
        let apply = try session.runToCompletion { await client.apply(stale) }

        guard case .failure(let readFailure) = read else { return XCTFail("a stale read must fail") }
        XCTAssertEqual(readFailure.code, "transcription_lease_not_held")
        guard case .failure(let applyFailure) = apply else { return XCTFail("a stale apply must fail") }
        XCTAssertEqual(applyFailure.code, "transcription_lease_not_held")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertEqual(store.item?.transcriptionState, "audio_pending")
        XCTAssertEqual(store.job?.status, "running")
    }

    func testTheLeasedJobReadsBackItsAudioReferenceAndLocaleWithoutWriting() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 1, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client

        let read = try session.runToCompletion { await client.readSource(jobID: TranscriptionFixtures.jobID, attempt: 1) }

        guard case .success(let source) = read else { return XCTFail("the leased read must succeed") }
        XCTAssertEqual(source.itemID, TranscriptionFixtures.itemID)
        XCTAssertEqual(source.audioReference, reference)
        XCTAssertEqual(source.locale, "en_US")
        XCTAssertEqual(source.transcriptionState, "audio_pending")
        XCTAssertEqual(store.job?.status, "running")
        XCTAssertEqual(store.job?.attemptCount, 1)
        XCTAssertTrue(store.attachments.isEmpty)
    }

    func testRedeliveryOfTheSameOutcomeConvergesAndADifferentTranscriptIsRefused() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 1, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client
        let first = outcomeRequest(attempt: 1, result: transcript("water the plants"))
        let different = outcomeRequest(attempt: 1, result: transcript("something else entirely"))

        let attached = try session.runToCompletion { await client.apply(first) }
        let repeated = try session.runToCompletion { await client.apply(first) }
        let conflicting = try session.runToCompletion { await client.apply(different) }

        guard case .success(let firstAck) = attached else { return XCTFail("the first apply must commit") }
        XCTAssertEqual(firstAck.disposition, .attached)
        XCTAssertEqual(firstAck.transcriptionState, "transcribed")
        guard case .success(let secondAck) = repeated else { return XCTFail("a redelivery must converge") }
        XCTAssertEqual(secondAck.disposition, .alreadyAttached)
        guard case .failure(let conflict) = conflicting else { return XCTFail("a different transcript must be refused") }
        XCTAssertEqual(conflict.code, "transcription_conflicting_result")
        XCTAssertEqual(store.attachments.map(\.text), ["water the plants"])
        XCTAssertEqual(store.job?.status, "completed")
    }

    func testAudioOfAnotherIdentityIsDiscardedAndNeverAttached() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 1, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client
        let wrongFile = TranscriptionOutcomeRequest(
            jobID: TranscriptionFixtures.jobID, leaseAttempt: 1, audioReference: "FinalizedAudio/another.m4a",
            result: transcript("heard another file"))

        let outcome = try session.runToCompletion { await client.apply(wrongFile) }

        guard case .success(let acknowledgment) = outcome else { return XCTFail("a discard is acknowledged") }
        XCTAssertEqual(acknowledgment.disposition, .discarded)
        XCTAssertEqual(acknowledgment.discardReason, "audio_identity_mismatch")
        XCTAssertTrue(store.attachments.isEmpty)
        XCTAssertNil(store.captureText)
        XCTAssertEqual(store.item?.transcriptionState, "audio_pending")
        XCTAssertEqual(try Data(contentsOf: audioURL), TranscriptionFixtures.audioBytes)
    }

    func testFailuresAreBoundedAndNeverEchoTheTranscript() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 1, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client
        let secret = "private words that must not appear in a failure message"
        let stale = outcomeRequest(attempt: 9, result: transcript(secret))

        let outcome = try session.runToCompletion { await client.apply(stale) }

        guard case .failure(let failure) = outcome else { return XCTFail("a stale apply must fail") }
        XCTAssertFalse(failure.message.contains(secret))
        XCTAssertFalse(failure.code.contains(secret))
        XCTAssertLessThan(failure.message.utf8.count, 256)
    }

    func testShuttingTheClientDownFailsWaitersAndRefusesLaterCalls() throws {
        let store = try seededStore(jobStatus: "running", attemptCount: 1, leaseExpiresAt: "2099-01-01T00:00:00Z")
        let session = try makeSession(store, recognizer: FakeOnDeviceRecognizer(outcome: .failed))
        let client = session.client
        client.shutDown()

        let read = try session.runToCompletion { await client.readSource(jobID: TranscriptionFixtures.jobID, attempt: 1) }

        guard case .failure(let failure) = read else { return XCTFail("a call after shutdown must fail") }
        XCTAssertEqual(failure.code, "client_shut_down")
        XCTAssertEqual(failure.errorClass, .cancelled)
    }

    // MARK: the registered handler

    func testTheHandlerIsRegisteredForTheTranscriptionJobTypeAndTheCoreRunsIt() throws {
        let store = try seededStore()
        let recognizer = FakeOnDeviceRecognizer(outcome: .transcript(text: "registered", confidence: nil))
        let session = try makeSession(store, recognizer: recognizer)

        session.service.activate()

        XCTAssertTrue(session.pump(until: { recognizer.callCount == 1 }), "the core never ran the native handler")
        XCTAssertTrue(session.pump(until: { store.job?.status == "completed" }))
        XCTAssertTrue(session.pump(until: { !session.service.isDraining }))
        guard case .finished(let summary)? = session.service.lastOutcome else { return XCTFail("no drain summary") }
        XCTAssertTrue(summary.jobs.contains { $0.jobType == "transcription_attachment" })
    }
}
