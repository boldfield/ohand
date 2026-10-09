import XCTest

final class FakeRecognitionHandle: RecognitionHandle {
    private(set) var cancelCount = 0

    func cancel() {
        cancelCount += 1
    }
}

final class FakeSpeechEngine: SpeechRecognitionEngine {
    var currentSnapshot = RecognizerSnapshot(
        permission: .authorized,
        recognizerExists: true,
        isAvailable: true,
        supportsOnDevice: true
    )
    var permissionAfterRequest: SpeechPermission?
    var completeSynchronouslyWith: RecognitionOutcome?
    private(set) var snapshotLocales: [String] = []
    private(set) var recognizeCalls: [(audioURL: URL, localeIdentifier: String)] = []
    private(set) var handles: [FakeRecognitionHandle] = []
    private(set) var completions: [(RecognitionOutcome) -> Void] = []

    func snapshot(localeIdentifier: String) -> RecognizerSnapshot {
        snapshotLocales.append(localeIdentifier)
        return currentSnapshot
    }

    func requestPermission(completion: @escaping (SpeechPermission) -> Void) {
        if let granted = permissionAfterRequest {
            currentSnapshot.permission = granted
        }
        completion(currentSnapshot.permission)
    }

    func recognize(
        audioURL: URL,
        localeIdentifier: String,
        completion: @escaping (RecognitionOutcome) -> Void
    ) -> RecognitionHandle {
        recognizeCalls.append((audioURL: audioURL, localeIdentifier: localeIdentifier))
        let handle = FakeRecognitionHandle()
        handles.append(handle)
        completions.append(completion)
        if let outcome = completeSynchronouslyWith {
            completion(outcome)
        }
        return handle
    }
}

