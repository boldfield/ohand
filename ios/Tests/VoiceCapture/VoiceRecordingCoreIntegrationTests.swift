import AVFoundation
import SQLite3
import XCTest
@testable import OhAnd
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Drives the recording controller over the real ingress service and the real Rust core on the simulator, with a
/// synthetic recorder. A process death is simulated by discarding every object and closing the core: only files
/// survive. Device lock, real interruptions and termination are physical-matrix evidence, not shown here.
final class VoiceRecordingCoreIntegrationTests: IngressStorageTestCase {
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var runtimes: [Runtime] = []

    /// One "process": core, ingress service, handoff and recording controller over the protected store locations.
    private final class Runtime {
        let core: CoreHandle
        let engine = SyntheticVoiceEngine()
        let permission = FakeMicrophonePermission()
        let notificationCenter = NotificationCenter()
        let poller = ManualPoller()
        private(set) var controller: VoiceRecordingController?
        private(set) var service: ForegroundIngressService?
        private(set) var outcomes: [VoiceCaptureOutcome] = []

        init(layout: ProtectedStorageLayout, context: IngressCaptureContext, protection: ProtectedStorageService) throws {
            core = try CoreHandle(path: layout.databaseURL.path)
            do {
                let importer = try CoreIngressImporter(core: core)
                let ingress = try ForegroundIngressService(layout: layout, importer: importer, protection: protection)
                service = ingress
                var environment = VoiceRecordingEnvironment.live { _ in
                    let outcome = protection.applyPolicy(to: .ingressInProgressAudio)
                    if !outcome.isFullyConfigured { throw NSError(domain: "synthetic.voice-protection", code: 1) }
                }
                environment.isForeground = { true }
                let manualPoller = poller
                environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
                let newController = VoiceRecordingController(
                    inProgressDirectory: layout.directory(for: .ingressInProgressAudio),
                    engine: engine,
                    permission: permission,
                    handoff: IngressVoiceHandoff(service: ingress, context: context),
                    environment: environment,
                    notificationCenter: notificationCenter,
                    makeCaptureID: { "voice-core-1" })
                newController.onOutcome = { [unowned self] in outcomes.append($0) }
                controller = newController
            } catch {
                core.close()
                throw error
            }
        }

