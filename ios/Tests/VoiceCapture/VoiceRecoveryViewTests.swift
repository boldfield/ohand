import AVFoundation
import SwiftUI
import UIKit
import XCTest
@testable import OhAnd
@testable import OhAndServices

/// Hosts the real recovery view in a window over the real coordinator, controller and ingress writer. Each control the
/// user would tap is found on screen by the identifier the view gives it, and is then driven through the model call the
/// view's button makes, so what is shown and what happens are checked together.
final class VoiceRecoveryViewTests: VoiceCaptureTestCase {
    private static let standardPhone = CGSize(width: 390, height: 844)
    private static let smallPhone = CGSize(width: 320, height: 568)
    private static let minimumTouchTarget: CGFloat = 44

    private var retainedWindow: UIWindow?
    private var model: VoiceRecoveryModel!
    private var frames: [String: CGRect] = [:]

    override func tearDown() {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        model = nil
        super.tearDown()
    }

    /// A relaunch: new controller and coordinator over the same storage, then the recovery surface appears.
    private func relaunchAndShow(
        category: ContentSizeCategory = .large,
        windowSize: CGSize = VoiceRecoveryViewTests.standardPhone
    ) {
        controller = makeController()
        coordinator = makeCoordinator()
        model = VoiceRecoveryModel(coordinator: coordinator, controller: controller)
        let previousStateHandler = controller.onStateChange
        controller.onStateChange = { [unowned self] state in
            previousStateHandler?(state)
            model.recorderStateChanged(state)
        }
        let previousOutcomeHandler = controller.onOutcome
        controller.onOutcome = { [unowned self] outcome in
            previousOutcomeHandler?(outcome)
            model.recorderEnded(with: outcome)
        }
        let root = VoiceRecoveryView(model: model, layoutReporter: { [unowned self] in frames = $0 })
            .environment(\.sizeCategory, category)
        let hosting = UIHostingController(rootView: root)
        let window = UIWindow(frame: CGRect(origin: .zero, size: windowSize))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        settle()
    }