final class TranscriptionCoordinatorTests: XCTestCase {
    private var directory: URL!
    private var audioURL: URL!
    private let audioBytes = Data((0..<2048).map { UInt8($0 % 251) })
    private var engine: FakeSpeechEngine!
    private var clock: TimeInterval = 100
    private var coordinator: TranscriptionCoordinator!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("transcription-probe-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        audioURL = directory.appendingPathComponent("transcription-fixture.wav")
        try audioBytes.write(to: audioURL)
        engine = FakeSpeechEngine()
        clock = 100
        coordinator = TranscriptionCoordinator(engine: engine, localeIdentifier: "en-US", now: { [unowned self] in self.clock })
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    private func assertAudioPreserved(file: StaticString = #filePath, line: UInt = #line) throws {
        let current = try Data(contentsOf: audioURL)
        XCTAssertEqual(current, audioBytes, "audio bytes must be untouched", file: file, line: line)
        XCTAssertEqual(coordinator.audioURL, audioURL, "audio must stay attached", file: file, line: line)
    }

    private func loadAndExpect(_ expected: TranscriptionStatus, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNil(coordinator.loadAudio(at: audioURL), file: file, line: line)
        XCTAssertEqual(coordinator.status, expected, file: file, line: line)
    }

    // MARK: readiness mapping

    func testReadinessTable() {
        func snapshot(
            _ permission: SpeechPermission,
            exists: Bool = true,
            available: Bool = true,
            onDevice: Bool = true
        ) -> RecognizerSnapshot {
            return RecognizerSnapshot(permission: permission, recognizerExists: exists, isAvailable: available, supportsOnDevice: onDevice)
        }
        XCTAssertNil(evaluateReadiness(snapshot(.authorized), localeIdentifier: "en-US"))
        XCTAssertEqual(evaluateReadiness(snapshot(.notDetermined), localeIdentifier: "en-US"), .permissionNotDetermined)
        XCTAssertEqual(evaluateReadiness(snapshot(.denied), localeIdentifier: "en-US"), .permissionDenied)
        XCTAssertEqual(evaluateReadiness(snapshot(.restricted), localeIdentifier: "en-US"), .permissionRestricted)
        XCTAssertEqual(
            evaluateReadiness(snapshot(.authorized, exists: false, available: false, onDevice: false), localeIdentifier: "zz-ZZ"),
            .languageUnsupported(locale: "zz-ZZ")
        )
        XCTAssertEqual(
            evaluateReadiness(snapshot(.authorized, onDevice: false), localeIdentifier: "fr-FR"),
            .onDeviceUnavailable(locale: "fr-FR")
        )
        XCTAssertEqual(
            evaluateReadiness(snapshot(.authorized, available: false), localeIdentifier: "en-US"),
            .recognizerUnavailable(locale: "en-US")
        )
        XCTAssertEqual(
            evaluateReadiness(snapshot(.denied, exists: false, available: false, onDevice: false), localeIdentifier: "en-US"),
            .permissionDenied,
            "permission is reported before recognizer problems"
        )
    }

    func testEveryPendingReasonHasVisibleText() {
        let reasons: [PendingReason] = [
            .permissionNotDetermined, .permissionDenied, .permissionRestricted,
            .languageUnsupported(locale: "zz-ZZ"), .onDeviceUnavailable(locale: "en-US"), .recognizerUnavailable(locale: "en-US")
        ]
        for reason in reasons {
            XCTAssertFalse(reason.description.isEmpty)
            XCTAssertTrue(TranscriptionStatus.pending(reason).description.contains("Audio kept"))
        }
    }

    // MARK: audio preserved while pending

    func testPendingStatesKeepAudioAndNeverCallRecognizer() throws {
        let cases: [(RecognizerSnapshot, PendingReason)] = [
            (RecognizerSnapshot(permission: .notDetermined, recognizerExists: true, isAvailable: true, supportsOnDevice: true), .permissionNotDetermined),
            (RecognizerSnapshot(permission: .denied, recognizerExists: true, isAvailable: true, supportsOnDevice: true), .permissionDenied),
            (RecognizerSnapshot(permission: .restricted, recognizerExists: true, isAvailable: true, supportsOnDevice: true), .permissionRestricted),
            (RecognizerSnapshot(permission: .authorized, recognizerExists: false, isAvailable: false, supportsOnDevice: false), .languageUnsupported(locale: "en-US")),
            (RecognizerSnapshot(permission: .authorized, recognizerExists: true, isAvailable: true, supportsOnDevice: false), .onDeviceUnavailable(locale: "en-US")),
            (RecognizerSnapshot(permission: .authorized, recognizerExists: true, isAvailable: false, supportsOnDevice: true), .recognizerUnavailable(locale: "en-US"))
        ]
        for (snapshot, reason) in cases {
            engine.currentSnapshot = snapshot
            loadAndExpect(.pending(reason))
            coordinator.startTranscription()
            XCTAssertEqual(coordinator.status, .pending(reason))
            try assertAudioPreserved()
        }
        XCTAssertTrue(engine.recognizeCalls.isEmpty, "no recognition may be attempted, so nothing can fall back to the network")
    }

    func testUnsupportedLanguageSelectionLeavesAudioPending() throws {
        loadAndExpect(.ready)
        engine.currentSnapshot.recognizerExists = false
        XCTAssertTrue(coordinator.setLocale("zz-ZZ"))
        XCTAssertEqual(engine.snapshotLocales.last, "zz-ZZ")
        XCTAssertEqual(coordinator.status, .pending(.languageUnsupported(locale: "zz-ZZ")))
        coordinator.startTranscription()
        XCTAssertTrue(engine.recognizeCalls.isEmpty)
        try assertAudioPreserved()
    }

    func testPermissionRevokedAfterReadyBlocksNextRun() throws {
        loadAndExpect(.ready)
        engine.currentSnapshot.permission = .denied
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .pending(.permissionDenied))
        XCTAssertTrue(engine.recognizeCalls.isEmpty)
        try assertAudioPreserved()
    }

