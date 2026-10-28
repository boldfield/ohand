import XCTest
@testable import OhAnd
@testable import OhAndCoreBridge
@testable import OhAndServices

final class CaptureEntryVoiceAutoStartTests: XCTestCase {
    private var voice: FakeVoiceController!
    private var textIngress: ScriptedTextCaptureIngress!
    private var model: CaptureEntryModel!
    private var handoff: CaptureEntryHandoff!
    private var session: CaptureEntrySession!

    override func setUp() {
        super.setUp()
        voice = FakeVoiceController()
        textIngress = ScriptedTextCaptureIngress()
        model = CaptureEntryModel(
            textIngress: textIngress, voice: voice, defaultMode: .silent,
            access: { CaptureEntryFixtures.openAccess }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-capture-1" })
        handoff = CaptureEntryHandoff(notificationCenter: NotificationCenter())
        session = CaptureEntrySession(
            handoff: handoff, defaultMode: { .silent }, access: { CaptureEntryFixtures.openAccess },
            notificationCenter: NotificationCenter())
    }

    private func presentRequest(_ mode: CaptureEntryMode?) {
        handoff.register(source: .controlIntent, requestedMode: mode)
        if let presentation = session.willEnterForeground() { model.present(presentation) }
    }

    func testTheVoiceControlsIntentRegistersAVoiceRequest() async throws {
        _ = CaptureEntryHandoff.live.takePending()
        _ = try await OpenCaptureIntent(target: .voice).perform()

        let request = try XCTUnwrap(CaptureEntryHandoff.live.takePending())
        XCTAssertEqual(request.requestedMode, .voice)
    }

    func testTheNoteControlsIntentRegistersASilentRequest() async throws {
        _ = CaptureEntryHandoff.live.takePending()
        _ = try await OpenCaptureIntent(target: .silent).perform()

        XCTAssertEqual(CaptureEntryHandoff.live.takePending()?.requestedMode, .silent)
    }

    func testAnExplicitVoiceRequestWithGrantedPermissionStartsExactlyOneRecording() {
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 1)
        XCTAssertEqual(model.mode, .voice)
        XCTAssertTrue(model.isRecording)
    }

    func testASecondForegroundEntryWithNothingPendingStartsNothingMore() {
        presentRequest(.voice)
        voice.finish(with: FakeVoiceController.saved())
        session.didEnterBackground()

        if let presentation = session.willEnterForeground() { model.present(presentation) }

        XCTAssertEqual(voice.startCount, 1)
    }

    func testUndeterminedPermissionShowsVoiceWithoutStarting() {
        voice.microphonePermission = .undetermined
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 0)
        XCTAssertEqual(model.mode, .voice)
    }

    func testDeniedPermissionStartsNothing() {
        voice.microphonePermission = .denied
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 0)
        XCTAssertEqual(model.mode, .voice)
    }

    func testTypingInProgressIsNotInterrupted() {
        model.textModel.updateText("half a thought")
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 0)
        XCTAssertEqual(model.textModel.text, "half a thought")
        XCTAssertEqual(model.mode, .silent)
    }

    func testARecordingInProgressIsNotRestarted() {
        voice.state = .recording(captureID: "voice-0", startedAt: Date(timeIntervalSince1970: 1_790_000_000))
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 0)
    }

    func testASaveInProgressIsNotInterrupted() {
        textIngress.holdsCompletions = true
        model.textModel.updateText("buy oat milk")
        model.saveText()
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 0)
    }

    func testARequestWithoutAModeStartsNothing() {
        presentRequest(nil)

        XCTAssertEqual(voice.startCount, 0)
    }

    func testASilentRequestStartsNothing() {
        presentRequest(.silent)

        XCTAssertEqual(voice.startCount, 0)
        XCTAssertEqual(model.mode, .silent)
    }

    func testAShortcutURLWithVoiceModeStartsRecordingOnceForeground() throws {
        _ = session.willEnterForeground()
        let url = try XCTUnwrap(URL(string: "ohand-capture://capture?mode=voice"))
        if let presentation = session.receive(url: url) { model.present(presentation) }

        XCTAssertEqual(voice.startCount, 1)
    }

    func testAStartFailureShowsTheNoticeAndLeavesTheSurfaceUsable() {
        voice.startResult = .failure(.recorderUnavailable)
        presentRequest(.voice)

        XCTAssertEqual(voice.startCount, 1)
        XCTAssertEqual(model.voiceNotice, CaptureEntryModel.startFailureMessage(.recorderUnavailable))
        XCTAssertTrue(model.isIdle)
    }

    func testAnAutoStartedRecordingKeepsItsAnnouncementPendingUntilTheViewAsksOnce() {
        // The cold-launch order: recording starts before any view exists to observe a change.
        presentRequest(.voice)

        XCTAssertTrue(model.recordingStartAnnouncementPending)
        XCTAssertTrue(model.consumeRecordingStartAnnouncement())
        XCTAssertFalse(model.consumeRecordingStartAnnouncement())
    }

    func testNoAnnouncementIsPendingWhenNothingStarted() {
        voice.microphonePermission = .denied
        presentRequest(.voice)

        XCTAssertFalse(model.recordingStartAnnouncementPending)
        XCTAssertFalse(model.consumeRecordingStartAnnouncement())
    }

    func testAMicTapStartAlsoRaisesTheAnnouncement() {
        model.startRecording()

        XCTAssertTrue(model.consumeRecordingStartAnnouncement())
    }

    func testTheAssembledVoiceServicesReportTheInjectedPermission() throws {
        let storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("auto-start-permission-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: storageRoot) }
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        defer { root.shutDown() }
        let permission = FakeMicrophonePermission(status: .denied)
        let voice = VoiceCaptureServices(
            root: root, engine: SyntheticVoiceEngine(), permission: permission,
            notificationCenter: NotificationCenter())

        XCTAssertEqual(voice.recorder.microphonePermission, .denied)
        permission.status = .granted
        XCTAssertEqual(voice.recorder.microphonePermission, .granted)
    }
}