    private func settle() {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

    private func control(_ action: VoiceRecoveryAction, _ captureID: String) -> CGRect {
        frames[VoiceRecoveryView.actionIdentifier(action, fileName: "\(captureID).wav")] ?? .zero
    }

    private func assertTappable(_ rect: CGRect, _ label: String, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNotEqual(rect, .zero, "\(label) is shown", file: file, line: line)
        XCTAssertGreaterThanOrEqual(rect.height, Self.minimumTouchTarget, "\(label) is a full-size touch target", file: file, line: line)
        XCTAssertGreaterThanOrEqual(rect.width, Self.minimumTouchTarget, file: file, line: line)
    }

    private func cancelledRecordingLeftBehind() throws {
        _ = try startRecording()
        controller.cancel()
        guard case .retainedUnsubmitted = try lastOutcome() else { return XCTFail("expected a retained recording") }
    }

    /// A stop whose import is not confirmed leaves a staged record, which is what makes a read failure matter.
    private func cancelledRecordingLeftBehindStaged() throws {
        _ = try startRecording()
        controller.stop()
        guard case .keptForRetry = try lastOutcome() else { return XCTFail("expected a staged capture") }
    }

    // MARK: Re-entry

    func testReentryAfterACancellationShowsTheRecordingWithFinishAddAndDeleteControls() throws {
        try cancelledRecordingLeftBehind()

        relaunchAndShow()

        XCTAssertEqual(model.rows.count, 1)
        XCTAssertNotEqual(frames[VoiceRecoveryView.rowIdentifier("voice-test-1.wav")] ?? .zero, .zero, "the recording is shown")
        for action in [VoiceRecoveryAction.finish, .continueRecording, .delete] {
            assertTappable(control(action, "voice-test-1"), action.label)
        }
        XCTAssertTrue(model.rows[0].detail.contains("not saved until you finish"), "the row never claims it was saved")
        XCTAssertNil(frames[VoiceRecoveryView.emptyIdentifier])
        XCTAssertTrue(importer.importedRecords.isEmpty, "showing the recording submitted nothing")
    }

    func testReentryWithNothingLeftSaysSo() throws {
        relaunchAndShow()

        XCTAssertNotEqual(frames[VoiceRecoveryView.emptyIdentifier] ?? .zero, .zero)
        XCTAssertTrue(model.rows.isEmpty)
    }

    func testReentryAfterAKilledProcessShowsTheDurationAndSizeReadFromTheFile() throws {
        try leaveRecording("voice-left-1", frames: 24000)

        relaunchAndShow()

        let row = try XCTUnwrap(model.rows.first { $0.fileName == "voice-left-1.wav" })
        let size = try XCTUnwrap(FileManager.default.attributesOfItem(atPath: recordingFile("voice-left-1").path)[.size] as? NSNumber)
        XCTAssertTrue(row.detail.contains("0:02"), "24000 frames at 16 kHz is 1.5 seconds, shown as 0:02: \(row.detail)")
        XCTAssertTrue(row.detail.contains(VoiceRecoveryMessages.size(size.int64Value)), row.detail)
        assertTappable(control(.finish, "voice-left-1"), "finish")
    }

    func testAnUnreadableRecordingOffersOnlyDelete() throws {
        try Data([0x00, 0x01, 0x02]).write(to: recordingFile("voice-garbled"))

        relaunchAndShow()

        assertTappable(control(.delete, "voice-garbled"), "delete")
        XCTAssertEqual(control(.finish, "voice-garbled"), .zero)
        XCTAssertEqual(control(.continueRecording, "voice-garbled"), .zero)
        XCTAssertTrue(model.rows[0].detail.contains("cannot be read as audio"))
    }

    func testUnknownIngressStateShowsAnExplanationInsteadOfAList() throws {
        try cancelledRecordingLeftBehindStaged()
        try leaveRecording("voice-left-1")
        failingFileSystem.failing = [.read]

        relaunchAndShow()

        XCTAssertTrue(model.rows.isEmpty)
        XCTAssertNotEqual(frames[VoiceRecoveryView.noticeIdentifier(0)] ?? .zero, .zero)
        XCTAssertTrue(model.notices.contains { $0.contains("could not be checked") })
        XCTAssertNil(frames[VoiceRecoveryView.emptyIdentifier], "an unknown state is never shown as empty")
        XCTAssertTrue(exists(recordingFile("voice-left-1")))
    }

    func testControlsStayFullSizeAtTheLargestAccessibilityTextOnASmallPhone() throws {
        try leaveRecording("voice-left-1")

        relaunchAndShow(category: .accessibilityExtraExtraExtraLarge, windowSize: Self.smallPhone)

        for action in [VoiceRecoveryAction.finish, .continueRecording, .delete] {
            let rect = control(action, "voice-left-1")
            assertTappable(rect, action.label)
            XCTAssertLessThanOrEqual(rect.maxX, Self.smallPhone.width + 0.5, "\(action.label) is not cut off sideways")
            XCTAssertGreaterThanOrEqual(rect.minX, -0.5)
        }
    }

    // MARK: Actions

    func testFinishFromTheShownControlSavesOnlyAfterTheCoreConfirms() throws {
        try cancelledRecordingLeftBehind()
        relaunchAndShow()
        assertTappable(control(.finish, "voice-test-1"), "finish")

        importer.holdsCompletions = true
        confirmNextImport("voice-test-1")
        model.perform(.finish, onFileName: "voice-test-1.wav")
        XCTAssertEqual(model.busyFileName, "voice-test-1.wav")
        XCTAssertFalse(model.isEnabled(.finish), "a second tap cannot start a second finish")
        XCTAssertFalse((model.status ?? "").hasPrefix("Saved"), "nothing is saved before the core answers")
        importer.release()
        settle()

        XCTAssertTrue((model.status ?? "").contains("Saved"))
        XCTAssertNil(model.busyFileName)
        XCTAssertEqual(importer.importedRecords.count, 1)
        XCTAssertEqual(frames[VoiceRecoveryView.rowIdentifier("voice-test-1.wav")] ?? .zero, .zero, "the saved recording is no longer offered")
    }

    func testFinishThatTheCoreDoesNotConfirmIsReportedAsKeptNotSaved() throws {
        try leaveRecording("voice-left-1")
        relaunchAndShow()

        model.perform(.finish, onFileName: "voice-left-1.wav")
        settle()

        let status = try XCTUnwrap(model.status)
        XCTAssertFalse(status.contains("Saved"), status)
        XCTAssertTrue(status.contains("Not saved yet"), status)
    }

    func testDeleteNeedsAConfirmationAndOnlyThenRemovesTheOneRecording() throws {
        try leaveRecording("voice-left-1")
        try leaveRecording("voice-left-2")
        relaunchAndShow()
        assertTappable(control(.delete, "voice-left-1"), "delete")

        model.perform(.delete, onFileName: "voice-left-1.wav")
        settle()
        XCTAssertEqual(model.deletionRequestFileName, "voice-left-1.wav")
        XCTAssertTrue(exists(recordingFile("voice-left-1")), "asking to delete deletes nothing")

        model.cancelDeletion()
        settle()
        XCTAssertNil(model.deletionRequestFileName)
        XCTAssertTrue(exists(recordingFile("voice-left-1")), "keeping it leaves it alone")

        model.perform(.delete, onFileName: "voice-left-1.wav")
        model.confirmDeletion(fileName: "voice-left-2.wav")
        XCTAssertTrue(exists(recordingFile("voice-left-1")), "a confirmation for another recording is ignored")
        XCTAssertTrue(exists(recordingFile("voice-left-2")))
        model.confirmDeletion(fileName: "voice-left-1.wav")
        settle()

        XCTAssertFalse(exists(recordingFile("voice-left-1")))
        XCTAssertTrue(exists(recordingFile("voice-left-2")), "nothing else was deleted")
        XCTAssertTrue((model.status ?? "").hasPrefix("Deleted"))
        XCTAssertEqual(model.rows.map { $0.fileName }, ["voice-left-2.wav"])
        XCTAssertEqual(frames[VoiceRecoveryView.rowIdentifier("voice-left-1.wav")] ?? .zero, .zero)
    }

    func testAddingAudioShowsStopAndCancelThenReportsTheCancelledOutcomeHonestly() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        engine.framesWritten = 4000
        relaunchAndShow()

        model.perform(.continueRecording, onFileName: "voice-left-1.wav")
        settle()

        XCTAssertTrue(model.isRecording)
        assertTappable(frames[VoiceRecoveryView.stopIdentifier] ?? .zero, "stop")
        assertTappable(frames[VoiceRecoveryView.cancelIdentifier] ?? .zero, "cancel")
        XCTAssertFalse(model.isEnabled(.continueRecording), "one recording at a time")
        XCTAssertEqual(frames[VoiceRecoveryView.rowIdentifier("voice-left-1.wav")] ?? .zero, .zero, "the recording being written is not offered")

        model.cancelRecording()
        settle()

        XCTAssertFalse(model.isRecording)
        XCTAssertNil(frames[VoiceRecoveryView.stopIdentifier])
        XCTAssertTrue((model.status ?? "").hasPrefix("Cancelled"))
        XCTAssertFalse((model.status ?? "").contains("Saved"))
        XCTAssertEqual(try audioFrames(recordingFile("voice-left-1")), 12000, "the added audio was joined, not discarded")
        XCTAssertTrue(importer.importedRecords.isEmpty, "a cancellation submits nothing")
        XCTAssertTrue((model.rows.first?.detail ?? "").contains("0:01"))
        assertTappable(control(.finish, "voice-left-1"), "finish after the cancellation")
    }

