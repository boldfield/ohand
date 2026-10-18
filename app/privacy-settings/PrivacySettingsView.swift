import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// The processing-policy screen: every route with what it may send, and deliberate steps to allow one more capability
/// on one route for one destination. Everything it shows comes from `PrivacySettingsModel`.
struct PrivacySettingsView: View {
    static let noticeIdentifier = "privacy-settings.notice"
    static let lockedIdentifier = "privacy-settings.locked"
    static let failureIdentifier = "privacy-settings.failure"
    static let tryAgainIdentifier = "privacy-settings.try-again"
    static let noProfilesIdentifier = "privacy-settings.no-profiles"
    static let questionIdentifier = "privacy-settings.question"
    static let confirmationNoteIdentifier = "privacy-settings.confirmation-note"
    static let allowIdentifier = "privacy-settings.allow"
    static let cancelIdentifier = "privacy-settings.cancel"
    static let actionStateIdentifier = "privacy-settings.action-state"
    static func routeIdentifier(_ route: String) -> String { "privacy-settings.route.\(route)" }
    static func routeStatusIdentifier(_ route: String) -> String { "privacy-settings.route-status.\(route)" }
    static func offerIdentifier(_ route: String, _ capability: String, _ version: String) -> String {
        "privacy-settings.offer.\(route).\(capability).\(version)"
    }

    /// What each identified part of the screen shows, collected so a hosted test can read the rendered words.
    struct RenderedKey: PreferenceKey {
        static var defaultValue: [String: String] { [:] }
        static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
            value.merge(nextValue()) { _, new in new }
        }
    }

    @ObservedObject var model: PrivacySettingsModel
    /// Lets a hosted test see what the screen renders; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    init(model: PrivacySettingsModel, renderReporter: (([String: String]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

    var body: some View {
        SettingsScreenChrome(title: "Privacy", infoTitle: "Privacy", infoParagraphs: PrivacySettingsText.infoParagraphs) {
            content
        }
        .task { await model.refresh() }
        .onDisappear { model.clear() }
        .onPreferenceChange(RenderedKey.self) { renderReporter?($0) }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView("Loading").frame(maxWidth: .infinity)
        case .locked:
            note(PrivacySettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            DSPillButton(title: "Try again", style: .secondary) { Task { await model.refresh() } }
                .accessibilityIdentifier(Self.tryAgainIdentifier)
                .reportPrivacyRendered(Self.tryAgainIdentifier, text: "Try again")
        case .loaded:
            ForEach(model.routes, id: \.routeID) { route in routeSection(route) }
            note(PrivacySettingsText.localFirstNotice, identifier: Self.noticeIdentifier)
            if model.listing?.profiles.isEmpty ?? true {
                note(PrivacySettingsText.noProfilesNotice, identifier: Self.noProfilesIdentifier)
            }
            if let proposal = model.proposal { proposalCard(proposal) }
            if let state = model.actionState {
                note(state.message, identifier: Self.actionStateIdentifier)
            }
        }
    }

    /// One decision per route: what it may send now, then a row for each destination it could also be allowed.
    private func routeSection(_ route: RoutePolicyRoute) -> some View {
        DSGroupedSection(title: PrivacySettingsText.routeLabel(route)) {
            if route.grantedCapabilities.isEmpty {
                DSRow(label: PrivacySettingsText.routeLocalOnly, iconSystemName: "iphone", showsChevron: false)
                    .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
                    .reportPrivacyRendered(Self.routeStatusIdentifier(route.routeID), text: PrivacySettingsText.routeLocalOnly)
            } else {
                VStack(spacing: 0) {
                    ForEach(route.grantedCapabilities, id: \.capability) { grant in
                        DSRow(
                            label: PrivacySettingsText.grantSummary(grant), iconSystemName: "checkmark.shield",
                            showsChevron: false)
                    }
                }
                .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
                .reportPrivacyRendered(
                    Self.routeStatusIdentifier(route.routeID),
                    text: route.grantedCapabilities.map(PrivacySettingsText.grantSummary).joined(separator: "\n"))
            }
            ForEach(PrivacySettingsModel.grantableCapabilities, id: \.self) { capability in
                if !model.isGranted(routeID: route.routeID, capability: capability) {
                    ForEach(model.offeredVersions(for: capability), id: \.version.profileVersion) { offer in
                        offerRow(route: route, capability: capability, offer: offer)
                    }
                }
            }
        }
        .accessibilityIdentifier(Self.routeIdentifier(route.routeID))
        .reportPrivacyRendered(Self.routeIdentifier(route.routeID), text: PrivacySettingsText.routeLabel(route))
    }

    private func offerRow(
        route: RoutePolicyRoute, capability: String, offer: PrivacyOfferedVersion
    ) -> some View {
        let label = "Allow \(PrivacySettingsText.capabilityName(capability).lowercased()) via "
            + PrivacySettingsText.versionLabel(profileID: offer.profileID, version: offer.version)
        let identifier = Self.offerIdentifier(route.routeID, capability, offer.version.profileVersion)
        return DSRow(label: label, iconSystemName: "plus", showsChevron: false) {
            model.propose(routeID: route.routeID, capability: capability, profileVersion: offer.version.profileVersion)
        }
        .reportPrivacyRendered(identifier, text: label)
        .accessibilityIdentifier(identifier)
        // Disabled outside the report so the report reads the disabled environment.
        .disabled(model.isWorking)
    }

    /// The deliberate step: the question, its one footer line, then Allow and Not now.
    private func proposalCard(_ proposal: PrivacyGrantProposal) -> some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                Text(PrivacySettingsText.proposalQuestion(proposal))
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.questionIdentifier)
                    .reportPrivacyRendered(Self.questionIdentifier, text: PrivacySettingsText.proposalQuestion(proposal))
                Text(PrivacySettingsText.confirmationFooter)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.confirmationNoteIdentifier)
                    .reportPrivacyRendered(Self.confirmationNoteIdentifier, text: PrivacySettingsText.confirmationFooter)
                DSPillButton(title: "Allow", style: .primary) { Task { await model.confirm(proposal) } }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.allowIdentifier, text: "Allow")
                    .accessibilityIdentifier(Self.allowIdentifier)
                    .disabled(model.isWorking)
                DSPillButton(title: "Not now", style: .plain) { model.cancel() }
                    .frame(maxWidth: .infinity)
                    .reportPrivacyRendered(Self.cancelIdentifier, text: "Not now")
                    .accessibilityIdentifier(Self.cancelIdentifier)
                    .disabled(model.isWorking)
            }
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, DSSpacing.md)
            .accessibilityIdentifier(identifier)
            .reportPrivacyRendered(identifier, text: text)
    }
}

/// Reports what an identified part shows, read from the laid-out view: nothing while it has no size, and
/// " (disabled)" after the text when the environment has disabled it.
private struct PrivacyRenderedReport: ViewModifier {
    let identifier: String
    let text: String
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.background(
            GeometryReader { proxy in
                Color.clear.preference(
                    key: PrivacySettingsView.RenderedKey.self,
                    value: proxy.size.width > 0 && proxy.size.height > 0
                        ? [identifier: isEnabled ? text : "\(text) (disabled)"] : [:])
            })
    }
}

extension View {
    fileprivate func reportPrivacyRendered(_ identifier: String, text: String) -> some View {
        modifier(PrivacyRenderedReport(identifier: identifier, text: text))
    }
}

extension PrivacySettingsModel {
    /// The model as the app assembles it: the root's provider configuration, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> PrivacySettingsModel {
        PrivacySettingsModel(
            controls: root.providerConfiguration, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
