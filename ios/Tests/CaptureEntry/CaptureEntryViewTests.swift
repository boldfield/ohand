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

    /// Every accessibility element the hosted home exposes, found the way VoiceOver finds them.
    private func accessibilityElements() -> [NSObject] {
        var found: [NSObject] = []
        func walk(_ object: NSObject) {
            let count = object.accessibilityElementCount()
            if count != NSNotFound, count > 0 {
                for index in 0..<count {
                    if let child = object.accessibilityElement(at: index) as? NSObject { found.append(child); walk(child) }
                }
            }
            if let elements = object.accessibilityElements {
                for case let child as NSObject in elements { found.append(child); walk(child) }
            }
            if let view = object as? UIView { view.subviews.forEach(walk) }
        }
        if let root = retainedWindow?.rootViewController?.view { walk(root) }
        return found
    }

    private func element(labelled label: String) -> NSObject? {
        accessibilityElements().first { $0.accessibilityLabel == label }
    }

    private func visibleLabels() -> [String] { accessibilityElements().compactMap { $0.accessibilityLabel } }

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

    func testRestTypingAndRecordingEachShowTheirOwnControls() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        let rest = frame(CaptureEntryView.composerIdentifier)
        XCTAssertNotNil(element(labelled: "What's on your mind?"), "rest shows the placeholder field")
        XCTAssertNotNil(element(labelled: "Record"), "rest shows the mic")
        XCTAssertNil(element(labelled: "Save"), "rest has no Save")
        XCTAssertNil(element(labelled: "Discard"))
        XCTAssertNil(element(labelled: "Stop recording"))

        model.textModel.updateText("buy oat milk")
        settle()
        XCTAssertNotNil(element(labelled: "Record"), "typing keeps the record button")
        XCTAssertNotNil(element(labelled: "Save"), "typing shows Save")
        XCTAssertNil(element(labelled: "Discard"))
        XCTAssertNil(element(labelled: "Stop recording"))
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.textModel.updateText("")
        model.startRecording()
        settle()
        XCTAssertEqual(voice.startCount, 1)
        XCTAssertNotNil(element(labelled: "Discard"), "recording shows Discard")
        XCTAssertNotNil(element(labelled: "Stop recording"), "recording shows Stop")
        XCTAssertNil(element(labelled: "Save"))
        XCTAssertNil(element(labelled: "Record"))
        assertHeaderAndComposerVisible(windowSize: Self.standardPhone, keyboardHeight: Self.standardPhoneKeyboardHeight)

        model.cancelRecording()
        voice.state = .idle
        settle()
        XCTAssertNotNil(element(labelled: "Record"), "back at rest")
        XCTAssertNil(element(labelled: "Discard"))
        XCTAssertEqual(frame(CaptureEntryView.composerIdentifier).width, rest.width, accuracy: 0.5)
    }

    func testAnInFlightSaveIsReportedAndDisablesSave() {
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        textIngress.holdsCompletions = true
        model.textModel.updateText("buy oat milk")
        model.saveText()
        settle()

        XCTAssertTrue(visibleLabels().contains("Saving\u{2026}"), "the in-flight save is stated")
        let save = element(labelled: "Save")
        XCTAssertNotNil(save)
        XCTAssertTrue(save?.accessibilityTraits.contains(.notEnabled) ?? false, "Save is visibly unavailable while saving")
        XCTAssertTrue(element(labelled: "Record")?.accessibilityTraits.contains(.notEnabled) ?? false)
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
        model = makeModel(access: CaptureEntryAccess(protectedDataAvailable: false, readAuthorized: true))
        voice.startResult = .failure(.microphoneDenied)
        host(keyboardHeight: Self.standardPhoneKeyboardHeight)
        model.startRecording()
        settle()

        let labels = visibleLabels().joined(separator: "\n")
        XCTAssertNotNil(model.voiceNotice, "the microphone failure is reported")
        XCTAssertTrue(labels.contains(CaptureEntryModel.startFailureMessage(.microphoneDenied)), "the voice notice is shown")
        XCTAssertTrue(labels.contains(CaptureMessages.protectedDataNotice), "and so is the warning that saving may fail")
    }
}