    func testRequestPermissionRefreshesPendingStatus() {
        engine.currentSnapshot.permission = .notDetermined
        engine.permissionAfterRequest = .authorized
        loadAndExpect(.pending(.permissionNotDetermined))
        coordinator.requestPermission()
        XCTAssertEqual(coordinator.status, .ready)
    }

    func testRequestPermissionDeniedStaysPending() {
        engine.currentSnapshot.permission = .notDetermined
        engine.permissionAfterRequest = .denied
        loadAndExpect(.pending(.permissionNotDetermined))
        coordinator.requestPermission()
        XCTAssertEqual(coordinator.status, .pending(.permissionDenied))
    }

    func testAvailabilityChangeUpdatesPendingAndReady() {
        loadAndExpect(.ready)
        engine.currentSnapshot.isAvailable = false
        coordinator.refreshAfterEnvironmentChange()
        XCTAssertEqual(coordinator.status, .pending(.recognizerUnavailable(locale: "en-US")))
        engine.currentSnapshot.isAvailable = true
        coordinator.refreshAfterEnvironmentChange()
        XCTAssertEqual(coordinator.status, .ready)
    }

    func testEnvironmentChangeDoesNotDisturbRunningOrFinishedTranscription() {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        engine.currentSnapshot.isAvailable = false
        coordinator.refreshAfterEnvironmentChange()
        XCTAssertEqual(coordinator.status, .inProgress)
        engine.completions[0](.transcript(text: "hello", confidence: nil))
        coordinator.refreshAfterEnvironmentChange()
        XCTAssertEqual(coordinator.status, .transcribed(text: "hello", confidence: nil, durationSeconds: 0))
    }