    func testAfterAFailedJoinTheSurfaceShowsBothRecordingsAndWithholdsAddAudio() throws {
        try leaveRecording("voice-left-1", frames: 8000)
        engine.framesWritten = 4000
        relaunchAndShow()
        model.perform(.continueRecording, onFileName: "voice-left-1.wav")
        joinShouldFail = true
        model.stopRecording()
        joinShouldFail = false
        settle()

        XCTAssertTrue((model.status ?? "").contains("could not be joined"))
        XCTAssertTrue((model.status ?? "").contains("earlier recording is unchanged"))
        XCTAssertEqual(Set(model.rows.map { $0.fileName }), ["voice-left-1.wav", "voice-left-1-continued.wav"])
        assertTappable(control(.finish, "voice-left-1"), "finish")
        assertTappable(control(.finish, "voice-left-1-continued"), "finish the added audio")
        XCTAssertEqual(control(.continueRecording, "voice-left-1"), .zero, "add audio is withheld while the added audio waits")
        XCTAssertTrue(try XCTUnwrap(model.rows.first { $0.fileName == "voice-left-1.wav" }).detail.contains("separate recording"))
    }

    func testAddedAudioStillWarnsOnScreenAfterTheRecordingItWasAddedToIsFinished() throws {
        try leaveRecording("voice-left-1", frames: 12000)
        try leaveRecording("voice-left-1-continued", frames: 4000)
        relaunchAndShow()
        XCTAssertEqual(Set(model.rows.map { $0.fileName }), ["voice-left-1.wav", "voice-left-1-continued.wav"])

        confirmNextImport("voice-left-1")
        assertTappable(control(.finish, "voice-left-1"), "finish")
        model.perform(.finish, onFileName: "voice-left-1.wav")
        settle()

        XCTAssertTrue((model.status ?? "").hasPrefix("Saved"))
        XCTAssertEqual(model.rows.map { $0.fileName }, ["voice-left-1-continued.wav"])
        let row = try XCTUnwrap(model.rows.first)
        XCTAssertEqual(row.title, "Added audio kept separately")
        XCTAssertTrue(row.detail.contains("save the same audio twice"), "the duplicate risk is still shown")
        XCTAssertNotEqual(frames[VoiceRecoveryView.rowIdentifier("voice-left-1-continued.wav")] ?? .zero, .zero)
        assertTappable(control(.delete, "voice-left-1-continued"), "delete the added audio")
        XCTAssertEqual(importer.importedRecords.count, 1)
    }

    // MARK: Normal capture

    func testStartingANewRecordingNeedsNothingFromTheRecoverySurface() throws {
        try leaveRecording("voice-left-1")
        relaunchAndShow()
        XCTAssertEqual(model.rows.count, 1)

        let started = try startRecording()
        confirmNextImport(started.captureID)
        controller.stop()
        settle()

        guard case .saved = try lastOutcome() else { return XCTFail("expected saved, got \(String(describing: outcomes.last))") }
        XCTAssertTrue(exists(recordingFile("voice-left-1")), "the unfinished recording is still offered")
        XCTAssertEqual(model.rows.map { $0.fileName }, ["voice-left-1.wav"])
    }
}
