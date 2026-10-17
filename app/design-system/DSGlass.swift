import SwiftUI

/// Glass for floating controls: the system Liquid Glass material where it exists, a translucent fill with a 1 pt edge
/// elsewhere. Glass is never put on glass.
private struct DSGlassBackground<GlassShape: Shape>: ViewModifier {
    let shape: GlassShape

    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.glassEffect(.regular, in: shape)
        } else {
            content
                .background(shape.fill(.ultraThinMaterial))
                .background(shape.fill(DSColor.glass.color))
                .overlay(shape.stroke(DSColor.glassStroke.color, lineWidth: 1))
        }
    }
}

extension View {
    func dsGlass<GlassShape: Shape>(in shape: GlassShape) -> some View {
        modifier(DSGlassBackground(shape: shape))
    }
}

/// Shows a component in light and dark side by side, for SwiftUI previews.
struct DSPreviewPair<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        HStack(spacing: 0) {
            ForEach([ColorScheme.light, ColorScheme.dark], id: \.self) { scheme in
                content
                    .padding(DSSpacing.md)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(DSColor.surface0.color)
                    .environment(\.colorScheme, scheme)
            }
        }
    }
}
