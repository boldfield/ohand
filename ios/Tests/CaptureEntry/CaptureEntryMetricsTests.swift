import XCTest
@testable import OhAnd
@testable import OhAndCoreBridge

/// Drives the real handoff, session and model and observes what they report as metrics.
final class CaptureEntryMetricsTests: XCTestCase {
    private var clock: TestUptimeClock!
    private var metrics: RecordingMetrics!
    private var handoff: CaptureEntryHandoff!
    private var textIngress: ScriptedTextCaptureIngress!
    private var voice: FakeVoiceController!
    private var access = CaptureEntryFixtures.openAccess
    private var session: CaptureEntrySession!
    private var model: CaptureEntryModel!

    override func setUp() {
        super.setUp()
        clock = TestUptimeClock()
        metrics = RecordingMetrics()
        access = CaptureEntryFixtures.openAccess
        handoff = CaptureEntryHandoff(
            notificationCenter: NotificationCenter(), makeEntryID: { UUID().uuidString },
            uptimeNanoseconds: { [unowned self] in clock.now() })
        textIngress = ScriptedTextCaptureIngress()
        voice = FakeVoiceController()
        session = CaptureEntrySession(
            handoff: handoff, defaultMode: { .silent }, access: { [unowned self] in access },
            notificationCenter: NotificationCenter())
        model = CaptureEntryModel(
            textIngress: textIngress, voice: voice, defaultMode: .silent, access: { [unowned self] in access },
            wording: CaptureEntryFixtures.wording, makeCaptureID: { "text-capture-1" },
            now: { Date(timeIntervalSince1970: 1_790_000_000) }, metrics: metrics,
            uptimeNanoseconds: { [unowned self] in clock.now() })
    }

    private func present() {
        guard let presentation = session.willEnterForeground() else { return XCTFail("an entry was pending") }
        model.present(presentation)
    }

    // MARK: trigger to ready

    func testColdTriggerToReadyIsMeasuredFromTheSystemTriggerToTheReadySurface() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 420)
        present()
        XCTAssertTrue(metrics.records.isEmpty, "presenting alone is not ready")
        clock.advance(milliseconds: 30)
        model.surfaceDidBecomeReady()
        XCTAssertEqual(metrics.records, [.triggerToReady(.cold, milliseconds: 450)])
    }

    func testAnEntryIsMeasuredOnceNoMatterHowOftenTheViewReportsReady() {
        handoff.register(source: .shortcutURL, requestedMode: .voice)
        present()
        clock.advance(milliseconds: 100)
        model.surfaceDidBecomeReady()
        clock.advance(milliseconds: 900)
        model.surfaceDidBecomeReady()
        XCTAssertEqual(metrics.records, [.triggerToReady(.cold, milliseconds: 100)])
    }

    func testALaterEntryInTheSameSessionIsWarmAndMeasuredSeparately() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        present()
        model.surfaceDidBecomeReady()
        session.didEnterBackground()
        clock.advance(milliseconds: 5_000)

        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 80)
        present()
        clock.advance(milliseconds: 20)
        model.surfaceDidBecomeReady()
        XCTAssertEqual(
            metrics.records, [.triggerToReady(.cold, milliseconds: 0), .triggerToReady(.warm, milliseconds: 100)])
    }

    func testAnEntryWhileProtectedDataIsUnavailableIsALockedHandoff() {
        access = CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: false)
        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 60)
        present()
        model.surfaceDidBecomeReady()
        XCTAssertEqual(metrics.records, [.triggerToReady(.lockedHandoff, milliseconds: 60)])
    }

    func testASecondPressBeforePresentationKeepsTheEarliestTrigger() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 200)
        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 50)
        present()
        model.surfaceDidBecomeReady()
        XCTAssertEqual(metrics.records, [.triggerToReady(.cold, milliseconds: 250)])
    }

    func testAPlainLaunchAndAPresentationWithoutATriggerMeasureNothing() {
        XCTAssertNil(session.willEnterForeground(), "a plain launch presents nothing")
        model.surfaceDidBecomeReady()
        model.present(CaptureEntryFixtures.presentation())
        model.surfaceDidBecomeReady()
        XCTAssertTrue(metrics.records.isEmpty)
    }

    // MARK: end of input to save

    func testTextSaveIsMeasuredFromTheSaveToTheDurableAcknowledgmentNotFromTheTrigger() {
        handoff.register(source: .controlIntent, requestedMode: nil)
        clock.advance(milliseconds: 700)
        present()
        model.surfaceDidBecomeReady()
        clock.advance(milliseconds: 9_000)

        textIngress.holdsCompletions = true
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        clock.advance(milliseconds: 45)
        textIngress.release()
        XCTAssertEqual(
            metrics.records,
            [.triggerToReady(.cold, milliseconds: 700), .endOfInputToSave(.text, milliseconds: 45)])
    }

    func testAKeptTextCaptureIsADurableEndAndAFailedStageIsNot() {
        textIngress.outcomes = [.notStaged(captureID: "text-capture-1")]
        model.textModel.updateText("book dentist")
        model.saveText()
        XCTAssertTrue(metrics.records.isEmpty, "nothing was saved")

        textIngress.outcomes = [.keptOnDevice(captureID: "text-capture-1")]
        model.saveText()
        XCTAssertEqual(metrics.records, [.endOfInputToSave(.text, milliseconds: 0)])
    }

    func testVoiceIsMeasuredFromStopToTheDurableAcknowledgment() {
        model.startRecording()
        clock.advance(milliseconds: 30_000)
        model.stopRecording()
        clock.advance(milliseconds: 250)
        voice.finish(with: FakeVoiceController.saved())
        XCTAssertEqual(metrics.records, [.endOfInputToSave(.voice, milliseconds: 250)])
    }

    func testARecordingTheLimitEndedIsMeasuredFromWhenItStartedFinishing() {
        model.startRecording()
        clock.advance(milliseconds: 60_000)
        voice.state = .finishing(captureID: "voice-1")
        clock.advance(milliseconds: 120)
        voice.finish(with: FakeVoiceController.saved(end: .durationLimit))
        XCTAssertEqual(metrics.records, [.endOfInputToSave(.voice, milliseconds: 120)])
    }

    func testACancelledRecordingAndAFailedOneAreNotSaveSamples() {
        model.startRecording()
        model.cancelRecording()
        voice.finish(with: .retainedUnsubmitted(FakeVoiceController.summary()))
        XCTAssertTrue(metrics.records.isEmpty)
    }

    func testTheSurfaceWorksWithoutAMetricsSink() {
        let bare = CaptureEntryModel(
            textIngress: textIngress, voice: voice, defaultMode: .silent, access: { [unowned self] in access },
            wording: CaptureEntryFixtures.wording, makeCaptureID: { "text-capture-1" },
            now: { Date(timeIntervalSince1970: 1_790_000_000) })
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        bare.textModel.updateText("water the plants")
        bare.saveText()
        bare.surfaceDidBecomeReady()
        XCTAssertEqual(bare.acknowledgment?.savedItemID, "item-1")
    }
}
