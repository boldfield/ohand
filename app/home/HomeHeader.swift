import SwiftUI

struct HomeLayoutKey: PreferenceKey {
    static var defaultValue: [String: CGRect] = [:]

    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, new in new }
    }
}

extension View {
    /// Reports where this view lands on screen, so a hosted test can check what is visible and unclipped.
    func reportHomeFrame(_ identifier: String) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(key: HomeLayoutKey.self, value: [identifier: proxy.frame(in: .global)])
        })
    }
}

/// The floating glass header: Search, the wordmark and Settings, which becomes Done while the keyboard is up.
struct HomeHeader: View {
    static let searchIdentifier = "home.search"
    static let settingsIdentifier = "home.settings"
    static let doneIdentifier = "home.done"

    let keyboardIsVisible: Bool
    let actionsAreDisabled: Bool
    var isRegularWidth = false
    var onSearch: () -> Void = {}
    var onSettings: () -> Void = {}
    var onDone: () -> Void = {}

    var body: some View {
        HStack(spacing: DSSpacing.sm) {
            if isRegularWidth {
                Button(action: onSearch) {
                    DSGlassPill {
                        HStack(spacing: DSSpacing.xs) {
                            Image(systemName: "magnifyingglass")
                            Text("Search").font(DSFont.body)
                            Spacer(minLength: 0)
                        }
                        .foregroundColor(DSColor.inkMuted.color)
                    }
                }
                .buttonStyle(.plain)
                .disabled(actionsAreDisabled)
                .accessibilityLabel("Search")
                .accessibilityIdentifier(Self.searchIdentifier)
                wordmark
            } else {
                DSGlassButton(systemName: "magnifyingglass", accessibilityLabel: "Search", action: onSearch)
                    .disabled(actionsAreDisabled)
                    .accessibilityIdentifier(Self.searchIdentifier)
                Spacer(minLength: 0)
                wordmark
                Spacer(minLength: 0)
            }
            if keyboardIsVisible {
                Button(action: onDone) {
                    Text("Done")
                        .font(DSFont.button)
                        .foregroundColor(DSColor.accent.color)
                        .padding(.horizontal, DSSpacing.md)
                        .frame(minHeight: DSLayout.minimumTouchTarget)
                        .dsGlass(in: Capsule())
                        .dsShadow(.floating)
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier(Self.doneIdentifier)
                .reportHomeFrame(Self.doneIdentifier)
            } else {
                DSGlassButton(systemName: "gearshape", accessibilityLabel: "Settings", action: onSettings)
                    .disabled(actionsAreDisabled)
                    .accessibilityIdentifier(Self.settingsIdentifier)
                    .reportHomeFrame(Self.settingsIdentifier)
            }
        }
    }

    private var wordmark: some View {
        DSGlassPill {
            HStack(spacing: DSSpacing.xs) {
                DSMark(size: 22)
                Text("Oh And")
                    .font(DSFont.wordmark)
                    .foregroundColor(DSColor.ink.color)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}
