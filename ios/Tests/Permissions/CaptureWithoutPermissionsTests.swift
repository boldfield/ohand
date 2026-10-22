import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// A provider that is never reachable, as on a fresh install with no provider configured.
private final class UnreachableProviderSender: ProviderRequestSender {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}

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

    private final class SearchBox: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: Result<RetrievalPage, RetrievalSearchFailure>?

        var value: Result<RetrievalPage, RetrievalSearchFailure>? {
            lock.lock(); defer { lock.unlock() }
            return stored
        }

        func set(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
            lock.lock(); stored = result; lock.unlock()
        }
    }

    func testFreshInstallWithNoProviderAndDeniedNotificationsSavesAndRetrievesASilentCapture() throws {
        let storageRoot = directory.appendingPathComponent("storage", isDirectory: true)
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let deniedCenter = FakeNotificationCenter()
        deniedCenter.authorizationStatus = .denied
        let module = RetrievalSearchModule()
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "permissions-route", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(), readSession: session,
                notificationCenter: deniedCenter, reachability: FakeReachability(reachable: false)),
            modules: [module])
        defer { root.shutDown() }

        // Notifications are denied, no provider is configured, and no other permission has been requested.
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: "synthetic silent roofer capture", capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, let alreadySaved)? = outcome else {
            return XCTFail("expected a confirmed local save, got \(String(describing: outcome))")
        }
        XCTAssertFalse(alreadySaved)

        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
        let box = SearchBox()
        module.search(RetrievalSearchRequest(query: "roofer")) { box.set($0) }
        XCTAssertTrue(pump(until: { box.value != nil }), "the search completes")
        guard case .success(let page)? = box.value else {
            return XCTFail("expected the capture to be retrieved, got \(String(describing: box.value))")
        }
        XCTAssertEqual(page.rows.map(\.reference.value), [itemID])
        XCTAssertEqual(page.rows.first?.text, "synthetic silent roofer capture")
        XCTAssertTrue(deniedCenter.addedRequests.isEmpty, "no notification was scheduled")
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
