import CoreGraphics
import XCTest

/// The rule that decides contrast issues under and on glass, without the app: synthetic frames and pixels only.
final class GlassContrastLedgerTests: XCTestCase {
    private let screen = CGRect(x: 0, y: 0, width: 402, height: 874)
    private let clearBand: ClosedRange<CGFloat> = 126...675

    private var nextNodeID = 0

    private func element(
        _ label: String, card: String = "", identifier: String = "", _ frame: CGRect, ancestors: [ScreenElement] = []
    ) -> ScreenElement {
        nextNodeID += 1
        return ScreenElement(
            nodeID: nextNodeID, ancestorIDs: Set(ancestors.map(\.nodeID)),
            key: ScreenElement.key(card: card, identifier: identifier, label: label), identifier: identifier, label: label, frame: frame)
    }

    // MARK: Scrolled under glass

    /// A low-contrast button under the back button is excused there, but the test owes a clear audit of it; when that clear
    /// audit flags it too, the issue is real.
    func testALowContrastElementUnderTheBackButtonFailsOnceAuditedClear() {
        var ledger = GlassContrastLedger()
        let underBack = element("Save words", identifier: "item-controls.text.save", CGRect(x: 16, y: 54, width: 139, height: 44))
        XCTAssertEqual(
            ledger.decide(contrastIssueOn: underBack, placement: .scrollsUnderGlass, screenName: "detail-controls", clearBand: clearBand,
                          screen: screen, others: [], measure: { XCTFail("no measurement under glass"); return nil }),
            .owed)
        XCTAssertEqual(ledger.unsettled.map(\.key), [underBack.key], "an excuse nothing settles must fail the test")

        let clear = element("Save words", identifier: "item-controls.text.save", CGRect(x: 16, y: 300, width: 139, height: 44))
        let decision = ledger.decide(contrastIssueOn: clear, placement: .scrollsUnderGlass, screenName: "detail-clear", clearBand: clearBand,
                                     screen: screen, others: [], measure: { 21 })
        guard case .issue = decision else { return XCTFail("a flagged element clear of the glass must fail, got \(decision)") }
        let settled = ledger.recordStop("detail-clear", listed: [(clear, .scrollsUnderGlass)], flaggedKeys: [clear.key],
                                        clearBand: clearBand, screen: screen, others: [])
        XCTAssertTrue(settled.isEmpty, "a flagged stop settles nothing")
        XCTAssertEqual(ledger.unsettled.count, 1)
    }

    /// A dark glyph from another element in the frame (the back chevron) never settles a scrolled element: only an audit
    /// with the element wholly in the clear band does.
    func testAnElementIsSettledOnlyByAnAuditWithItWhollyClear() {
        var ledger = GlassContrastLedger()
        let partly = element("Type", CGRect(x: 16, y: 114, width: 31, height: 16))
        _ = ledger.decide(contrastIssueOn: partly, placement: .scrollsUnderGlass, screenName: "a", clearBand: clearBand, screen: screen,
                          others: [], measure: { 19.45 })
        ledger.recordStop("a", listed: [(partly, .scrollsUnderGlass)], flaggedKeys: [], clearBand: clearBand, screen: screen, others: [])
        XCTAssertEqual(ledger.unsettled.count, 1, "partly under the glass is not clear, flagged or not")

        let clear = element("Type", CGRect(x: 16, y: 200, width: 31, height: 16))
        ledger.recordStop("b", listed: [(clear, .scrollsUnderGlass)], flaggedKeys: [], clearBand: clearBand, screen: screen, others: [])
        XCTAssertTrue(ledger.unsettled.isEmpty)
        XCTAssertEqual(ledger.settled[clear.key], "b")
    }

    /// The same caption label in two cards is two elements: settling one does not settle the other.
    func testCaptionsAreKeyedByTheirCardAndNotByTheirAge() {
        XCTAssertEqual(ScreenElement.stableLabel("Idea \u{00B7} 13 sec. ago"), "Idea")
        var ledger = GlassContrastLedger()
        let typed = element("Idea \u{00B7} 13 sec. ago", card: "home.recents.card.1", CGRect(x: 48, y: 727, width: 109, height: 16))
        _ = ledger.decide(contrastIssueOn: typed, placement: .scrollsUnderGlass, screenName: "home", clearBand: clearBand, screen: screen,
                          others: [], measure: { nil })
        let spoken = element("Idea \u{00B7} 14 sec. ago", card: "home.recents.card.2", CGRect(x: 48, y: 300, width: 109, height: 16))
        ledger.recordStop("home-end", listed: [(spoken, .scrollsUnderGlass)], flaggedKeys: [], clearBand: clearBand, screen: screen, others: [])
        XCTAssertEqual(ledger.unsettled.map(\.key), [typed.key])
        let typedLater = element("Idea \u{00B7} 15 sec. ago", card: "home.recents.card.1", CGRect(x: 48, y: 400, width: 109, height: 16))
        ledger.recordStop("home-clear", listed: [(typedLater, .scrollsUnderGlass)], flaggedKeys: [], clearBand: clearBand, screen: screen, others: [])
        XCTAssertTrue(ledger.unsettled.isEmpty)
    }

    func testAnElementOffScreenIsNotClear() {
        let below = element("Synthetic spoken note", CGRect(x: 32, y: 700, width: 285, height: 249))
        XCTAssertFalse(GlassContrastLedger.isUnobstructed(below, placement: .scrollsUnderGlass, clearBand: 126...10_000, screen: screen, others: []))
    }