        func terminate() {
            controller = nil
            service = nil
            core.close()
        }
    }

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let seedCore = try CoreHandle(path: layout.databaseURL.path)
        seedCore.close()
        try withDatabase { database in
            let statement = "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-personal', 'route-personal', 'personal', '[]', '2026-10-08T09:00:00Z')"
            XCTAssertEqual(sqlite3_exec(database, statement, nil, nil, nil), SQLITE_OK)
        }
    }

    override func tearDown() {
        runtimes.forEach { $0.terminate() }
        runtimes = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every opened core was closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was freed")
        super.tearDown()
    }

    // MARK: Helpers

    private func withDatabase(_ body: (OpaquePointer?) throws -> Void) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        try body(database)
    }

    private func scalarText(_ query: String, captureID: String) throws -> String? {
        var text: String?
        try withDatabase { database in
            var statement: OpaquePointer?
            XCTAssertEqual(sqlite3_prepare_v2(database, query, -1, &statement, nil), SQLITE_OK)
            defer { sqlite3_finalize(statement) }
            sqlite3_bind_text(statement, 1, captureID, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
            if sqlite3_step(statement) == SQLITE_ROW, let raw = sqlite3_column_text(statement, 0) {
                text = String(cString: raw)
            }
        }
        return text
    }

    private func itemCount(captureID: String) throws -> Int {
        Int(try scalarText("SELECT COUNT(*) FROM items WHERE capture_id = ?", captureID: captureID) ?? "-1") ?? -1
    }

    private func audioReference(captureID: String) throws -> String? {
        try scalarText("SELECT audio_reference FROM captures WHERE capture_id = ?", captureID: captureID)
    }

    private func launch(boundary: FileAttributeBoundary = FileManagerAttributeBoundary()) throws -> Runtime {
        let protection = ProtectedStorageService(layout: layout, boundary: boundary)
        let runtime = try Runtime(layout: layout, context: context, protection: protection)
        runtimes.append(runtime)
        return runtime
    }

    private func waitForOutcome(_ runtime: Runtime) throws -> VoiceCaptureOutcome {
        let deadline = Date().addingTimeInterval(30)
        while runtime.outcomes.isEmpty && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return try XCTUnwrap(runtime.outcomes.first, "no outcome was delivered")
    }

    private func startRecording(_ runtime: Runtime) throws {
        var result: Result<VoiceRecordingStarted, VoiceStartFailure>?
        try XCTUnwrap(runtime.controller).start { result = $0 }
        XCTAssertEqual(try XCTUnwrap(result).get().captureID, "voice-core-1")
    }

    // MARK: Tests

    func testStoppedRecordingIsSavedInTheCoreWithAProtectedValidAudioReference() throws {
        let boundary = RecordingFileAttributeBoundary(wrapping: FileManagerAttributeBoundary())
        let runtime = try launch(boundary: boundary)
        try startRecording(runtime)

        try XCTUnwrap(runtime.controller).stop()
        let outcome = try waitForOutcome(runtime)

        guard case .saved(let summary, let acknowledgment) = outcome else { return XCTFail("expected saved, got \(outcome)") }
        XCTAssertTrue(summary.isComplete)
        XCTAssertTrue(summary.protectionApplied)
        XCTAssertFalse(acknowledgment.itemID.isEmpty)
        XCTAssertEqual(try itemCount(captureID: "voice-core-1"), 1)
        let reference = try XCTUnwrap(try audioReference(captureID: "voice-core-1"))
        XCTAssertEqual(reference, "FinalizedAudio/voice-core-1.wav")
        let resolved = layout.rootDirectory.appendingPathComponent(reference)
        XCTAssertEqual(try AVAudioFile(forReading: resolved).length, 8000, "the core's reference opens the recorded audio")
        XCTAssertFalse(exists(inProgressAudioURL("voice-core-1.wav")), "exactly one file holds the audio")
        XCTAssertFalse(exists(recordURL("voice-core-1")), "the staging record is removed after confirmation")

        let inProgressPolicy = StoreProtectionPolicy.policy(for: .ingressInProgressAudio)
        let recordingPath = inProgressAudioURL("voice-core-1.wav").path
        XCTAssertEqual(boundary.protectionByPath[recordingPath], inProgressPolicy.fileProtection,
                       "the recording received the in-progress store's class while it was being written")
        XCTAssertEqual(boundary.exclusionByPath[layout.directory(for: .ingressInProgressAudio).path], true)
    }

    func testInterruptedRecordingSurvivesProcessDeathBeforeImportAndSavesOnce() throws {
        let beforeDeath = try launch()
        try startRecording(beforeDeath)
        beforeDeath.core.close()
        beforeDeath.notificationCenter.post(
            name: AVAudioSession.interruptionNotification, object: nil,
            userInfo: [AVAudioSessionInterruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue])

        let firstOutcome = try waitForOutcome(beforeDeath)

        guard case .keptForRetry(let summary, .importUnconfirmed) = firstOutcome else {
            return XCTFail("expected keptForRetry, got \(firstOutcome)")
        }
        XCTAssertEqual(summary.end, .audioInterrupted)
        XCTAssertFalse(summary.isComplete)
        XCTAssertTrue(exists(finalizedAudioURL("voice-core-1.wav")), "the interrupted prefix is durable")
        XCTAssertTrue(exists(recordURL("voice-core-1")))
        beforeDeath.terminate()

        let relaunched = try launch()
        var report: IngressRecoveryReport?
        try XCTUnwrap(relaunched.service).recover { report = $0 }
        let deadline = Date().addingTimeInterval(30)
        while report == nil && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }

        guard case .saved? = try XCTUnwrap(report).entries.first?.outcome else { return XCTFail("recovery did not save") }
        XCTAssertEqual(try itemCount(captureID: "voice-core-1"), 1)
        XCTAssertEqual(try audioReference(captureID: "voice-core-1"), "FinalizedAudio/voice-core-1.wav")
        XCTAssertEqual(try AVAudioFile(forReading: finalizedAudioURL("voice-core-1.wav")).length, 8000)
    }
}

/// The real recorder engine cannot capture on a hosted simulator without a granted microphone, and a test must never
/// raise the system prompt. This pins the refusal path and, when a developer machine has granted access, the start and
/// close path; recorded audio quality is physical-matrix evidence.
final class AVAudioRecorderVoiceEngineTests: XCTestCase {
    func testRealEngineRefusesWithoutGrantedPermissionAndCreatesNoFile() throws {
        let permission = SystemMicrophonePermission()
        let engine = AVAudioRecorderVoiceEngine(permission: permission, finishWaitSeconds: 0.2)
        let destination = FileManager.default.temporaryDirectory.appendingPathComponent("voice-engine-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: destination) }

        if permission.status == .granted {
            try engine.start(writingTo: destination, maxDuration: 1)
            _ = engine.stopAndClose()
        } else {
            XCTAssertThrowsError(try engine.start(writingTo: destination, maxDuration: 1)) { error in
                XCTAssertEqual(error as? VoiceEngineStartError, VoiceEngineStartError(reason: .microphoneDenied))
            }
            XCTAssertFalse(FileManager.default.fileExists(atPath: destination.path))
            XCTAssertFalse(engine.stopAndClose(), "closing an engine that never started reports no clean finish")
        }
    }
}
