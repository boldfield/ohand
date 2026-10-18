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

    @ObservedObject var model: PrivacySettingsModel

    init(model: PrivacySettingsModel) { self.model = model }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) { content }.padding()
        }
        .navigationTitle("Privacy")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.refresh() }
        .onDisappear { model.clear() }
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
            if route.grantedCapabilities.isEmpty {
                note(PrivacySettingsText.routeLocalOnly, identifier: Self.routeStatusIdentifier(route.routeID))
            } else {
                ForEach(route.grantedCapabilities, id: \.capability) { grant in
                    Text(PrivacySettingsText.grantSummary(grant)).font(.footnote)
                }
                .accessibilityIdentifier(Self.routeStatusIdentifier(route.routeID))
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
            Button {
                model.cancel()
            } label: {
                Text("Not now").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.bordered)
            .disabled(model.isWorking)
            .accessibilityIdentifier(Self.cancelIdentifier)
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text).font(.footnote).foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityIdentifier(identifier)
    }
}

extension PrivacySettingsModel {
    /// The model as the app assembles it: the root's provider configuration, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> PrivacySettingsModel {
        PrivacySettingsModel(
            controls: root.providerConfiguration, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
