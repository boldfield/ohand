import SwiftUI
import UIKit
import XCTest
@testable import OhAnd

/// Hosts the real composer home and checks what is on screen: the header, the composer, Settings or Done, the Saved
/// pill and the short messages. A bottom safe-area inset stands in for the keyboard, as in `TextCaptureLayoutTests`.
final class CaptureEntryViewTests: XCTestCase {
    private static let smallPhone = CGSize(width: 320, height: 568)
    private static let standardPhone = CGSize(width: 390, height: 844)
    private static let tablet = CGSize(width: 1024, height: 1366)
    private static let smallPhoneKeyboardHeight: CGFloat = 260
    private static let standardPhoneKeyboardHeight: CGFloat = 336
    private static let minimumTouchTarget: CGFloat = 44

    private var textIngress: ScriptedTextCaptureIngress!
    private var voice: FakeVoiceController!
    private var model: CaptureEntryModel!
    private var frames: [String: CGRect] = [:]
    private var retainedWindow: UIWindow?

    override func setUp() {
        super.setUp()
        textIngress = ScriptedTextCaptureIngress()
        voice = FakeVoiceController()
        frames = [:]
        model = makeModel(access: CaptureEntryFixtures.openAccess)
    }

    override func tearDown() {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        super.tearDown()
    }

    private func makeModel(access: CaptureEntryAccess, progressReader: CaptureProgressReading? = nil) -> CaptureEntryModel {
        CaptureEntryModel(
            textIngress: textIngress, voice: voice, progressReader: progressReader, defaultMode: .silent,
            access: { access }, wording: CaptureEntryFixtures.wording,
            makeCaptureID: { "text-capture-1" }, now: { Date(timeIntervalSince1970: 1_790_000_000) })
    }

    private func host(
        category: ContentSizeCategory = .large,
        windowSize: CGSize = CaptureEntryViewTests.standardPhone,
        keyboardHeight: CGFloat = 0,
        sizeClass: UserInterfaceSizeClass = .compact
    ) {
        let root = CaptureEntryView(model: model, layoutReporter: { [unowned self] in frames = $0 })
            .environment(\.sizeCategory, category)
            .environment(\.horizontalSizeClass, sizeClass)
        let controller = UIHostingController(rootView: root)
        controller.additionalSafeAreaInsets = UIEdgeInsets(top: 0, left: 0, bottom: keyboardHeight, right: 0)
        let window = UIWindow(frame: CGRect(origin: .zero, size: windowSize))
        window.rootViewController = controller
        window.makeKeyAndVisible()
        retainedWindow = window
        settle(until: "the header and composer have reported their frames") {
            self.frames[CaptureEntryView.headerIdentifier] != nil && self.frames[CaptureEntryView.composerIdentifier] != nil
        }
        settle()
    }

