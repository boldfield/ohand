import SwiftUI
import UIKit

/// What the status screen shows, keyed by accessibility identifier, so a hosted test can read it.
struct StatusRenderedKey: PreferenceKey {
    static var defaultValue: [String: String] = [:]

    static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

/// The status screen. Everything it shows comes from `StatusModel`; it is reachable but never required.
struct StatusView: View {
    static let retryIdentifier = "status.retry"
    static let openSettingsIdentifier = "status.open-settings"
    static let refreshIdentifier = "status.refresh"
    static let providerSettingsIdentifier = "status.provider-settings"
    static let retryNoteIdentifier = "status.retry-note"
    static func rowIdentifier(_ kind: StatusFactKind) -> String { "status.row.\(kind.rawValue)" }

    static let infoParagraphs = [
        StatusText.introText,
        "Trying again only asks the waiting work to run once more from the saved original. It never changes what you wrote.",
    ]

    @ObservedObject var model: StatusModel
    /// The Provider settings screen this one links to when a destination needs the person.
    var makeProviderSettings: () -> AnyView = { AnyView(EmptyView()) }
    /// Lets a hosted test see what the screen shows; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    var body: some View {
        SettingsScreenChrome(title: "Status", infoTitle: "Status", infoParagraphs: Self.infoParagraphs) {
            DSGroupedSection(footer: StatusText.introText) {
                ForEach(model.facts) { fact in
                    VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                        Text(fact.headline)
                            .font(DSFont.rowLabel)
                            .foregroundColor(DSColor.ink.color)
                        Text(fact.detail)
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.horizontal, DSSpacing.md)
                    .padding(.vertical, DSSpacing.xs)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier(Self.rowIdentifier(fact.kind))
                    .preference(key: StatusRenderedKey.self, value: [Self.rowIdentifier(fact.kind): "\(fact.headline) | \(fact.detail)"])
                }
            }
            if model.facts.contains(where: { $0.action == .retry }) {
                Button(StatusText.retryLabel) { Task { await model.retry() } }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.retryIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.retryIdentifier: StatusText.retryLabel])
            }
            if model.retryRequested {
                Text(StatusText.retryRequestedNote)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, DSSpacing.md)
                    .accessibilityIdentifier(Self.retryNoteIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.retryNoteIdentifier: StatusText.retryRequestedNote])
            }
            if model.facts.contains(where: { $0.action == .openProviderSettings }) {
                NavigationLink(StatusText.providerSettingsLabel) { makeProviderSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.providerSettingsIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.providerSettingsIdentifier: StatusText.providerSettingsLabel])
            }
            if model.facts.contains(where: { $0.action == .openSettings }) {
                Button(StatusText.openSettingsLabel) { model.openSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.openSettingsIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.openSettingsIdentifier: StatusText.openSettingsLabel])
            }
            Button(StatusText.refreshLabel) { Task { await model.refresh() } }
                .font(DSFont.rowLabel)
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .accessibilityIdentifier(Self.refreshIdentifier)
                    .preference(key: StatusRenderedKey.self, value: [Self.refreshIdentifier: StatusText.refreshLabel])
        }
        .onPreferenceChange(StatusRenderedKey.self) { renderReporter?($0) }
        .task { await model.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            Task { await model.refresh() }
        }
    }
}
