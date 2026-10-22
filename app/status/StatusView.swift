import SwiftUI
import UIKit

/// The status screen. Everything it shows comes from `StatusModel`; it is reachable but never required.
struct StatusView: View {
    static let retryIdentifier = "status.retry"
    static let openSettingsIdentifier = "status.open-settings"
    static let refreshIdentifier = "status.refresh"
    static func rowIdentifier(_ kind: StatusFactKind) -> String { "status.row.\(kind.rawValue)" }

    static let infoParagraphs = [
        StatusText.introText,
        "Trying again only asks the waiting work to run once more from the saved original. It never changes what you wrote.",
    ]

    @ObservedObject var model: StatusModel

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
                }
            }
            if model.facts.contains(where: { $0.action == .retry }) {
                Button(StatusText.retryLabel) { Task { await model.retry() } }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.retryIdentifier)
            }
            if model.facts.contains(where: { $0.action == .openSettings }) {
                Button(StatusText.openSettingsLabel) { model.openSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.openSettingsIdentifier)
            }
            Button(StatusText.refreshLabel) { Task { await model.refresh() } }
                .font(DSFont.rowLabel)
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .accessibilityIdentifier(Self.refreshIdentifier)
        }
        .task { await model.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            Task { await model.refresh() }
        }
    }
}
