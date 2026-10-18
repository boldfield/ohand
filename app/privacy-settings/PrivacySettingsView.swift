import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// The processing-policy screen: every route with what it may send, and deliberate steps to allow one more capability
/// on one route for one destination. Everything it shows comes from `PrivacySettingsModel`.
struct PrivacySettingsView: View {
    static let noticeIdentifier = "privacy-settings.notice"
    static let stickyIdentifier = "privacy-settings.sticky"
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
        ScrollView {
            VStack(alignment: .leading, spacing: 16) { content }.padding()
        }
        .navigationTitle("Privacy")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.refresh() }
        .onDisappear { model.clear() }
        .onPreferenceChange(RenderedKey.self) { renderReporter?($0) }
    }

    @ViewBuilder
    private var content: some View {
        note(PrivacySettingsText.localFirstNotice, identifier: Self.noticeIdentifier)
        switch model.phase {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView("Loading").frame(maxWidth: .infinity)
        case .locked:
            note(PrivacySettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            Button("Try again") { Task { await model.refresh() } }
                .frame(minHeight: 44)
                .accessibilityIdentifier(Self.tryAgainIdentifier)
        case .loaded:
            note(PrivacySettingsText.stickyNotice, identifier: Self.stickyIdentifier)
            if model.listing?.profiles.isEmpty ?? true {
                note(PrivacySettingsText.noProfilesNotice, identifier: Self.noProfilesIdentifier)
            }
            ForEach(model.routes, id: \.routeID) { route in routeSection(route) }
            if let proposal = model.proposal { proposalSection(proposal) }
            if let state = model.actionState {
                note(state.message, identifier: Self.actionStateIdentifier)
            }
        }
    }

    private func routeSection(_ route: RoutePolicyRoute) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(PrivacySettingsText.routeLabel(route)).font(.headline)
                .accessibilityIdentifier(Self.routeIdentifier(route.routeID))
                .preference(
                    key: RenderedKey.self,
                    value: [Self.routeIdentifier(route.routeID): PrivacySettingsText.routeLabel(route)])
            if route.grantedCapabilities.isEmpty {
                note(PrivacySettingsText.routeLocalOnly, identifier: Self.routeStatusIdentifier(route.routeID))
            } else {
                ForEach(route.grantedCapabilities, id: \.capability) { grant in
                    Text(PrivacySettingsText.grantSummary(grant)).font(.footnote)
                }
                .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
                .preference(
                    key: RenderedKey.self,
                    value: [
                        Self.routeStatusIdentifier(route.routeID):
                            route.grantedCapabilities.map(PrivacySettingsText.grantSummary).joined(separator: "\n")
                    ])
            }
            ForEach(PrivacySettingsModel.grantableCapabilities, id: \.self) { capability in
                if !model.isGranted(routeID: route.routeID, capability: capability) {
                    ForEach(model.offeredVersions(for: capability), id: \.version.profileVersion) { offer in
                        Button {
                            model.propose(
                                routeID: route.routeID, capability: capability,
                                profileVersion: offer.version.profileVersion)
                        } label: {
                            Text(
                                "Allow \(PrivacySettingsText.capabilityName(capability).lowercased()) via "
                                    + PrivacySettingsText.versionLabel(profileID: offer.profileID, version: offer.version)
                            )
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        }
                        .buttonStyle(.bordered)
                        .disabled(model.isWorking)
                        .accessibilityIdentifier(
                            Self.offerIdentifier(route.routeID, capability, offer.version.profileVersion))
                        .preference(
                            key: RenderedKey.self,
                            value: [
                                Self.offerIdentifier(route.routeID, capability, offer.version.profileVersion):
                                    "Allow \(PrivacySettingsText.capabilityName(capability).lowercased()) via "
                                    + PrivacySettingsText.versionLabel(profileID: offer.profileID, version: offer.version)
                            ])
                    }
                }
            }
        }
    }

    private func proposalSection(_ proposal: PrivacyGrantProposal) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            note(PrivacySettingsText.proposalQuestion(proposal), identifier: Self.questionIdentifier)
            note(PrivacySettingsText.confirmationNote, identifier: Self.confirmationNoteIdentifier)
            Button {
                Task { await model.confirm(proposal) }
            } label: {
                Text("Allow").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.isWorking)
            .accessibilityIdentifier(Self.allowIdentifier)
            .preference(key: RenderedKey.self, value: [Self.allowIdentifier: model.isWorking ? "Allow (disabled)" : "Allow"])
            Button {
                model.cancel()
            } label: {
                Text("Not now").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.bordered)
            .disabled(model.isWorking)
            .accessibilityIdentifier(Self.cancelIdentifier)
            .preference(
                key: RenderedKey.self, value: [Self.cancelIdentifier: model.isWorking ? "Not now (disabled)" : "Not now"])
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text).font(.footnote).foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(identifier)
            .preference(key: RenderedKey.self, value: [identifier: text])
    }
}

extension PrivacySettingsModel {
    /// The model as the app assembles it: the root's provider configuration, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> PrivacySettingsModel {
        PrivacySettingsModel(
            controls: root.providerConfiguration, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