    func testRefreshWithoutAudioStaysNoAudio() {
        coordinator.refreshAfterEnvironmentChange()
        XCTAssertEqual(coordinator.status, .noAudio)
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .noAudio)
        XCTAssertTrue(engine.recognizeCalls.isEmpty)
    }

    // MARK: running

    func testSuccessfulTranscriptionUsesFixtureAndLocaleAndMeasuresDuration() throws {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .inProgress)
        XCTAssertEqual(engine.recognizeCalls.count, 1)
        XCTAssertEqual(engine.recognizeCalls[0].audioURL, audioURL)
        XCTAssertEqual(engine.recognizeCalls[0].localeIdentifier, "en-US")
        clock += 1.5
        engine.completions[0](.transcript(text: "  Remind me to call the dentist  ", confidence: 0.8))
        XCTAssertEqual(
            coordinator.status,
            .transcribed(text: "Remind me to call the dentist", confidence: 0.8, durationSeconds: 1.5)
        )
        XCTAssertTrue(coordinator.status.description.contains("1.50s"))
        try assertAudioPreserved()
    }

    func testSynchronousCompletionLeavesNoStaleHandle() {
        loadAndExpect(.ready)
        engine.completeSynchronouslyWith = .transcript(text: "done", confidence: nil)
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .transcribed(text: "done", confidence: nil, durationSeconds: 0))
        coordinator.cancelTranscription()
        XCTAssertEqual(engine.handles[0].cancelCount, 0, "a finished run has nothing to cancel")
        XCTAssertEqual(coordinator.status, .transcribed(text: "done", confidence: nil, durationSeconds: 0))
    }

    func testEmptyTranscriptIsFailureNotSuccess() throws {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        engine.completions[0](.transcript(text: "   ", confidence: nil))
        XCTAssertEqual(coordinator.status, .failed(reason: "No speech recognized", durationSeconds: 0))
        try assertAudioPreserved()
    }

    func testEngineFailureKeepsAudioAndAllowsRetry() throws {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        clock += 0.25
        engine.completions[0](.failure(reason: "kAFAssistantErrorDomain 1110: No speech detected"))
        XCTAssertEqual(
            coordinator.status,
            .failed(reason: "kAFAssistantErrorDomain 1110: No speech detected", durationSeconds: 0.25)
        )
        try assertAudioPreserved()
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .inProgress)
        engine.completions[1](.transcript(text: "second try", confidence: nil))
        XCTAssertEqual(coordinator.status, .transcribed(text: "second try", confidence: nil, durationSeconds: 0))
    }

    func testSecondStartWhileRunningIsIgnored() {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        coordinator.startTranscription()
        XCTAssertEqual(engine.recognizeCalls.count, 1)
    }

    func testDuplicateCompletionIsIgnored() {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        engine.completions[0](.transcript(text: "first", confidence: nil))
        engine.completions[0](.failure(reason: "late duplicate"))
        XCTAssertEqual(coordinator.status, .transcribed(text: "first", confidence: nil, durationSeconds: 0))
    }

    func testAudioDeletedBeforeStartIsReportedNotTranscribed() throws {
        loadAndExpect(.ready)
        try FileManager.default.removeItem(at: audioURL)
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .failed(reason: "Audio file is missing or empty", durationSeconds: 0))
        XCTAssertTrue(engine.recognizeCalls.isEmpty)
    }

    // MARK: cancellation

    func testCancelKeepsCancelledStateAgainstLateCallbacks() throws {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        coordinator.cancelTranscription()
        XCTAssertEqual(coordinator.status, .cancelled)
        XCTAssertEqual(engine.handles[0].cancelCount, 1)
        clock += 5
        engine.completions[0](.failure(reason: "kAFAssistantErrorDomain 216: Request was canceled"))
        XCTAssertEqual(coordinator.status, .cancelled, "the cancel callback must not overwrite the cancelled state")
        engine.completions[0](.transcript(text: "late result", confidence: nil))
        XCTAssertEqual(coordinator.status, .cancelled)
        try assertAudioPreserved()
    }

    func testCancelWhenNotRunningDoesNothing() {
        loadAndExpect(.ready)
        coordinator.cancelTranscription()
        XCTAssertEqual(coordinator.status, .ready)
        XCTAssertTrue(engine.handles.isEmpty)
    }

    func testStaleCallbackFromCancelledRunCannotCompleteNewRun() {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        coordinator.cancelTranscription()
        coordinator.startTranscription()
        XCTAssertEqual(coordinator.status, .inProgress)
        engine.completions[0](.transcript(text: "from the cancelled run", confidence: nil))
        XCTAssertEqual(coordinator.status, .inProgress)
        engine.completions[1](.transcript(text: "from the new run", confidence: nil))
        XCTAssertEqual(coordinator.status, .transcribed(text: "from the new run", confidence: nil, durationSeconds: 0))
    }

    // MARK: loading, locale and interruptions

    func testLoadRejectsMissingAndEmptyFilesAndKeepsPreviousAudio() throws {
        loadAndExpect(.ready)
        let missing = directory.appendingPathComponent("missing.wav")
        XCTAssertNotNil(coordinator.loadAudio(at: missing))
        let empty = directory.appendingPathComponent("empty.wav")
        try Data().write(to: empty)
        XCTAssertNotNil(coordinator.loadAudio(at: empty))
        XCTAssertEqual(coordinator.audioURL, audioURL)
        XCTAssertEqual(coordinator.status, .ready)
    }

    func testLoadAndLocaleChangeRefusedWhileRunning() {
        loadAndExpect(.ready)
        coordinator.startTranscription()
        XCTAssertNotNil(coordinator.loadAudio(at: audioURL))
        XCTAssertFalse(coordinator.setLocale("fr-FR"))
        XCTAssertEqual(coordinator.localeIdentifier, "en-US")
        XCTAssertEqual(coordinator.status, .inProgress)
    }

    func testInterruptionsAreRecordedOnlyDuringARun() {
        loadAndExpect(.ready)
        coordinator.noteInterruption(.applicationBackgrounded)
        XCTAssertTrue(coordinator.interruptions.isEmpty)
        coordinator.startTranscription()
        coordinator.noteInterruption(.applicationBackgrounded)
        coordinator.noteInterruption(.protectedDataUnavailable)
        XCTAssertEqual(coordinator.interruptions, [.applicationBackgrounded, .protectedDataUnavailable])
        XCTAssertTrue(coordinator.displayText.contains("app moved to background"))
        XCTAssertTrue(coordinator.displayText.contains("device locked"))
        engine.completions[0](.transcript(text: "ok", confidence: nil))
        coordinator.noteInterruption(.applicationBackgrounded)
        XCTAssertEqual(coordinator.interruptions.count, 2)
    }

    func testOnChangeFiresForStatusChanges() {
        var changeCount = 0
        coordinator.onChange = { changeCount += 1 }
        loadAndExpect(.ready)
        coordinator.startTranscription()
        engine.completions[0](.transcript(text: "x", confidence: nil))
        XCTAssertGreaterThanOrEqual(changeCount, 3)
    }

    // MARK: fixture lookup

    func testFixtureLookupIsDeterministic() throws {
        let fixtureDirectory = directory.appendingPathComponent("fixtures")
        try FileManager.default.createDirectory(at: fixtureDirectory, withIntermediateDirectories: true)
        XCTAssertNil(TranscriptionFixture.locate(in: fixtureDirectory))
        try audioBytes.write(to: fixtureDirectory.appendingPathComponent("zzz-other.wav"))
        try audioBytes.write(to: fixtureDirectory.appendingPathComponent("transcription-fixture-old.wav"))
        XCTAssertNil(TranscriptionFixture.locate(in: fixtureDirectory), "only the exact fixture name is ever picked")
        let generated = TranscriptionFixture.generatedURL(in: fixtureDirectory)
        XCTAssertEqual(generated.lastPathComponent, "transcription-fixture.caf")
        try audioBytes.write(to: generated)
        XCTAssertEqual(TranscriptionFixture.locate(in: fixtureDirectory), generated)
        let supplied = fixtureDirectory.appendingPathComponent("transcription-fixture.wav")
        try audioBytes.write(to: supplied)
        XCTAssertEqual(TranscriptionFixture.locate(in: fixtureDirectory), supplied, "operator-supplied wav wins over the generated caf")
    }
}

