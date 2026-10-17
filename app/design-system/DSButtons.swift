import SwiftUI

enum DSPillStyle {
    case primary
    case secondary
    /// Text only, in `accent`.
    case plain
    /// Text only, in `destructive`.
    case destructive
}

struct DSPillButtonStyle: ButtonStyle {
    let style: DSPillStyle
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(DSFont.button)
            .foregroundColor(foreground)
            .padding(.horizontal, DSSpacing.lg)
            .frame(minHeight: height)
            .frame(minHeight: DSLayout.minimumTouchTarget)
            .background(Capsule().fill(fill(isPressed: configuration.isPressed)))
            .contentShape(Capsule())
            .opacity(configuration.isPressed && !hasFill ? 0.6 : 1)
    }

    private var hasFill: Bool {
        style == .primary || style == .secondary
    }

    private var height: CGFloat {
        style == .primary ? DSLayout.primaryButtonHeight : DSLayout.secondaryButtonHeight
    }

    private var foreground: Color {
        switch style {
        case .primary: return (isEnabled ? DSColor.onAccent : DSColor.inkFaint).color
        case .secondary: return DSColor.ink.color
        case .plain: return (isEnabled ? DSColor.accent : DSColor.inkFaint).color
        case .destructive: return (isEnabled ? DSColor.destructive : DSColor.inkFaint).color
        }
    }

    private func fill(isPressed: Bool) -> Color {
        switch style {
        case .primary:
            guard isEnabled else { return DSColor.surface2.color }
            return (isPressed ? DSColor.accentPressed : DSColor.accent).color
        case .secondary:
            return DSColor.tint.color
        case .plain, .destructive:
            return .clear
        }
    }
}

/// A pill-shaped text button. Primary is 50 pt tall, the others 44 pt; every style has at least a 44 pt target.
struct DSPillButton: View {
    let title: String
    var style: DSPillStyle = .primary
    var action: () -> Void = {}

    var body: some View {
        Button(action: action) {
            Text(title)
        }
        .buttonStyle(DSPillButtonStyle(style: style))
    }
}

/// A 44 pt circle on glass holding an SF Symbol: search, settings, back, info.
struct DSGlassButton: View {
    let systemName: String
    let accessibilityLabel: String
    var action: () -> Void = {}

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 17, weight: .semibold))
                .foregroundColor(DSColor.ink.color)
                .frame(width: DSLayout.minimumTouchTarget, height: DSLayout.minimumTouchTarget)
                .dsGlass(in: Circle())
                .dsShadow(.floating)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(accessibilityLabel)
    }
}

/// A glass capsule holding the header wordmark or the search field.
struct DSGlassPill<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .padding(.horizontal, DSSpacing.md)
            .frame(minHeight: DSLayout.minimumTouchTarget)
            .dsGlass(in: Capsule())
            .dsShadow(.floating)
    }
}

#if DEBUG
#Preview("Buttons") {
    DSPreviewPair {
        VStack(spacing: DSSpacing.sm) {
            DSPillButton(title: "Save", style: .primary)
            DSPillButton(title: "Save", style: .primary).disabled(true)
            DSPillButton(title: "Add provider", style: .secondary)
            DSPillButton(title: "Advanced", style: .plain)
            DSPillButton(title: "Discard", style: .destructive)
            HStack {
                DSGlassButton(systemName: "magnifyingglass", accessibilityLabel: "Search")
                DSGlassPill {
                    HStack {
                        DSMark(size: 22)
                        Text("Oh And").font(DSFont.wordmark).foregroundColor(DSColor.ink.color)
                    }
                }
            }
        }
    }
}
#endif
