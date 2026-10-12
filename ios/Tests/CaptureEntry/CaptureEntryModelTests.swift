import XCTest
@testable import OhAnd

final class CaptureEntryModelTests: XCTestCase {
    private var textIngress: ScriptedTextCaptureIngress!
    private var voice: FakeVoiceController!
    private var reader: ScriptedProgressReader!
    private var access = CaptureEntryFixtures.openAccess
    private var captureEndCount = 0
    private var model: CaptureEntryModel!

    override func setUp() {
        super.setUp()
        textIngress = ScriptedTextCaptureIngress()
        voice = FakeVoiceController()
        reader = ScriptedProgressReader()
        access = CaptureEntryFixtures.openAccess
        captureEndCount = 0
        model = makeModel(reader: reader)
    }

    private func makeModel(reader: CaptureProgressReading?) -> CaptureEntryModel {
        let created = CaptureEntryModel(
            textIngress: textIngress, voice: voice, progressReader: reader, defaultMode: .silent,
            access: { [unowned self] in access }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-capture-1" }, now: { Date(timeIntervalSince1970: 1_790_000_000) })
        created.captureDidEnd = { [unowned self] in captureEndCount += 1 }
        return created
    }

    private func saveText(_ text: String = "buy oat milk", itemID: String = "item-1") {
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: itemID, alreadySaved: false)]
        model.textModel.updateText(text)
        model.saveText()
    }

    private var message: String { model.acknowledgmentMessage ?? "" }

    // MARK: Acknowledgment wording at the surface

    func testASilentSaveSaysSavedNotProcessedAndNoReminderConfirmed() {
        saveText()

        XCTAssertEqual(message, "Text saved. It has not been processed yet. No reminder has been confirmed.")
        XCTAssertEqual(model.textModel.text, "", "the field is free for the next capture once the text is durable")
        XCTAssertEqual(captureEndCount, 1, "the bounded background completion is asked for right after the save")
    }

    func testSavingIsShownWhileTheImportIsInFlight() {
        textIngress.holdsCompletions = true
        saveText()

        XCTAssertEqual(model.acknowledgment?.save, .saving)

        textIngress.release()
        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-1")
    }

    func testAKeptCaptureIsNeverCalledSaved() {
        textIngress.outcomes = [.keptOnDevice(captureID: "text-capture-1")]
        model.textModel.updateText("water the plants")
        model.saveText()

        XCTAssertTrue(message.contains("not confirmed saved"), message)
        XCTAssertTrue(message.contains("No reminder has been confirmed"), message)
        XCTAssertEqual(captureEndCount, 1)
        XCTAssertTrue(reader.requestedItemIDs.isEmpty, "there is no item to read")
    }

    func testAFailedStageKeepsTheTextAndDoesNotEndTheCapture() {
        textIngress.outcomes = [.notStaged(captureID: "text-capture-1")]
        model.textModel.updateText("book dentist")
        model.saveText()

        XCTAssertEqual(message, "Not saved. Your text is still here.")
        XCTAssertEqual(model.textModel.text, "book dentist")
        XCTAssertEqual(captureEndCount, 0)
    }

    func testEditingAfterAnAcknowledgmentClearsIt() {
        textIngress.outcomes = [.notStaged(captureID: "text-capture-1")]
        model.textModel.updateText("book dentist")
        model.saveText()
        XCTAssertNotNil(model.acknowledgment)

        model.textModel.updateText("book dentist friday")

        XCTAssertNil(model.acknowledgment)
    }

    func testSavingNeedsNoProgressReaderOrManagementShell() {
        model = makeModel(reader: nil)
        saveText()

        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-1")
        XCTAssertEqual(captureEndCount, 1)
    }

    // MARK: Progress

    func testAnInstalledReminderIsWordedWithItsResolvedTime() {
        reader.result = .status(CaptureEntryFixtures.installedStatus())

        saveText()

        XCTAssertEqual(reader.requestedItemIDs, ["item-1"])
        XCTAssertTrue(message.hasPrefix("Text saved. Reminder set for "), message)
    }

    func testAnUnreadableStatusLeavesTheDefaultAcknowledgment() {
        reader.result = .unavailable

        saveText()

        XCTAssertEqual(message, "Text saved. It has not been processed yet. No reminder has been confirmed.")
    }

    func testOnlyTheSavedItemIsRead() {
        reader.result = .status(CaptureEntryFixtures.installedStatus())

        saveText(itemID: "item-42")
        model.refreshProgress()

        XCTAssertEqual(Set(reader.requestedItemIDs), ["item-42"])
    }

    func testNothingIsReadWhileReadAccessIsNotAuthorized() {
        access = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: false)
        reader.result = .status(CaptureEntryFixtures.installedStatus())

        saveText()
        model.refreshProgress()

        XCTAssertTrue(reader.requestedItemIDs.isEmpty)
        XCTAssertEqual(message, "Text saved. It has not been processed yet. No reminder has been confirmed.")
        XCTAssertEqual(captureEndCount, 1, "capture does not depend on read access")
    }

    func testNothingIsReadBeforeTheFirstUnlockAndTheNoticeSaysWhy() {
        access = CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: true)

        model.present(CaptureEntryFixtures.presentation(access: access))
        saveText()

        XCTAssertTrue(reader.requestedItemIDs.isEmpty)
        XCTAssertNotNil(model.protectedDataNotice)
    }

    func testAnAnswerThatArrivesAfterRelockIsDropped() {
        reader.holdsCompletions = true
        reader.result = .status(CaptureEntryFixtures.installedStatus())
        saveText()
        XCTAssertEqual(reader.requestedItemIDs, ["item-1"])

        access = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: false)
        reader.release()

        XCTAssertFalse(message.contains("Reminder set"), message)
        XCTAssertTrue(message.contains("No reminder has been confirmed"), message)
    }

    func testLosingReadAccessRemovesWhatWasLearnedFromStoredData() {
        reader.result = .status(CaptureEntryFixtures.installedStatus())
        saveText()
        XCTAssertTrue(message.contains("Reminder set"))

        access = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: false)
        model.refreshAccess()

        XCTAssertFalse(message.contains("Reminder set"), message)
    }

    func testAStaleAnswerForAnEarlierCaptureCannotOverwriteALaterOne() {
        reader.holdsCompletions = true
        reader.result = .status(CaptureEntryFixtures.installedStatus())
        saveText(itemID: "item-1")

        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-2", alreadySaved: false)]
        model.textModel.updateText("second")
        model.saveText()
        reader.release()

        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-2")
        XCTAssertEqual(reader.requestedItemIDs, ["item-1", "item-2"])
    }

    // MARK: Voice

    func testASavedRecordingSaysAudioSavedAndNotTranscribed() {
        model.switchMode(to: .voice)
        model.startRecording()
        XCTAssertEqual(voice.startCount, 1)
        XCTAssertEqual(model.recorderState, .recording(captureID: "voice-1", startedAt: Date(timeIntervalSince1970: 1_790_000_000)))

        voice.finish(with: FakeVoiceController.saved())

        XCTAssertEqual(message, "Audio saved. It has not been transcribed yet. No reminder has been confirmed.")
        XCTAssertEqual(captureEndCount, 1)
        XCTAssertEqual(reader.requestedItemIDs, ["item-1"])
    }

    func testABackgroundInterruptionNeverLeavesABareSuccess() {
        model.startRecording()

        voice.finish(with: FakeVoiceController.saved(end: .leftForeground))

        XCTAssertTrue(message.contains("left the screen"), message)
        XCTAssertTrue(message.contains("No reminder has been confirmed"), message)
        XCTAssertFalse(message.contains("Reminder set"), message)
    }

    func testAnUnconfirmedRecordingIsKeptNotSaved() {
        model.startRecording()

        voice.finish(with: .keptForRetry(FakeVoiceController.summary(end: .deviceLocking), .importUnconfirmed))

        XCTAssertTrue(message.contains("not confirmed saved"), message)
        XCTAssertTrue(message.contains("locked"), message)
        XCTAssertTrue(message.contains("No reminder has been confirmed"), message)
        XCTAssertEqual(captureEndCount, 1)
    }

    func testACancelledRecordingIsNotSavedAndDoesNotEndACapture() {
        model.startRecording()

        voice.finish(with: .retainedUnsubmitted(FakeVoiceController.summary(end: .cancelledByUser)))

        XCTAssertEqual(message, "Recording cancelled. The audio is kept on this device and was not saved.")
        XCTAssertEqual(captureEndCount, 0)
    }

    func testAStartFailureSaysNothingWasRecorded() {
        voice.startResult = .failure(.microphoneDenied)

        model.startRecording()

        XCTAssertTrue(model.voiceNotice?.contains("Nothing was recorded") ?? false)
        XCTAssertNil(model.acknowledgment)
        XCTAssertEqual(model.recorderState, .idle)
    }

    func testStopAndCancelReachTheRecorder() {
        model.stopRecording()
        model.cancelRecording()

        XCTAssertEqual(voice.stopCount, 1)
        XCTAssertEqual(voice.cancelCount, 1)
    }

    // MARK: Presentation

    func testAnEntryWhileIdleAppliesItsModeAndClearsThePreviousAcknowledgment() {
        saveText()

        model.present(CaptureEntryFixtures.presentation(mode: .voice))

        XCTAssertEqual(model.mode, .voice)
        XCTAssertNil(model.acknowledgment, "an entry does not show earlier captures")
    }

    func testAnEntryNeverDiscardsInputInProgress() {
        model.textModel.updateText("half a thought")

        model.present(CaptureEntryFixtures.presentation(mode: .voice))

        XCTAssertEqual(model.mode, .silent)
        XCTAssertEqual(model.textModel.text, "half a thought")
    }

    func testAnEntryDuringARecordingKeepsTheRecording() {
        model.startRecording()

        model.present(CaptureEntryFixtures.presentation(mode: .silent))

        XCTAssertEqual(voice.cancelCount, 0)
        XCTAssertEqual(voice.stopCount, 0)
        XCTAssertEqual(model.mode, .silent)
    }

    func testTheModeCannotBeSwitchedWhileInputIsInProgress() {
        model.textModel.updateText("typing")

        model.switchMode(to: .voice)

        XCTAssertEqual(model.mode, .silent)
    }

    func testPresentingAnEntryReadsNoStoredData() {
        access = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: false)

        model.present(CaptureEntryFixtures.presentation(access: access))

        XCTAssertTrue(reader.requestedItemIDs.isEmpty)
        XCTAssertNil(model.acknowledgment)
    }
}