/// Exercises the real Speech framework wrapper. Results that depend on the host (permission, installed models) are
/// printed as observations rather than asserted, because the hosted simulator is not evidence of device behaviour.
final class SpeechFrameworkEngineAvailabilityTests: XCTestCase {
    func testUnsupportedLocaleHasNoRecognizerAndIsReportedAsUnsupportedLanguage() {
        let engine = SpeechFrameworkEngine()
        let snapshot = engine.snapshot(localeIdentifier: "zz-ZZ")
        XCTAssertFalse(snapshot.recognizerExists)
        XCTAssertFalse(snapshot.isAvailable)
        XCTAssertFalse(snapshot.supportsOnDevice)
        XCTAssertNotNil(evaluateReadiness(snapshot, localeIdentifier: "zz-ZZ"))
    }

    func testRecognizeFailsClosedWhenOnDeviceRecognitionIsUnavailable() {
        let engine = SpeechFrameworkEngine()
        let completed = expectation(description: "completion")
        var outcome: RecognitionOutcome?
        let handle = engine.recognize(
            audioURL: URL(fileURLWithPath: "/nonexistent/transcription-fixture.wav"),
            localeIdentifier: "zz-ZZ"
        ) { result in
            outcome = result
            completed.fulfill()
        }
        wait(for: [completed], timeout: 5)
        handle.cancel()
        guard case .failure(let reason)? = outcome else {
            return XCTFail("expected a failure outcome, got \(String(describing: outcome))")
        }
        XCTAssertTrue(reason.contains("no request was made"), reason)
    }

    func testObserveHostSpeechAvailability() {
        let engine = SpeechFrameworkEngine()
        let snapshot = engine.snapshot(localeIdentifier: "en-US")
        let readiness = evaluateReadiness(snapshot, localeIdentifier: "en-US").map { $0.description } ?? "ready"
        print("OBSERVED host speech availability for en-US: \(snapshot) -> \(readiness)")
    }
}
