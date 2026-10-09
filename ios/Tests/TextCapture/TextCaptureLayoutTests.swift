import SwiftUI
import UIKit
import XCTest
@testable import OhAnd

/// Hosts the real capture view in a small window and checks where its parts land with a keyboard-sized bottom inset and
/// at the largest accessibility text sizes. The inset stands in for the keyboard: SwiftUI treats it as unsafe area,
/// exactly as it does for the real keyboard.
final class TextCaptureLayoutTests: XCTestCase {
    private static let smallPhone = CGSize(width: 320, height: 568)
    private static let standardPhone = CGSize(width: 390, height: 844)
    private static let smallPhoneKeyboardHeight: CGFloat = 260
    private static let standardPhoneKeyboardHeight: CGFloat = 336
    private static let minimumTouchTarget: CGFloat = 44

    private var retainedWindow: UIWindow?

    override func tearDown() {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        super.tearDown()
    }

    private func layout(
        category: ContentSizeCategory,
        keyboardHeight: CGFloat,
        windowSize: CGSize = TextCaptureLayoutTests.smallPhone,
        text: String = "",
        completing outcome: TextCaptureOutcome? = nil
    ) -> TextCaptureLayoutSnapshot {
        let ingress = ScriptedTextCaptureIngress()
        if let outcome { ingress.outcomes = [outcome] }
        let model = TextCaptureModel(ingress: ingress, makeCaptureID: { "layout-capture" })
        model.updateText(text)
        if outcome != nil { model.save() }

        var latest = TextCaptureLayoutSnapshot()
        let root = TextCaptureView(model: model, layoutReporter: { latest = $0 })
            .environment(\.sizeCategory, category)
        let controller = UIHostingController(rootView: root)
        controller.additionalSafeAreaInsets = UIEdgeInsets(top: 0, left: 0, bottom: keyboardHeight, right: 0)
        let window = UIWindow(frame: CGRect(origin: .zero, size: windowSize))
        window.rootViewController = controller
        window.makeKeyAndVisible()
        retainedWindow = window
        controller.view.setNeedsLayout()
        controller.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        controller.view.layoutIfNeeded()
        return latest
    }

    private func assertSaveReachableAndFieldUsable(
        _ snapshot: TextCaptureLayoutSnapshot,
        keyboardHeight: CGFloat,
        windowSize: CGSize = TextCaptureLayoutTests.smallPhone,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        let visibleBottom = windowSize.height - keyboardHeight
        XCTAssertNotEqual(snapshot.saveButton, .zero, "the save button is on screen", file: file, line: line)
        XCTAssertGreaterThanOrEqual(snapshot.saveButton.minX, 0, file: file, line: line)
        XCTAssertLessThanOrEqual(snapshot.saveButton.maxX, windowSize.width, file: file, line: line)
        XCTAssertLessThanOrEqual(snapshot.saveButton.maxY, visibleBottom + 0.5, "save stays above the keyboard", file: file, line: line)
        XCTAssertGreaterThanOrEqual(snapshot.saveButton.height, Self.minimumTouchTarget, file: file, line: line)
        XCTAssertGreaterThanOrEqual(snapshot.saveButton.width, Self.minimumTouchTarget, file: file, line: line)
        XCTAssertGreaterThanOrEqual(snapshot.editor.height, 80, "the field keeps room to type", file: file, line: line)
        XCTAssertLessThanOrEqual(snapshot.editor.maxY, snapshot.saveButton.minY + 0.5, "the field does not run under save", file: file, line: line)
    }

    func testSaveIsReachableWithoutTheKeyboardAtDefaultTextSize() {
        let snapshot = layout(category: .large, keyboardHeight: 0)

        assertSaveReachableAndFieldUsable(snapshot, keyboardHeight: 0)
        XCTAssertGreaterThan(snapshot.editor.height, 300, "an empty capture screen is mostly the field")
    }

    func testSaveStaysAboveTheKeyboardAtDefaultTextSize() {
        let snapshot = layout(category: .large, keyboardHeight: Self.smallPhoneKeyboardHeight)

        assertSaveReachableAndFieldUsable(snapshot, keyboardHeight: Self.smallPhoneKeyboardHeight)
    }

    func testSaveAndFieldRemainUsableAtLargestAccessibilityTextWithKeyboard() {
        for category in [ContentSizeCategory.accessibilityLarge, .accessibilityExtraExtraLarge, .accessibilityExtraExtraExtraLarge] {
            let snapshot = layout(category: category, keyboardHeight: Self.smallPhoneKeyboardHeight, text: "buy oat milk")

            assertSaveReachableAndFieldUsable(snapshot, keyboardHeight: Self.smallPhoneKeyboardHeight)
        }
    }

    func testLongStatusMessageAtLargestTextDoesNotPushSaveOrTheFieldOut() {
        let snapshot = layout(
            category: .accessibilityExtraExtraExtraLarge,
            keyboardHeight: Self.standardPhoneKeyboardHeight,
            windowSize: Self.standardPhone,
            text: "buy oat milk",
            completing: .keptOnDevice(captureID: "layout-capture"))

        XCTAssertNotEqual(snapshot.status, .zero, "the honest result is shown")
        XCTAssertLessThanOrEqual(snapshot.status.height, 140.5, "a long message scrolls instead of taking the screen")
        assertSaveReachableAndFieldUsable(
            snapshot, keyboardHeight: Self.standardPhoneKeyboardHeight, windowSize: Self.standardPhone)
    }

    func testStatusMessageAtDefaultTextSizeFitsASmallPhoneWithKeyboard() {
        let snapshot = layout(
            category: .large,
            keyboardHeight: Self.smallPhoneKeyboardHeight,
            text: "buy oat milk",
            completing: .keptOnDevice(captureID: "layout-capture"))

        XCTAssertNotEqual(snapshot.status, .zero)
        assertSaveReachableAndFieldUsable(snapshot, keyboardHeight: Self.smallPhoneKeyboardHeight)
    }

    func testNoStatusRegionIsShownBeforeAnythingHasHappened() {
        let snapshot = layout(category: .large, keyboardHeight: 0)

        XCTAssertEqual(snapshot.status, .zero, "no banner, count or coaching before a capture")
    }
}
