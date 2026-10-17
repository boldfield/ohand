import SwiftUI
import UIKit

/// An sRGB colour with alpha, kept as plain numbers so tests can compute contrast without rendering.
struct DSRGBA: Equatable {
    let red: Double
    let green: Double
    let blue: Double
    let alpha: Double

    init(hex: UInt32, alpha: Double = 1) {
        red = Double((hex >> 16) & 0xFF) / 255
        green = Double((hex >> 8) & 0xFF) / 255
        blue = Double(hex & 0xFF) / 255
        self.alpha = alpha
    }

    var uiColor: UIColor {
        UIColor(red: red, green: green, blue: blue, alpha: alpha)
    }

    /// WCAG 2.x relative luminance of the opaque colour.
    var relativeLuminance: Double {
        func linearized(_ channel: Double) -> Double {
            channel <= 0.03928 ? channel / 12.92 : pow((channel + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linearized(red) + 0.7152 * linearized(green) + 0.0722 * linearized(blue)
    }

    /// WCAG contrast ratio between two opaque colours, from 1 to 21.
    func contrastRatio(against other: DSRGBA) -> Double {
        let lighter = max(relativeLuminance, other.relativeLuminance)
        let darker = min(relativeLuminance, other.relativeLuminance)
        return (lighter + 0.05) / (darker + 0.05)
    }
}

/// Every colour of the M1 UI design, defined once with its light and dark value. Call sites use `token.color`
/// and never a hex value.
enum DSColor: String, CaseIterable {
    case brand, onBrand
    case accent, accentPressed, onAccent
    case tint
    case surface0, surface1, surface2
    case glass, glassStroke
    case ink, inkMuted, inkFaint
    case hairline
    case signalRecord, destructive, signalSuccess, signalWarning
    case shadowFloating, shadowCard

    var light: DSRGBA {
        switch self {
        case .brand: return DSRGBA(hex: 0x1E2761)
        case .onBrand: return DSRGBA(hex: 0xFFFFFF)
        case .accent: return DSRGBA(hex: 0x3D4ED6)
        case .accentPressed: return DSRGBA(hex: 0x2F3DB0)
        case .onAccent: return DSRGBA(hex: 0xFFFFFF)
        case .tint: return DSRGBA(hex: 0xE7E9FF)
        case .surface0: return DSRGBA(hex: 0xF5F6FA)
        case .surface1: return DSRGBA(hex: 0xFFFFFF)
        case .surface2: return DSRGBA(hex: 0xECEEF5)
        case .glass: return DSRGBA(hex: 0xFFFFFF, alpha: 0.64)
        case .glassStroke: return DSRGBA(hex: 0xFFFFFF, alpha: 0.90)
        case .ink: return DSRGBA(hex: 0x11132A)
        case .inkMuted: return DSRGBA(hex: 0x565B78)
        case .inkFaint: return DSRGBA(hex: 0x8A8EA8)
        case .hairline: return DSRGBA(hex: 0xE1E3EE)
        case .signalRecord: return DSRGBA(hex: 0xE5484D)
        case .destructive: return DSRGBA(hex: 0xC9373C)
        case .signalSuccess: return DSRGBA(hex: 0x1F8A5B)
        case .signalWarning: return DSRGBA(hex: 0xA86400)
        case .shadowFloating: return DSRGBA(hex: 0x11132A, alpha: 0.14)
        case .shadowCard: return DSRGBA(hex: 0x11132A, alpha: 0.06)
        }
    }

    var dark: DSRGBA {
        switch self {
        case .brand: return DSRGBA(hex: 0x2E3A94)
        case .onBrand: return DSRGBA(hex: 0xFFFFFF)
        case .accent: return DSRGBA(hex: 0x8E9BFF)
        case .accentPressed: return DSRGBA(hex: 0xA9B3FF)
        case .onAccent: return DSRGBA(hex: 0x0B0C14)
        case .tint: return DSRGBA(hex: 0x23264A)
        case .surface0: return DSRGBA(hex: 0x0A0B12)
        case .surface1: return DSRGBA(hex: 0x151726)
        case .surface2: return DSRGBA(hex: 0x1D2033)
        case .glass: return DSRGBA(hex: 0x1C1F32, alpha: 0.58)
        case .glassStroke: return DSRGBA(hex: 0xFFFFFF, alpha: 0.12)
        case .ink: return DSRGBA(hex: 0xF1F2FA)
        case .inkMuted: return DSRGBA(hex: 0xA2A6C4)
        case .inkFaint: return DSRGBA(hex: 0x6E7290)
        case .hairline: return DSRGBA(hex: 0x2A2D44)
        case .signalRecord: return DSRGBA(hex: 0xFF6369)
        case .destructive: return DSRGBA(hex: 0xFF6369)
        case .signalSuccess: return DSRGBA(hex: 0x4CC38A)
        case .signalWarning: return DSRGBA(hex: 0xF0B34A)
        case .shadowFloating: return DSRGBA(hex: 0x000000, alpha: 0.50)
        case .shadowCard: return DSRGBA(hex: 0x000000, alpha: 0.0)
        }
    }

    /// A dynamic colour that follows the system appearance.
    var uiColor: UIColor {
        let lightValue = light
        let darkValue = dark
        return UIColor { traits in
            traits.userInterfaceStyle == .dark ? darkValue.uiColor : lightValue.uiColor
        }
    }

    var color: Color {
        Color(uiColor: uiColor)
    }
}