    private func settle(for seconds: TimeInterval = 0.3) {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: seconds))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

    /// Lays the hosted view out and turns the run loop until the condition holds; fails the test naming it otherwise.
    private func settle(
        until condition: String, timeout: TimeInterval = 5, file: StaticString = #filePath, line: UInt = #line,
        _ holds: () -> Bool
    ) {
        waitUntilPumping(condition, timeout: timeout, file: file, line: line) {
            self.retainedWindow?.rootViewController?.view.layoutIfNeeded()
            return holds()
        }
    }

    private func isDrawn(_ identifier: String) -> Bool { frames[identifier].map { $0 != .zero } ?? false }

    private func frame(_ identifier: String) -> CGRect { frames[identifier] ?? .zero }

    private func assertHeaderAndComposerVisible(
        windowSize: CGSize, keyboardHeight: CGFloat, file: StaticString = #filePath, line: UInt = #line
    ) {
        let header = frame(CaptureEntryView.headerIdentifier)
        let composer = frame(CaptureEntryView.composerIdentifier)
        XCTAssertNotEqual(header, .zero, "the header is on screen", file: file, line: line)
        XCTAssertNotEqual(composer, .zero, "the composer is on screen", file: file, line: line)
        XCTAssertGreaterThanOrEqual(header.minY, 0, file: file, line: line)
        XCTAssertLessThanOrEqual(header.maxY, composer.minY + 0.5, "the composer does not cover the header", file: file, line: line)
        XCTAssertLessThanOrEqual(
            composer.maxY, windowSize.height - keyboardHeight + 0.5, "the keyboard does not cover the composer",
            file: file, line: line)
        XCTAssertGreaterThanOrEqual(composer.minX, 0, file: file, line: line)
        XCTAssertLessThanOrEqual(composer.maxX, windowSize.width + 0.5, file: file, line: line)
        XCTAssertGreaterThanOrEqual(header.minX, 0, file: file, line: line)
        XCTAssertLessThanOrEqual(header.maxX, windowSize.width + 0.5, file: file, line: line)
    }

    /// Checks the controls the composer actually drew, not only which state it was given.
    private func assertComposerState(
        _ state: String, file: StaticString = #filePath, line: UInt = #line
    ) {
        for other in ["rest", "typing", "recording"] {
            let shown = frames[CaptureEntryView.composerStateIdentifier(other)] != nil
            XCTAssertEqual(shown, other == state, "composer \(other) is \(other == state ? "" : "not ")shown", file: file, line: line)
        }
        let expected: Set<String>
        switch state {
        case "rest": expected = [HomeComposer.placeholderIdentifier, HomeComposer.fieldIdentifier, HomeComposer.recordIdentifier]
        case "typing": expected = [HomeComposer.fieldIdentifier, HomeComposer.recordIdentifier, HomeComposer.saveIdentifier]
        default: expected = [HomeComposer.discardIdentifier, HomeComposer.stopIdentifier]
        }
        let controls = [
            HomeComposer.placeholderIdentifier, HomeComposer.fieldIdentifier, HomeComposer.recordIdentifier,
            HomeComposer.saveIdentifier, HomeComposer.discardIdentifier, HomeComposer.stopIdentifier,
        ]
        for control in controls {
            let drawn = frames[control].map { $0 != .zero } ?? false
            XCTAssertEqual(
                drawn, expected.contains(control), "\(control) is \(expected.contains(control) ? "" : "not ")drawn while \(state)",
                file: file, line: line)
            XCTAssertNil(
                frames[HomeComposer.disabledIdentifier(control)], "\(control) is not disabled while \(state)",
                file: file, line: line)
        }
        if expected.contains(HomeComposer.recordIdentifier) || expected.contains(HomeComposer.stopIdentifier) {
            let action = frame(expected.contains(HomeComposer.stopIdentifier) ? HomeComposer.stopIdentifier : HomeComposer.recordIdentifier)
            XCTAssertGreaterThanOrEqual(action.height, Self.minimumTouchTarget, file: file, line: line)
        }
    }

    private func assertComposerStateName(_ state: String, file: StaticString = #filePath, line: UInt = #line) {
        for other in ["rest", "typing", "recording"] {
            XCTAssertEqual(frames[CaptureEntryView.composerStateIdentifier(other)] != nil, other == state, file: file, line: line)
        }
    }

    /// While Record and Save cannot act they are drawn disabled; the text field stays enabled.
    private func assertEntryActionsDisabled(saveIsShown: Bool, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNotNil(frames[HomeComposer.disabledIdentifier(HomeComposer.recordIdentifier)], "Record is disabled", file: file, line: line)
        XCTAssertNil(frames[HomeComposer.recordIdentifier], file: file, line: line)
        if saveIsShown {
            XCTAssertNotNil(frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)], "Save is disabled", file: file, line: line)
            XCTAssertNil(frames[HomeComposer.saveIdentifier], file: file, line: line)
        }
        XCTAssertNotNil(frames[HomeComposer.fieldIdentifier], "the text field stays enabled", file: file, line: line)
        XCTAssertNil(frames[HomeComposer.disabledIdentifier(HomeComposer.fieldIdentifier)], file: file, line: line)
    }

    private func waitUntilFocused(timeout: TimeInterval = 5) -> Bool {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while frames[HomeComposer.focusedIdentifier] == nil, Date() < deadline { settle(for: 0.1) }
        return frames[HomeComposer.focusedIdentifier] != nil
    }

    private func post(_ name: Notification.Name, until condition: String, _ holds: () -> Bool) {
        NotificationCenter.default.post(name: name, object: nil)
        settle(until: condition, holds)
        settle()
    }

    // MARK: Header and composer under the keyboard

    func testHeaderAndComposerAreOnScreenAtRest() {
        host()

        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: 0)
        XCTAssertNotEqual(frame(HomeHeader.settingsIdentifier), .zero, "Settings is shown at rest")
        XCTAssertEqual(frame(HomeHeader.doneIdentifier), .zero)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "nothing has been saved")
    }

    func testHeaderAndComposerStayVisibleUnderAKeyboardAtEveryTextSize() {
        let phones: [(CGSize, CGFloat)] = [
            (Self.smallPhone, Self.smallPhoneKeyboardHeight), (Self.standardPhone, Self.standardPhoneKeyboardHeight),
        ]
        for (size, keyboardHeight) in phones {
            for category in [ContentSizeCategory.large, .accessibilityExtraExtraExtraLarge] {
                model = makeModel(access: CaptureEntryFixtures.openAccess)
                model.textModel.updateText("buy oat milk and a very long list of things that wraps over several lines")
                host(category: category, windowSize: size, keyboardHeight: keyboardHeight)

                assertHeaderAndComposerVisible(windowSize: size, keyboardHeight: keyboardHeight)
            }
        }
    }

    // MARK: Settings and Done

    func testSettingsBecomesDoneWhileTheKeyboardIsUpAndBack() {
        host()
        XCTAssertNotEqual(frame(HomeHeader.settingsIdentifier), .zero)

        post(UIResponder.keyboardWillShowNotification, until: "Done replaces Settings") {
            self.isDrawn(HomeHeader.doneIdentifier) && self.frames[HomeHeader.settingsIdentifier] == nil
        }
        let done = frame(HomeHeader.doneIdentifier)
        XCTAssertNotEqual(done, .zero, "Done is shown while the keyboard is up")
        XCTAssertGreaterThanOrEqual(done.height, Self.minimumTouchTarget)
        XCTAssertNil(frames[HomeHeader.settingsIdentifier], "Settings is replaced by Done")

        post(UIResponder.keyboardWillHideNotification, until: "Settings is back and Done is gone") {
            self.isDrawn(HomeHeader.settingsIdentifier) && self.frames[HomeHeader.doneIdentifier] == nil
        }
        XCTAssertNotEqual(frame(HomeHeader.settingsIdentifier), .zero, "Settings is back once the keyboard is gone")
        XCTAssertNil(frames[HomeHeader.doneIdentifier])
    }

    // MARK: Width

    func testTheComposerIsCentredWithAMaximumWidthAtRegularWidth() {
        host(windowSize: Self.tablet, sizeClass: .regular)

        let composer = frame(CaptureEntryView.composerIdentifier)
        XCTAssertNotEqual(composer, .zero)
        XCTAssertLessThanOrEqual(composer.width, CaptureEntryView.composerMaximumWidth + 0.5)
        XCTAssertGreaterThanOrEqual(composer.width, 440, "the composer fills its column up to the maximum")
        XCTAssertEqual(composer.midX, Self.tablet.width / 2, accuracy: 1, "the composer is centred")
    }

    func testTheComposerFillsAPhoneWidthAtCompactWidth() {
        host()

        XCTAssertGreaterThan(frame(CaptureEntryView.composerIdentifier).width, Self.standardPhone.width - 80)
    }

    // MARK: Composer states

    func testRestTypingAndRecordingEachShowTheirOwnComposer() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        let rest = frame(CaptureEntryView.composerIdentifier)
        assertComposerState("rest")

        model.textModel.updateText("buy oat milk")
        settle(until: "the typing composer is drawn with Save") {
            self.frames[CaptureEntryView.composerStateIdentifier("typing")] != nil && self.isDrawn(HomeComposer.saveIdentifier)
        }
        assertComposerState("typing")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.textModel.updateText("")
        model.startRecording()
        settle(until: "recording started and the recording composer is drawn") {
            self.voice.startCount == 1 && self.frames[CaptureEntryView.composerStateIdentifier("recording")] != nil
                && self.isDrawn(HomeComposer.stopIdentifier)
        }
        XCTAssertEqual(voice.startCount, 1)
        assertComposerState("recording")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.cancelRecording()
        voice.state = .idle
        settle(until: "the rest composer is drawn") {
            self.frames[CaptureEntryView.composerStateIdentifier("rest")] != nil && self.isDrawn(HomeComposer.recordIdentifier)
        }
        assertComposerState("rest")
        XCTAssertEqual(frame(CaptureEntryView.composerIdentifier).width, rest.width, accuracy: 0.5)
    }

    func testAnInFlightSaveIsReportedAndDisablesRecordAndSaveButNotTheField() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.holdsCompletions = true
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the in-flight save is stated and Record and Save are drawn disabled") {
            self.isDrawn(CaptureEntryView.messageIdentifier)
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.recordIdentifier)] != nil
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)] != nil
        }

        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the in-flight save is stated")
        assertComposerStateName("typing")
        assertEntryActionsDisabled(saveIsShown: true)
        model.startRecording()
        XCTAssertEqual(voice.startCount, 0, "Record does nothing while a save is in flight")

        textIngress.release()
        settle(until: "the save settled and the rest composer is drawn") {
            self.model.textModel.text == "" && self.frames[CaptureEntryView.composerStateIdentifier("rest")] != nil
                && self.isDrawn(HomeComposer.recordIdentifier)
        }
        assertComposerState("rest")
    }

    func testASaveKeepsTheComposerFocused() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        XCTAssertTrue(waitUntilFocused(), "the silent composer takes focus when the home appears")
        textIngress.holdsCompletions = true
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the save is in flight") { self.model.acknowledgment?.save == .saving }
        settle()
        XCTAssertNotNil(frames[HomeComposer.focusedIdentifier], "the field keeps focus while the save is in flight")

        textIngress.release()
        settle(until: "the save settled and the composer cleared") {
            self.model.textModel.text == "" && self.frames[CaptureEntryView.composerStateIdentifier("rest")] != nil
        }
        settle()
        XCTAssertEqual(model.textModel.text, "", "the composer clears")
        XCTAssertNotNil(frames[HomeComposer.focusedIdentifier], "and keeps focus, so the keyboard stays up")
    }

    func testWhitespaceOnlyTextDrawsSaveDisabled() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.textModel.updateText("   ")
        settle(until: "Save is drawn disabled") {
            self.frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)] != nil
        }
        assertComposerStateName("typing")
        XCTAssertNotNil(
            frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)], "Save is disabled with nothing to save")
        XCTAssertNil(frames[HomeComposer.saveIdentifier])
        XCTAssertNotNil(frames[HomeComposer.recordIdentifier], "Record still works")
        XCTAssertNotNil(frames[HomeComposer.fieldIdentifier], "the text field stays enabled")

        model.textModel.updateText("  buy oat milk ")
        settle(until: "Save is drawn enabled") { self.isDrawn(HomeComposer.saveIdentifier) }
        XCTAssertNotNil(frames[HomeComposer.saveIdentifier], "Save is enabled once there is something to save")
        XCTAssertNil(frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)])
    }

    func testRecordAndSaveAreDisabledWhileTheRecorderStartsOrFinishes() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)

        voice.state = .requestingPermission
        settle(until: "waiting for the microphone is stated and Record is drawn disabled") {
            self.isDrawn(CaptureEntryView.messageIdentifier)
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.recordIdentifier)] != nil
        }
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "waiting for the microphone is stated")
        assertComposerStateName("rest")
        assertEntryActionsDisabled(saveIsShown: false)

        model.textModel.updateText("buy oat milk")
        settle(until: "the typing composer is drawn with Save disabled") {
            self.frames[CaptureEntryView.composerStateIdentifier("typing")] != nil
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)] != nil
        }
        assertComposerStateName("typing")
        assertEntryActionsDisabled(saveIsShown: true)

        voice.state = .finishing(captureID: "voice-1")
        settle(until: "finishing the recording is stated with Record and Save disabled") {
            self.isDrawn(CaptureEntryView.messageIdentifier)
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.recordIdentifier)] != nil
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)] != nil
        }
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "finishing the recording is stated")
        assertEntryActionsDisabled(saveIsShown: true)

        voice.state = .idle
        settle(until: "the typing composer is drawn with Record and Save enabled") {
            self.isDrawn(HomeComposer.recordIdentifier) && self.isDrawn(HomeComposer.saveIdentifier)
                && self.frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)] == nil
        }
        assertComposerState("typing")
    }

    // MARK: Saved pill and messages

    func testAConfirmedSaveClearsTheTextShowsTheSavedPillAndStillStatesTheReminder() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the composer cleared and the Saved pill and the message are shown") {
            self.model.textModel.text == "" && self.frames[CaptureEntryView.savedPillIdentifier] != nil
                && self.isDrawn(CaptureEntryView.messageIdentifier)
        }
        let pillShownAt = Date()

        XCTAssertEqual(model.textModel.text, "", "the composer clears")
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the Saved pill is shown")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the reminder and processing state is visible")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        // The fade is timed from the pill's appearance, which on a loaded simulator is later than the save; wait for it
        // to go rather than for a fixed time after the save.
        settle(
            until: "the Saved pill has faded",
            timeout: max(0.1, CaptureEntryView.savedPillDuration + 3 - Date().timeIntervalSince(pillShownAt))
        ) { self.frames[CaptureEntryView.savedPillIdentifier] == nil }
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the pill fades after about two seconds")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the message outlives the pill")
    }

    func testTheSavedPillMarksOnlyANewSave() {
        let saved = CaptureSaveState.saved(itemID: "item-1", alreadySaved: false)
        let observed = { (save: CaptureSaveState?) in CaptureEntryView.ObservedSave(save: save) }
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: nil, to: saved), .keep, "an acknowledgment already on screen")
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(.saving), to: saved), .show)
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(nil), to: saved), .show, "a recording that saved")
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(saved), to: saved), .keep, "a progress refresh")
        XCTAssertEqual(
            CaptureEntryView.savedPillChange(from: observed(saved), to: .saved(itemID: "item-2", alreadySaved: false)), .show)
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(saved), to: .saving), .hide)
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(saved), to: .notSaved), .hide)
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(saved), to: .keptOnDevice), .hide)
        XCTAssertEqual(CaptureEntryView.savedPillChange(from: observed(saved), to: nil), .hide)
    }

    func testAProgressOrForegroundRefreshNeitherReplaysNorExtendsTheSavedPill() {
        let reader = ScriptedProgressReader()
        reader.result = .status(CaptureEntryFixtures.installedStatus())
        reader.holdsCompletions = true
        model = makeModel(access: CaptureEntryFixtures.openAccess, progressReader: reader)
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the Saved pill is shown and the progress read is held") {
            self.frames[CaptureEntryView.savedPillIdentifier] != nil && !reader.requestedItemIDs.isEmpty
        }
        let savedAt = Date()
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the Saved pill is shown")

        settle(for: 1.2)
        reader.release()
        settle(until: "the delayed progress read republished the acknowledgment") { self.model.acknowledgment?.progress != nil }
        XCTAssertNotNil(model.acknowledgment?.progress, "the delayed progress read republished the acknowledgment")

        // The pill's own timer runs from when it appeared (`savedAt`); the progress read must not have moved it.
        settle(
            until: "the Saved pill has faded on its own timer",
            timeout: max(0.1, CaptureEntryView.savedPillDuration + 0.6 - Date().timeIntervalSince(savedAt))
        ) { self.frames[CaptureEntryView.savedPillIdentifier] == nil }
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the progress read did not extend the pill")

        let readsBeforeRefresh = reader.requestedItemIDs.count
        model.refreshProgress()
        settle(until: "the foreground refresh asked for a read") { reader.requestedItemIDs.count > readsBeforeRefresh }
        reader.release()
        settle()
        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-1")
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "a foreground refresh does not replay the pill")
    }

    func testASecondAttemptHidesTheSavedPillAndAFailureIsNeverShownBesideIt() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the first capture shows the Saved pill") { self.frames[CaptureEntryView.savedPillIdentifier] != nil }
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the first capture is Saved")

        textIngress.holdsCompletions = true
        textIngress.outcomes = [.notStaged(captureID: "text-capture-1")]
        model.textModel.updateText("call the dentist")
        model.saveText()
        settle(until: "the second save is in flight and the pill is gone") {
            self.model.acknowledgment?.save == .saving && self.frames[CaptureEntryView.savedPillIdentifier] == nil
        }
        settle()
        XCTAssertEqual(model.acknowledgment?.save, .saving)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the pill goes once the next save starts")

        textIngress.release()
        settle(until: "the second save failed and the failure is stated") {
            self.model.acknowledgment?.save == .notSaved && self.isDrawn(CaptureEntryView.messageIdentifier)
        }
        settle()
        XCTAssertEqual(model.acknowledgment?.save, .notSaved)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "Saved is never shown beside Not saved")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the failure is stated")
        XCTAssertEqual(model.textModel.text, "call the dentist", "the text is kept")

        settle(for: CaptureEntryView.savedPillDuration)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the first save's timer does not bring it back")
    }

    func testAVoiceAttemptThatFailsToStartHidesTheSavedPill() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the first capture shows the Saved pill") { self.frames[CaptureEntryView.savedPillIdentifier] != nil }
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the first capture is Saved")

        voice.startResult = .failure(.microphoneDenied)
        model.startRecording()
        settle(until: "the start failure is stated") {
            self.voice.startCount == 1 && self.isDrawn(CaptureEntryView.noticeIdentifier)
        }
        settle()
        XCTAssertEqual(voice.startCount, 1)
        XCTAssertEqual(model.recorderState, .idle, "nothing was recorded")
        XCTAssertEqual(model.acknowledgment?.savedItemID, "item-1", "the start failure leaves the acknowledgment alone")
        XCTAssertNotEqual(frame(CaptureEntryView.noticeIdentifier), .zero, "the start failure is stated")
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "Saved is never shown beside the start failure")

        settle(for: CaptureEntryView.savedPillDuration)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the first save's timer does not bring it back")
    }

    func testAFailedSaveKeepsTheTextAndIsShownBesideAStandingNotice() {
        model = makeModel(access: CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: true))
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle(until: "the protected-data notice and the failure are shown") {
            self.isDrawn(CaptureEntryView.noticeIdentifier) && self.isDrawn(CaptureEntryView.messageIdentifier)
        }
        settle()

        XCTAssertEqual(model.textModel.text, "buy oat milk", "the text is kept")
        XCTAssertNotEqual(frame(CaptureEntryView.noticeIdentifier), .zero, "the protected-data notice is shown")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "and so is what happened to the save")
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "an unsaved capture is never called Saved")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)
    }

    func testBothStandingNoticesAreShownTogether() {
        let voiceNotice = CaptureEntryModel.startFailureMessage(.microphoneDenied)
        let combined = CaptureEntryView.standingNotice(voiceNotice: voiceNotice, protectedDataNoticeIsActive: true)
        XCTAssertTrue(combined?.contains(voiceNotice) ?? false, "the voice notice is shown")
        XCTAssertTrue(combined?.contains(CaptureMessages.protectedDataNotice) ?? false, "and so is the warning that saving may fail")
        XCTAssertEqual(CaptureEntryView.standingNotice(voiceNotice: voiceNotice, protectedDataNoticeIsActive: false), voiceNotice)
        XCTAssertEqual(
            CaptureEntryView.standingNotice(voiceNotice: nil, protectedDataNoticeIsActive: true), CaptureMessages.protectedDataNotice)
        XCTAssertNil(CaptureEntryView.standingNotice(voiceNotice: nil, protectedDataNoticeIsActive: false))

        model = makeModel(access: CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: true))
        voice.startResult = .failure(.microphoneDenied)
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        model.startRecording()
        settle(until: "the hosted home shows the notices") { self.isDrawn(CaptureEntryView.noticeIdentifier) }
        XCTAssertNotEqual(frame(CaptureEntryView.noticeIdentifier), .zero, "the hosted home shows the notices")
    }
}
