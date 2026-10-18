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
        settle()
    }

    private func settle(for seconds: TimeInterval = 0.3) {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: seconds))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

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

    private func waitUntilFocused(timeout: TimeInterval = 2) -> Bool {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while frames[HomeComposer.focusedIdentifier] == nil, Date() < deadline { settle(for: 0.1) }
        return frames[HomeComposer.focusedIdentifier] != nil
    }

    private func post(_ name: Notification.Name) {
        NotificationCenter.default.post(name: name, object: nil)
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

        post(UIResponder.keyboardWillShowNotification)
        let done = frame(HomeHeader.doneIdentifier)
        XCTAssertNotEqual(done, .zero, "Done is shown while the keyboard is up")
        XCTAssertGreaterThanOrEqual(done.height, Self.minimumTouchTarget)
        XCTAssertNil(frames[HomeHeader.settingsIdentifier], "Settings is replaced by Done")

        post(UIResponder.keyboardWillHideNotification)
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
        settle()
        assertComposerState("typing")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.textModel.updateText("")
        model.startRecording()
        settle()
        XCTAssertEqual(voice.startCount, 1)
        assertComposerState("recording")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.cancelRecording()
        voice.state = .idle
        settle()
        assertComposerState("rest")
        XCTAssertEqual(frame(CaptureEntryView.composerIdentifier).width, rest.width, accuracy: 0.5)
    }

    func testAnInFlightSaveIsReportedAndDisablesRecordAndSaveButNotTheField() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.holdsCompletions = true
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle()

        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the in-flight save is stated")
        assertComposerStateName("typing")
        assertEntryActionsDisabled(saveIsShown: true)
        model.startRecording()
        XCTAssertEqual(voice.startCount, 0, "Record does nothing while a save is in flight")

        textIngress.release()
        settle()
        assertComposerState("rest")
    }

    func testASaveKeepsTheComposerFocused() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        XCTAssertTrue(waitUntilFocused(), "the silent composer takes focus when the home appears")
        textIngress.holdsCompletions = true
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle()
        XCTAssertNotNil(frames[HomeComposer.focusedIdentifier], "the field keeps focus while the save is in flight")

        textIngress.release()
        settle()
        XCTAssertEqual(model.textModel.text, "", "the composer clears")
        XCTAssertNotNil(frames[HomeComposer.focusedIdentifier], "and keeps focus, so the keyboard stays up")
    }

    func testWhitespaceOnlyTextDrawsSaveDisabled() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.textModel.updateText("   ")
        settle()
        assertComposerStateName("typing")
        XCTAssertNotNil(
            frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)], "Save is disabled with nothing to save")
        XCTAssertNil(frames[HomeComposer.saveIdentifier])
        XCTAssertNotNil(frames[HomeComposer.recordIdentifier], "Record still works")
        XCTAssertNotNil(frames[HomeComposer.fieldIdentifier], "the text field stays enabled")

        model.textModel.updateText("  buy oat milk ")
        settle()
        XCTAssertNotNil(frames[HomeComposer.saveIdentifier], "Save is enabled once there is something to save")
        XCTAssertNil(frames[HomeComposer.disabledIdentifier(HomeComposer.saveIdentifier)])
    }

    func testRecordAndSaveAreDisabledWhileTheRecorderStartsOrFinishes() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)

        voice.state = .requestingPermission
        settle()
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "waiting for the microphone is stated")
        assertComposerStateName("rest")
        assertEntryActionsDisabled(saveIsShown: false)

        model.textModel.updateText("buy oat milk")
        settle()
        assertComposerStateName("typing")
        assertEntryActionsDisabled(saveIsShown: true)

        voice.state = .finishing(captureID: "voice-1")
        settle()
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "finishing the recording is stated")
        assertEntryActionsDisabled(saveIsShown: true)

        voice.state = .idle
        settle()
        assertComposerState("typing")
    }

    // MARK: Saved pill and messages

    func testAConfirmedSaveClearsTheTextShowsTheSavedPillAndStillStatesTheReminder() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.outcomes = [.saved(captureID: "text-capture-1", itemID: "item-1", alreadySaved: false)]
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle()

        XCTAssertEqual(model.textModel.text, "", "the composer clears")
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the Saved pill is shown")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the reminder and processing state is visible")
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        settle(for: CaptureEntryView.savedPillDuration + 0.6)
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
        let savedAt = Date()
        settle()
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the Saved pill is shown")

        settle(for: 1.2)
        reader.release()
        settle()
        XCTAssertNotNil(model.acknowledgment?.progress, "the delayed progress read republished the acknowledgment")

        settle(for: max(0, CaptureEntryView.savedPillDuration + 0.6 - Date().timeIntervalSince(savedAt)))
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the progress read did not extend the pill")

        model.refreshProgress()
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
        settle()
        XCTAssertNotNil(frames[CaptureEntryView.savedPillIdentifier], "the first capture is Saved")

        textIngress.holdsCompletions = true
        textIngress.outcomes = [.notStaged(captureID: "text-capture-1")]
        model.textModel.updateText("call the dentist")
        model.saveText()
        settle()
        XCTAssertEqual(model.acknowledgment?.save, .saving)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the pill goes once the next save starts")

        textIngress.release()
        settle()
        XCTAssertEqual(model.acknowledgment?.save, .notSaved)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "Saved is never shown beside Not saved")
        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the failure is stated")
        XCTAssertEqual(model.textModel.text, "call the dentist", "the text is kept")

        settle(for: CaptureEntryView.savedPillDuration)
        XCTAssertNil(frames[CaptureEntryView.savedPillIdentifier], "the first save's timer does not bring it back")
    }

    func testAFailedSaveKeepsTheTextAndIsShownBesideAStandingNotice() {
        model = makeModel(access: CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: true))
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        model.textModel.updateText("buy oat milk")
        model.saveText()
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
        settle()
        XCTAssertNotEqual(frame(CaptureEntryView.noticeIdentifier), .zero, "the hosted home shows the notices")
    }
}