    // MARK: Drawn on glass

    /// Text drawn on glass with something else overlapping it (a card behind the composer) is not measured; with nothing
    /// overlapping, a low measured contrast fails.
    func testTextDrawnOnGlassIsMeasuredOnlyWithNothingOverlappingAndFailsBelowTheMinimum() {
        var ledger = GlassContrastLedger()
        let placeholder = element("What's on your mind?", CGRect(x: 36, y: 739, width: 221, height: 36))
        let field = element("What's on your mind?", identifier: "home.composer.field", CGRect(x: 36, y: 735, width: 230, height: 44))
        let cardBehind = element("Idea \u{00B7} 13 sec. ago", card: "home.recents.card.1", CGRect(x: 48, y: 727, width: 109, height: 16))
        var measured = false
        XCTAssertEqual(
            ledger.decide(contrastIssueOn: placeholder, placement: .drawnOnGlass, screenName: "home", clearBand: clearBand, screen: screen,
                          others: [field, cardBehind], measure: { measured = true; return 21 }),
            .owed)
        XCTAssertFalse(measured, "a crop another element overlaps is never measured")

        let low = ledger.decide(contrastIssueOn: placeholder, placement: .drawnOnGlass, screenName: "home-empty", clearBand: clearBand,
                                screen: screen, others: [field], measure: { 3.1 })
        guard case .issue = low else { return XCTFail("a measured 3.1:1 must fail, got \(low)") }
        XCTAssertEqual(ledger.unsettled.count, 1)

        XCTAssertEqual(
            ledger.decide(contrastIssueOn: placeholder, placement: .drawnOnGlass, screenName: "home-empty", clearBand: clearBand,
                          screen: screen, others: [field], measure: { 5.2 }),
            .measured(5.2))
        XCTAssertTrue(ledger.unsettled.isEmpty)
    }

    func testAMeasurementThatFailsIsAnIssue() {
        var ledger = GlassContrastLedger()
        let mark = element("&", CGRect(x: 134, y: 67, width: 20, height: 50))
        let decision = ledger.decide(contrastIssueOn: mark, placement: .drawnOnGlass, screenName: "x", clearBand: clearBand, screen: screen,
                                     others: [], measure: { nil })
        guard case .issue = decision else { return XCTFail("an unmeasurable element must fail, got \(decision)") }
    }

    func testAnElementTheSnapshotDidNotShowIsAnIssue() {
        var ledger = GlassContrastLedger()
        let decision = ledger.decide(contrastIssueOn: nil, placement: .scrollsUnderGlass, screenName: "x", clearBand: clearBand, screen: screen,
                                     others: [], measure: { 21 })
        guard case .issue = decision else { return XCTFail("got \(decision)") }
    }

    // MARK: Measurement

    /// Grey text (#9A9A9A) on white is about 2.81:1; a black chevron in the crop would read 21:1, so it is excluded.
    func testTheMeasurementReadsTheTextAndNotAnExcludedGlyph() {
        let width = 40, height = 20
        var pixels = [UInt8](repeating: 255, count: width * height * 4)
        func paint(_ rect: CGRect, _ value: UInt8) {
            for row in Int(rect.minY)..<Int(rect.maxY) {
                for column in Int(rect.minX)..<Int(rect.maxX) {
                    let offset = (row * width + column) * 4
                    pixels[offset] = value; pixels[offset + 1] = value; pixels[offset + 2] = value
                }
            }
        }
        paint(CGRect(x: 20, y: 5, width: 15, height: 10), 0x9A)
        paint(CGRect(x: 2, y: 2, width: 6, height: 16), 0x00)
        let chevron = CGRect(x: 0, y: 0, width: 10, height: 20)
        let withChevron = ScreenshotContrast.ratio(pixels: pixels, width: width, height: height) ?? 0
        XCTAssertEqual(withChevron, 21, accuracy: 0.01, "the furthest pixel is the chevron when it is not excluded")
        let textOnly = ScreenshotContrast.ratio(pixels: pixels, width: width, height: height, excluded: [chevron]) ?? 0
        XCTAssertEqual(textOnly, 2.81, accuracy: 0.05)
        XCTAssertLessThan(textOnly, GlassContrastLedger.minimumContrastRatio)
    }

    /// An element whose frame another element overlaps, the way the back button overlaps a pill scrolled under it; its
    /// ancestors and the same control do not count, and a card behind it counts even when its frame contains it.
    func testOverlapIgnoresAncestorsAndTheSameControlButNotACardBehind() {
        let container = element("Controls", CGRect(x: 0, y: 40, width: 402, height: 100))
        let pill = element("Save words", CGRect(x: 16, y: 54, width: 139, height: 44), ancestors: [container])
        let back = element("Back", CGRect(x: 8, y: 50, width: 44, height: 44))
        let sameControl = element("Save words", identifier: "item-controls.text.save", CGRect(x: 16, y: 54, width: 139, height: 44))
        let cardBehind = element("Synthetic spoken note", card: "home.recents.card.2", CGRect(x: 16, y: 0, width: 370, height: 300))
        XCTAssertEqual(GlassContrastLedger.overlapping(pill, in: [back, container, sameControl, cardBehind]), [back, cardBehind])
    }
}
