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

    private func makeModel(access: CaptureEntryAccess) -> CaptureEntryModel {
        CaptureEntryModel(
            textIngress: textIngress, voice: voice, progressReader: nil, defaultMode: .silent,
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

    private func assertComposerState(
        _ state: String, file: StaticString = #filePath, line: UInt = #line
    ) {
        for other in ["rest", "typing", "recording"] {
            let shown = frames[CaptureEntryView.composerStateIdentifier(other)] != nil
            XCTAssertEqual(shown, other == state, "composer \(other) is \(other == state ? "" : "not ")shown", file: file, line: line)
        }
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

    func testAnInFlightSaveIsReportedAndDisablesTheComposer() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.holdsCompletions = true
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle()

        XCTAssertNotEqual(frame(CaptureEntryView.messageIdentifier), .zero, "the in-flight save is stated")
        XCTAssertNotNil(frames[CaptureEntryView.composerSavingIdentifier], "the composer is disabled while saving")

        textIngress.release()
        settle()
        XCTAssertNil(frames[CaptureEntryView.composerSavingIdentifier], "and enabled again afterwards")
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
