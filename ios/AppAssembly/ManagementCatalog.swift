import SwiftUI

/// What the host hands each management screen when it builds it: the models that screens present.
struct ManagementContext {
    let voiceRecovery: VoiceRecoveryModel
    let retrieval: RetrievalSearchModel
    let retention: RetentionSettingsModel
    let providerSettings: ProviderSettingsModel
    let promptSettings: PromptSettingsModel
    let sourceDetail: (String) -> SourceDetailModel
    let suggestion: SuggestionScreenModel
    /// Lets a hosted test see what the search screen shows; production leaves it unset.
    var retrievalRenderReporter: (([String: RetrievalRenderedElement]) -> Void)?
}

/// Frames of the placeholder screens, keyed by accessibility identifier, so a hosted test can see that they render.
struct ManagementPlaceholderFrameKey: PreferenceKey {
    static var defaultValue: [String: CGRect] = [:]

    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

private extension View {
    func reportPlaceholderFrame(_ identifier: String) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(key: ManagementPlaceholderFrameKey.self, value: [identifier: proxy.frame(in: .global)])
        })
    }
}

/// A destination host event, reported to an optional observer so tests (and only tests) can see what is on display.
enum ManagementPresentationEvent: Equatable {
    case appeared(ManagementRoute, isPlaceholder: Bool)
    case disappeared(ManagementRoute)
}

/// The screens other slices fill in for requests that come from outside the management lists: an item's source detail
/// and the current suggestion. A missing screen shows an honest placeholder, never a blank screen.
struct ManagementDestinations {
    var item: ((ManagementContext, String) -> AnyView)?
    var suggestion: ((ManagementContext) -> AnyView)?

    func isRegistered(_ route: ManagementRoute) -> Bool {
        switch route {
        case .entry(let entryID): return ManagementCatalog.entry(withID: entryID) != nil
        case .item: return item != nil
        case .suggestion: return suggestion != nil
        }
    }

    func view(for route: ManagementRoute, context: ManagementContext) -> AnyView {
        switch route {
        case .entry(let entryID):
            if let entry = ManagementCatalog.entry(withID: entryID) {
                return entry.makeView(context)
            }
            return AnyView(Text("This screen is not available.").padding())
        case .item(let itemID):
            if let makeItemView = item { return makeItemView(context, itemID) }
            return AnyView(ItemPlaceholderView())
        case .suggestion:
            if let makeSuggestionView = suggestion { return makeSuggestionView(context) }
            return AnyView(SuggestionPlaceholderView())
        }
    }
}

/// One management screen reachable from capture, or one section of settings. `makeView` is a single reference to the
/// owning slice's view.
struct ManagementEntry: Identifiable {
    let id: String
    let title: String
    let systemImage: String
    let makeView: (ManagementContext) -> AnyView
}

/// The host's lists of management screens. Later slices add their own entry here. Keep one entry per line so that
/// concurrent additions merge cleanly.
enum ManagementCatalog {
    static let searchEntryID = "search"
    static let settingsEntryID = "settings"

    /// Screens the capture surface links to. They are secondary: capture is always the first screen.
    static let secondaryEntries: [ManagementEntry] = [
        ManagementEntry(id: searchEntryID, title: "Search", systemImage: "magnifyingglass") { context in AnyView(RetrievalSearchView(model: context.retrieval, renderReporter: context.retrievalRenderReporter)) },
        ManagementEntry(id: settingsEntryID, title: "Settings", systemImage: "gearshape") { context in AnyView(SettingsRootView(sections: ManagementCatalog.settingsSections, context: context)) },
    ]

    /// Sections of the settings root.
    static let settingsSections: [ManagementEntry] = [
        ManagementEntry(id: "voice-recovery", title: "Unfinished recordings", systemImage: "waveform") { context in AnyView(VoiceRecoveryView(model: context.voiceRecovery).navigationTitle("Unfinished recordings").navigationBarTitleDisplayMode(.inline)) },
        ManagementEntry(id: "audio-retention", title: "Audio retention", systemImage: "externaldrive") { context in AnyView(RetentionSettingsView(model: context.retention)) },
        ManagementEntry(id: "provider-settings", title: "Providers", systemImage: "server.rack") { context in AnyView(ProviderSettingsView(model: context.providerSettings)) },
        ManagementEntry(id: "prompt-settings", title: "Daily prompt", systemImage: "bell.badge") { context in AnyView(PromptSettingsView(model: context.promptSettings)) },
    ]

    /// Where a slice registers the screen for an item or the current suggestion. Set the slice's own line, nothing else.
    static let destinations = ManagementDestinations(
        item: { context, itemID in AnyView(SourceDetailScreen(itemID: itemID, makeModel: context.sourceDetail).id(itemID)) },
        suggestion: { context in AnyView(SuggestionView(model: context.suggestion)) })

    static func entry(withID id: String) -> ManagementEntry? {
        secondaryEntries.first { $0.id == id }
    }
}

/// Shown until the item source-detail screen is registered.
struct ItemPlaceholderView: View {
    static let identifier = "management.item.placeholder"

    var body: some View {
        Text("This item's details are not available in this version yet.")
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .accessibilityIdentifier(Self.identifier)
            .reportPlaceholderFrame(Self.identifier)
            .navigationTitle("Item")
            .navigationBarTitleDisplayMode(.inline)
    }
}

/// Shown until the suggestion screen is registered.
struct SuggestionPlaceholderView: View {
    static let identifier = "management.suggestion.placeholder"

    var body: some View {
        Text("Suggestions are not available in this version yet.")
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .accessibilityIdentifier(Self.identifier)
            .reportPlaceholderFrame(Self.identifier)
            .navigationTitle("Suggestion")
            .navigationBarTitleDisplayMode(.inline)
    }
}

struct SettingsRootView: View {
    static let emptyIdentifier = "management.settings.empty"

    let sections: [ManagementEntry]
    let context: ManagementContext

    var body: some View {
        Group {
            if sections.isEmpty {
                Text("There is nothing to set up yet.")
                    .font(.body)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
                    .accessibilityIdentifier(Self.emptyIdentifier)
            } else {
                List(sections) { section in
                    NavigationLink {
                        section.makeView(context)
                    } label: {
                        Label(section.title, systemImage: section.systemImage)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
    }
}
