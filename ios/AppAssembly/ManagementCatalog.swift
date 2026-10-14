import SwiftUI

/// What the host hands each management screen when it builds it: the models that screens present.
struct ManagementContext {
    let voiceRecovery: VoiceRecoveryModel
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
        ManagementEntry(id: searchEntryID, title: "Search", systemImage: "magnifyingglass") { _ in AnyView(SearchPlaceholderView()) },
        ManagementEntry(id: settingsEntryID, title: "Settings", systemImage: "gearshape") { context in AnyView(SettingsRootView(sections: ManagementCatalog.settingsSections, context: context)) },
    ]

    /// Sections of the settings root.
    static let settingsSections: [ManagementEntry] = [
        ManagementEntry(id: "voice-recovery", title: "Unfinished recordings", systemImage: "waveform") { context in AnyView(VoiceRecoveryView(model: context.voiceRecovery).navigationTitle("Unfinished recordings").navigationBarTitleDisplayMode(.inline)) },
    ]

    static func entry(withID id: String) -> ManagementEntry? {
        secondaryEntries.first { $0.id == id }
    }
}

/// Shown until the retrieval screens exist. It says so rather than showing an empty result list.
struct SearchPlaceholderView: View {
    static let identifier = "management.search.placeholder"

    var body: some View {
        Text("Search is not available in this version yet. Your captures are saved.")
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            .accessibilityIdentifier(Self.identifier)
            .navigationTitle("Search")
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
