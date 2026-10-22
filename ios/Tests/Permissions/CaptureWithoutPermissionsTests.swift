import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Capture never waits on a permission, a provider or a notification: a fresh install saves a silent capture locally, and
/// a recording whose speech permission is refused keeps its audio and goes nowhere.
final class CaptureWithoutPermissionsTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        try super.setUpWithError()
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-permissions-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
        super.tearDown()
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 15) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    func testFreshInstallWithNoProviderSavesASilentCaptureLocally() throws {
        let storageRoot = directory.appendingPathComponent("storage", isDirectory: true)
        let root = try ServiceCompositionRoot(
            configuration: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "permissions-route", jobRetryDelaySeconds: 3600))
        defer { root.shutDown() }

        // Nothing has been granted, no provider is configured, and no permission has been requested.
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: "synthetic silent capture", capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, _, let alreadySaved)? = outcome else {
            return XCTFail("expected a confirmed local save, got \(String(describing: outcome))")
        }
        XCTAssertFalse(alreadySaved)
    }

    func testRefusedSpeechPermissionKeepsTheAudioAndAppliesNothing() async throws {
        let audioURL = directory.appendingPathComponent("clip-1.m4a")
        try TranscriptionFixtures.audioBytes.write(to: audioURL)
        let core = FakeTranscriptionCore(
            source: .success(TranscriptionFixtures.source(audioReference: "FinalizedAudio/clip-1.m4a")), applyResults: [])
        let recognizer = FakeOnDeviceRecognizer(outcome: .permissionDenied)
        let handler = TranscriptionJobHandler(
            core: core, recognizer: recognizer, audio: DirectoryAudioLocator(directory: directory),
            applyAttempts: 3, applyRetryDelay: 0, recognitionTimeout: TranscriptionJobHandler.defaultRecognitionTimeout)

        let result = await handler.run(
            JobCapabilityRun(jobID: TranscriptionFixtures.jobID, jobType: "transcription_attachment", attempt: 1))

        XCTAssertEqual(result, .permanentFailure(reason: "speech_permission_denied"))
        XCTAssertTrue(core.applied.isEmpty, "no transcript is invented")
        XCTAssertEqual(try Data(contentsOf: audioURL), TranscriptionFixtures.audioBytes, "the audio is untouched")
        XCTAssertEqual(recognizer.callCount, 1, "only the on-device recognizer was consulted")
    }

    func testSpeechNotYetDecidedLeavesTheAudioForALaterRetry() async throws {
        let audioURL = directory.appendingPathComponent("clip-1.m4a")
        try TranscriptionFixtures.audioBytes.write(to: audioURL)
        let core = FakeTranscriptionCore(
            source: .success(TranscriptionFixtures.source(audioReference: "FinalizedAudio/clip-1.m4a")), applyResults: [])
        let handler = TranscriptionJobHandler(
            core: core, recognizer: FakeOnDeviceRecognizer(outcome: .unavailableNow(reason: "speech_permission_not_determined")),
            audio: DirectoryAudioLocator(directory: directory),
            applyAttempts: 3, applyRetryDelay: 0, recognitionTimeout: TranscriptionJobHandler.defaultRecognitionTimeout)

        let result = await handler.run(
            JobCapabilityRun(jobID: TranscriptionFixtures.jobID, jobType: "transcription_attachment", attempt: 1))

        XCTAssertEqual(result, .transientFailure(reason: "speech_permission_not_determined"))
        XCTAssertTrue(core.applied.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: audioURL.path))
    }
}
