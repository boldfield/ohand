import XCTest
import UIKit
@testable import OhAnd

final class DesignSystemTokenTests: XCTestCase {
    private struct TextPair {
        let foreground: DSColor
        let ground: DSColor
        let isLargeText: Bool

        init(_ foreground: DSColor, on ground: DSColor, isLargeText: Bool = false) {
            self.foreground = foreground
            self.ground = ground
            self.isLargeText = isLargeText
        }
    }

    /// Every text pair the design document uses.
    private let textPairs: [TextPair] = [
        TextPair(.ink, on: .surface0), TextPair(.ink, on: .surface1), TextPair(.ink, on: .surface2),
        TextPair(.ink, on: .tint),
        TextPair(.inkMuted, on: .surface0), TextPair(.inkMuted, on: .surface1), TextPair(.inkMuted, on: .surface2),
        TextPair(.accent, on: .surface0), TextPair(.accent, on: .surface1), TextPair(.accent, on: .surface2),
        TextPair(.accent, on: .tint),
        TextPair(.destructive, on: .surface0), TextPair(.destructive, on: .surface1),
        TextPair(.onAccent, on: .accent), TextPair(.onAccent, on: .accentPressed),
        TextPair(.onBrand, on: .brand),
    ]

    func testTextPairsMeetWCAGContrastInBothThemes() {
        for pair in textPairs {
            let required = pair.isLargeText ? 3.0 : 4.5
            let lightRatio = pair.foreground.light.contrastRatio(against: pair.ground.light)
            let darkRatio = pair.foreground.dark.contrastRatio(against: pair.ground.dark)
            XCTAssertGreaterThanOrEqual(
                lightRatio, required, "\(pair.foreground) on \(pair.ground), light: \(lightRatio)")
            XCTAssertGreaterThanOrEqual(
                darkRatio, required, "\(pair.foreground) on \(pair.ground), dark: \(darkRatio)")
        }
    }

    func testContrastRatioMatchesKnownValues() {
        let black = DSRGBA(hex: 0x000000)
        let white = DSRGBA(hex: 0xFFFFFF)
        XCTAssertEqual(black.contrastRatio(against: white), 21, accuracy: 0.001)
        XCTAssertEqual(white.contrastRatio(against: white), 1, accuracy: 0.001)
    }

    func testTokenValuesMatchTheDesignTable() {
        XCTAssertEqual(DSColor.accent.light, DSRGBA(hex: 0x3D4ED6))
        XCTAssertEqual(DSColor.accent.dark, DSRGBA(hex: 0x8E9BFF))
        XCTAssertEqual(DSColor.onAccent.dark, DSRGBA(hex: 0x0B0C14))
        XCTAssertEqual(DSColor.surface0.dark, DSRGBA(hex: 0x0A0B12))
        XCTAssertEqual(DSColor.glass.light, DSRGBA(hex: 0xFFFFFF, alpha: 0.64))
        XCTAssertEqual(DSColor.glass.dark, DSRGBA(hex: 0x1C1F32, alpha: 0.58))
        XCTAssertEqual(DSColor.glassStroke.dark, DSRGBA(hex: 0xFFFFFF, alpha: 0.12))
        XCTAssertEqual(DSColor.destructive.light, DSRGBA(hex: 0xC9373C))
        XCTAssertEqual(DSColor.signalRecord.dark, DSRGBA(hex: 0xFF6369))
    }

    func testDynamicColoursResolveForBothAppearances() {
        for token in DSColor.allCases {
            let light = token.uiColor.resolvedColor(with: UITraitCollection(userInterfaceStyle: .light))
            let dark = token.uiColor.resolvedColor(with: UITraitCollection(userInterfaceStyle: .dark))
            XCTAssertEqual(light, token.light.uiColor, "\(token) light")
            XCTAssertEqual(dark, token.dark.uiColor, "\(token) dark")
        }
    }

    func testSpacingRadiusAndTouchTargetConstants() {
        XCTAssertEqual([DSSpacing.xxs, DSSpacing.xs, DSSpacing.sm, DSSpacing.md, DSSpacing.lg, DSSpacing.xl, DSSpacing.xxl],
                       [4, 8, 12, 16, 24, 32, 48])
        XCTAssertEqual(DSSpacing.sideMargin(for: .compact), 16)
        XCTAssertEqual(DSSpacing.sideMargin(for: .regular), 24)
        XCTAssertEqual([DSRadius.chip, DSRadius.field, DSRadius.card, DSRadius.composer], [10, 16, 24, 32])
        XCTAssertGreaterThanOrEqual(DSLayout.minimumTouchTarget, 44)
        XCTAssertGreaterThanOrEqual(DSLayout.secondaryButtonHeight, 44)
        XCTAssertGreaterThanOrEqual(DSLayout.rowMinimumHeight, 52)
        XCTAssertEqual(DSShadow.floating.y, 8)
        XCTAssertEqual(DSShadow.card.y, 1)
    }
}

final class DesignSystemFontTests: XCTestCase {
    func testBundledSerifFontsLoad() {
        for name in [DSFont.serifRegularName, DSFont.serifItalicName] {
            XCTAssertNotNil(UIFont(name: name, size: 20), "\(name) is not registered; check UIAppFonts and bundling")
        }
    }

    func testFontFilesAndLicenceAreInTheAppBundle() {
        for fileName in DSFontRegistry.bundledFontFileNames + ["OFL.txt"] {
            let url = Bundle.main.url(forResource: (fileName as NSString).deletingPathExtension,
                                      withExtension: (fileName as NSString).pathExtension)
            XCTAssertNotNil(url, "\(fileName) is not bundled")
        }
    }

    func testInfoPlistRegistersTheFonts() {
        let registered = Bundle.main.object(forInfoDictionaryKey: "UIAppFonts") as? [String]
        XCTAssertEqual(registered?.sorted(), DSFontRegistry.bundledFontFileNames.sorted())
    }
}

final class DesignSystemRowTests: XCTestCase {
    /// A row used as a `NavigationLink` label has no action and must not be a button, or the inner button takes the
    /// link's tap and the link never opens. The walkthrough UI test taps each Settings row to cover the link itself.
    func testARowIsAButtonOnlyWhenItHasAnAction() {
        XCTAssertFalse(DSRow(label: "Daily prompt", iconSystemName: "bell", value: "On").rendersAsButton)
        var tapped = false
        let row = DSRow(label: "Daily prompt") { tapped = true }
        XCTAssertTrue(row.rendersAsButton)
        row.action?()
        XCTAssertTrue(tapped)
    }
}
