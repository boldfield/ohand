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

/// What the settings root shows on each row, keyed by the row's entry id, so a hosted test can read it.
struct SettingsRootRenderedKey: PreferenceKey {
    static var defaultValue: [String: String] = [:]

    static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

struct SettingsRootView: View {
    static let emptyIdentifier = "management.settings.empty"
    static let eraseIdentifier = "management.settings.erase"
    static let versionIdentifier = "management.settings.version"
    static let exportEntryID = "export"

    /// The design's row order for the two groups. Rows missing from `sections` are skipped.
    static let firstGroupEntryIDs = ["prompt-settings", "voice-recovery"]
    static let secondGroupEntryIDs = ["provider-settings", "audio-retention", exportEntryID]

    let sections: [ManagementEntry]
    let context: ManagementContext
    /// Lets a hosted test see each row's label and value; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    @Environment(\.dismiss) private var dismiss
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass

    private var firstGroup: [ManagementEntry] { Self.ordered(Self.firstGroupEntryIDs, from: sections) }

    /// Processing, Audio, then Export, which lives with the other data settings.
    private var secondGroup: [ManagementEntry] {
        Self.ordered(Self.secondGroupEntryIDs, from: sections + (context.retention.exportSettings == nil ? [] : [Self.exportEntry]))
    }

    static func ordered(_ ids: [String], from entries: [ManagementEntry]) -> [ManagementEntry] {
        ids.compactMap { id in entries.first { $0.id == id } }
    }

    private static let exportEntry = ManagementEntry(id: exportEntryID, title: "Export", systemImage: "square.and.arrow.up") { context in
        context.retention.exportSettings.map { AnyView(ExportSettingsView(model: $0)) } ?? AnyView(EmptyView())
    }

    private var versionText: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        return version.map { "Oh And \($0)" } ?? "Oh And"
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: DSSpacing.lg) {
                HStack {
                    DSGlassButton(systemName: "chevron.left", accessibilityLabel: "Back") { dismiss() }
                    Spacer()
                }
                Text("Settings")
                    .font(DSFont.displayXL)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                if sections.isEmpty {
                    Text("There is nothing to set up yet.")
                        .font(DSFont.body)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityIdentifier(Self.emptyIdentifier)
                } else {
                    group(firstGroup)
                    group(secondGroup)
                }
                // Presentation only: the erase action is wired by the reset task (U07c), so this card is not yet
                // interactive.
                DSCard {
                    Text("Erase everything")
                        .font(DSFont.button)
                        .foregroundColor(DSColor.destructive.color)
                        .frame(maxWidth: .infinity)
                }
                .accessibilityIdentifier(Self.eraseIdentifier)
                Text(versionText)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier(Self.versionIdentifier)
            }
            .padding(.horizontal, DSSpacing.sideMargin(for: horizontalSizeClass))
            .padding(.vertical, DSSpacing.md)
        }
        .background(DSColor.surface0.color.ignoresSafeArea())
        .navigationBarBackButtonHidden(true)
        .toolbar(.hidden, for: .navigationBar)
        .onPreferenceChange(SettingsRootRenderedKey.self) { renderReporter?($0) }
        .task { await loadValues() }
    }

    /// Reads the current values the rows show. The models keep their own read-scope rules: a locked session leaves
    /// the stored values (processing, audio) empty rather than showing them.
    private func loadValues() async {
        async let prompt: Void = context.promptSettings.load()
        async let retention: Void = context.retention.refresh()
        async let providers: Void = context.providerSettings.refresh()
        _ = await (prompt, retention, providers)
    }

    @ViewBuilder
    private func group(_ entries: [ManagementEntry]) -> some View {
        if !entries.isEmpty {
            DSGroupedSection {
                ForEach(entries) { entry in
                    NavigationLink {
                        entry.makeView(context)
                    } label: {
                        SettingsEntryRow(entry: entry, context: context)
                            .allowsHitTesting(false)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}

/// One settings row with its current value. A value that is not known yet (not loaded, or the session is locked and
/// the setting is stored state) is left out rather than guessed.
private struct SettingsEntryRow: View {
    let entry: ManagementEntry
    let context: ManagementContext

    var body: some View {
        switch entry.id {
        case "prompt-settings":
            PromptValueRow(entry: entry, model: context.promptSettings)
        case "voice-recovery":
            RecoveryValueRow(entry: entry, model: context.voiceRecovery)
        case "provider-settings":
            ProcessingValueRow(entry: entry, model: context.providerSettings)
        case "audio-retention":
            RetentionValueRow(entry: entry, model: context.retention)
        default:
            reported(entry, value: nil)
        }
    }
}

private func reported(_ entry: ManagementEntry, label: String? = nil, value: String?) -> some View {
    DSRow(label: label ?? entry.title, iconSystemName: entry.systemImage, value: value)
        .preference(key: SettingsRootRenderedKey.self, value: [entry.id: [label ?? entry.title, value].compactMap { $0 }.joined(separator: ": ")])
}

private struct PromptValueRow: View {
    let entry: ManagementEntry
    @ObservedObject var model: PromptSettingsModel

    var body: some View {
        reported(entry, value: model.hasLoaded ? (model.isEnabled ? "On" : "Off") : nil)
    }
}

/// The count comes from the recordings left on this device, not from the stored history, so it is the same whether or
/// not the session is unlocked. It is empty until the first check finishes.
private struct RecoveryValueRow: View {
    let entry: ManagementEntry
    @ObservedObject var model: VoiceRecoveryModel
    @State private var hasRequestedCheck = false

    private var value: String? {
        guard hasRequestedCheck, !model.isChecking else { return nil }
        return model.rows.isEmpty ? "None" : "\(model.rows.count)"
    }

    var body: some View {
        reported(entry, value: value)
            .onAppear {
                hasRequestedCheck = true
                model.refresh()
            }
    }
}

private struct ProcessingValueRow: View {
    let entry: ManagementEntry
    @ObservedObject var model: ProviderSettingsModel

    private var value: String? {
        guard model.phase == .loaded else { return nil }
        switch model.operatingMode {
        case .localOnly: return "On this phone"
        case .privateServer: return "Private server"
        case .hosted(let protocolName, _): return protocolName
        }
    }

    var body: some View {
        reported(entry, label: "Processing", value: value)
    }
}

private struct RetentionValueRow: View {
    let entry: ManagementEntry
    @ObservedObject var model: RetentionSettingsModel

    var body: some View {
        reported(
            entry, label: "Audio",
            value: model.status.map { RetentionSettingsText.dayCount(Int($0.policy.periodSeconds / 86_400)) })
    }
}
