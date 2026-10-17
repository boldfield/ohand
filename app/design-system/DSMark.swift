import SwiftUI
import UIKit

/// The italic Instrument Serif ampersand. It is `brand` in light and `accent` in dark unless a colour is given
/// (white on a brand ground).
struct DSMark: View {
    let size: CGFloat
    var color: Color?

    var body: some View {
        Text("&")
            .font(DSFont.mark(size: size))
            .foregroundColor(color ?? DSMark.defaultColor)
            .accessibilityHidden(true)
    }

    private static var defaultColor: Color {
        Color(uiColor: UIColor { traits in
            traits.userInterfaceStyle == .dark ? DSColor.accent.dark.uiColor : DSColor.brand.light.uiColor
        })
    }
}

#if DEBUG
#Preview("Mark") {
    DSPreviewPair {
        VStack {
            DSMark(size: 96)
            DSMark(size: 44)
        }
    }
}
#endif
