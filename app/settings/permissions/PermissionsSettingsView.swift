import SwiftUI
import UIKit
import OhAndServices

/// The permissions screen: for each permission, whether it is on, what still works if it is not, and how to turn it on.
/// Everything it shows comes from `PermissionsSettingsModel`.
struct PermissionsSettingsView: View {
    static let openSettingsIdentifier = "permissions-settings.open-settings"
    static func rowIdentifier(_ kind: PermissionKind) -> String { "permissions-settings.row.\(kind.rawValue)" }

    static let introText = "Oh And asks for each permission only when you first use the feature that needs it. Saving a typed or silent capture and finding it again never needs any of them."
    static let openSettingsLabel = "Open iOS Settings"

    static let infoParagraphs = [introText, "If you say no, Oh And does not ask again. You can switch a permission on in iOS Settings at any time, and this screen updates when you come back."]

    @ObservedObject var model: PermissionsSettingsModel

    var body: some View {
        SettingsScreenChrome(
            title: "Permissions", infoTitle: "Permissions", infoParagraphs: Self.infoParagraphs
        ) {
            DSGroupedSection(footer: Self.introText) {
                ForEach(model.rows) { row in
                    VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                        Text(row.guidance.headline)
                            .font(DSFont.rowLabel)
                            .foregroundColor(DSColor.ink.color)
                        Text(row.guidance.detail)
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.horizontal, DSSpacing.md)
                    .padding(.vertical, DSSpacing.xs)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier(Self.rowIdentifier(row.kind))
                }
            }
            if model.rows.contains(where: { $0.guidance.recovery == .openSettings }) {
                Button(Self.openSettingsLabel) { model.openSettings() }
                    .font(DSFont.rowLabel)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(minHeight: DSLayout.rowMinimumHeight)
                    .accessibilityIdentifier(Self.openSettingsIdentifier)
            }
        }
        .task { await model.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            Task { await model.refresh() }
        }
    }
}
