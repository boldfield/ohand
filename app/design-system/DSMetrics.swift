import SwiftUI
import UIKit

/// The 4-point spacing grid and the side margins.
enum DSSpacing {
    static let xxs: CGFloat = 4
    static let xs: CGFloat = 8
    static let sm: CGFloat = 12
    static let md: CGFloat = 16
    static let lg: CGFloat = 24
    static let xl: CGFloat = 32
    static let xxl: CGFloat = 48

    static let compactSideMargin: CGFloat = 16
    static let regularSideMargin: CGFloat = 24

    static func sideMargin(for horizontalSizeClass: UserInterfaceSizeClass?) -> CGFloat {
        horizontalSizeClass == .regular ? regularSideMargin : compactSideMargin
    }
}

enum DSRadius {
    static let chip: CGFloat = 10
    static let iconTile: CGFloat = 10
    static let field: CGFloat = 16
    static let card: CGFloat = 24
    static let composer: CGFloat = 32
    static let sheet: CGFloat = 32
}

/// A drop shadow. `radius` is the SwiftUI blur radius, which is half the design's CSS blur value.
struct DSShadow {
    let color: DSColor
    let x: CGFloat
    let y: CGFloat
    let radius: CGFloat

    /// Floating controls: `0 8 32`.
    static let floating = DSShadow(color: .shadowFloating, x: 0, y: 8, radius: 16)
    /// Cards: `0 1 2`. Fully transparent in dark, where surface steps do the work.
    static let card = DSShadow(color: .shadowCard, x: 0, y: 1, radius: 1)
}

enum DSLayout {
    /// The minimum touch target in both dimensions.
    static let minimumTouchTarget: CGFloat = 44
    static let primaryButtonHeight: CGFloat = 50
    static let secondaryButtonHeight: CGFloat = 44
    static let chipHeight: CGFloat = 34
    static let rowMinimumHeight: CGFloat = 52
    static let iconTileSide: CGFloat = 32
    static let composerMicSide: CGFloat = 48
    static let stopButtonSide: CGFloat = 72
}

extension View {
    func dsShadow(_ shadow: DSShadow) -> some View {
        self.shadow(color: shadow.color.color, radius: shadow.radius, x: shadow.x, y: shadow.y)
    }
}

/// A type style from the design. Sans styles map to system text styles so they follow Dynamic Type; the serif
/// styles scale through `Font.custom(_:size:relativeTo:)`.
enum DSFont {
    static let serifRegularName = "InstrumentSerif-Regular"
    static let serifItalicName = "InstrumentSerif-Italic"

    /// First-level screen titles. 44 / 46.
    static var displayXL: Font { .custom(serifRegularName, size: 44, relativeTo: .largeTitle) }
    /// Text in the composer. 28 / 34.
    static var capture: Font { .custom(serifRegularName, size: 28, relativeTo: .title) }
    /// The italic composer placeholder.
    static var capturePlaceholder: Font { .custom(serifItalicName, size: 28, relativeTo: .title) }
    /// The recording timer. 76 / 76, tabular.
    static var numeral: Font { Font.custom(serifRegularName, size: 76, relativeTo: .largeTitle).monospacedDigit() }
    /// "Oh And" in the header.
    static var wordmark: Font { .custom(serifRegularName, size: 22, relativeTo: .title3) }
    /// Item text on the regular-width detail card.
    static var detailSerif: Font { .custom(serifRegularName, size: 30, relativeTo: .title) }
    /// The italic ampersand at a given point size.
    static func mark(size: CGFloat) -> Font { .custom(serifItalicName, size: size, relativeTo: .largeTitle) }

    static let body: Font = .body
    static let rowLabel: Font = .body
    static let sectionTitle: Font = .system(.subheadline, design: .default).weight(.semibold)
    static let footer: Font = .system(.footnote, design: .default)
    static let groupHeading: Font = .system(.footnote, design: .default).weight(.semibold)
    static let button: Font = .system(.body, design: .default).weight(.semibold)
    static let chip: Font = .system(.subheadline, design: .default).weight(.medium)
}

enum DSFontRegistry {
    static let bundledFontFileNames = ["InstrumentSerif-Regular.ttf", "InstrumentSerif-Italic.ttf"]
}
