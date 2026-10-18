import SwiftUI

/// The frame every restyled settings screen shares: a glass back button, an info button that opens a sheet with the
/// full explanation, a `displayXL` serif title and the page ground. The screen's own sections go in `content`.
struct SettingsScreenChrome<Content: View>: View {
    static var infoButtonLabel: String { "More information" }
    static var backButtonLabel: String { "Back" }

    let title: String
    let infoTitle: String
    let infoParagraphs: [String]
    let content: Content

    @Environment(\.dismiss) private var dismiss
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @State private var isInfoShown = false

    init(title: String, infoTitle: String, infoParagraphs: [String], @ViewBuilder content: () -> Content) {
        self.title = title
        self.infoTitle = infoTitle
        self.infoParagraphs = infoParagraphs
        self.content = content()
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: DSSpacing.lg) {
                HStack {
                    DSGlassButton(systemName: "chevron.left", accessibilityLabel: Self.backButtonLabel) { dismiss() }
                    Spacer()
                    DSGlassButton(systemName: "info", accessibilityLabel: Self.infoButtonLabel) { isInfoShown = true }
                }
                Text(title)
                    .font(DSFont.displayXL)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                    .fixedSize(horizontal: false, vertical: true)
                content
            }
            .padding(.horizontal, DSSpacing.sideMargin(for: horizontalSizeClass))
            .padding(.vertical, DSSpacing.md)
        }
        .background(DSColor.surface0.color.ignoresSafeArea())
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .sheet(isPresented: $isInfoShown) {
            SettingsInfoSheet(title: infoTitle, paragraphs: infoParagraphs)
        }
    }
}

/// The paragraphs an info sheet shows, so a hosted test can read what the sheet carries.
struct SettingsInfoRenderedKey: PreferenceKey {
    static var defaultValue: [String] = []

    static func reduce(value: inout [String], nextValue: () -> [String]) {
        value.append(contentsOf: nextValue())
    }
}

/// The sheet behind an info button: the full explanation the screen used to show inline.
struct SettingsInfoSheet: View {
    let title: String
    let paragraphs: [String]
    var renderReporter: (([String]) -> Void)?

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: DSSpacing.md) {
                Text(title)
                    .font(DSFont.displayXL)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                ForEach(paragraphs, id: \.self) { paragraph in
                    Text(paragraph)
                        .font(DSFont.body)
                        .foregroundColor(DSColor.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .preference(key: SettingsInfoRenderedKey.self, value: [paragraph])
                }
                DSPillButton(title: "Done", style: .secondary) { dismiss() }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(DSSpacing.lg)
        }
        .background(DSColor.surface0.color.ignoresSafeArea())
        .onPreferenceChange(SettingsInfoRenderedKey.self) { renderReporter?($0) }
        .presentationDetents([.medium, .large])
        .modifier(SheetCornerRadius())
    }
}

/// The sheet radius from the design, on the systems that let a sheet set it.
private struct SheetCornerRadius: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 16.4, *) {
            content.presentationCornerRadius(DSRadius.sheet)
        } else {
            content
        }
    }
}
