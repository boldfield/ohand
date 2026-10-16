import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// The provider settings screen: the current operating mode, the stored profile versions with default choices, and the
/// form for a new version. Everything it shows comes from `ProviderSettingsModel`.
struct ProviderSettingsView: View {
    static let modeIdentifier = "provider-settings.mode"
    static let defaultNoteIdentifier = "provider-settings.default-note"
    static let lockedIdentifier = "provider-settings.locked"
    static let failureIdentifier = "provider-settings.failure"
    static let tryAgainIdentifier = "provider-settings.try-again"
    static let defaultStateIdentifier = "provider-settings.default-state"
    static let saveStateIdentifier = "provider-settings.save-state"
    static let issuesIdentifier = "provider-settings.issues"
    static let saveIdentifier = "provider-settings.save"
    static let localOnlyIdentifier = "provider-settings.local-only"

    static func versionIdentifier(_ version: String) -> String { "provider-settings.version.\(version)" }
    static func makeDefaultIdentifier(_ version: String) -> String { "provider-settings.make-default.\(version)" }

    @ObservedObject var model: ProviderSettingsModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
            }
            .padding()
        }
        .navigationTitle("Providers")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.refresh() }
        .onDisappear { model.clear() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView("Loading").frame(maxWidth: .infinity)
        case .locked:
            note(ProviderSettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            Button("Try again") { Task { await model.refresh() } }
                .frame(minHeight: 44)
                .accessibilityIdentifier(Self.tryAgainIdentifier)
        case .loaded:
            modeSection
            versionsSection
            formSection
        }
    }

    private var modeSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Current provider").font(.headline)
            note(Self.modeText(model.operatingMode), identifier: Self.modeIdentifier)
            note(ProviderSettingsText.defaultChangeNote, identifier: Self.defaultNoteIdentifier)
            if model.defaultVersion != nil {
                Button {
                    Task { await model.useLocalOnly() }
                } label: {
                    Text("Use this device only").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .disabled(model.isChangingDefault)
                .accessibilityIdentifier(Self.localOnlyIdentifier)
            }
            if let message = model.defaultState?.message {
                note(message, identifier: Self.defaultStateIdentifier)
            }
        }
    }

    static func modeText(_ mode: ProviderSettingsOperatingMode) -> String {
        switch mode {
        case .localOnly: return ProviderSettingsText.localOnlyNotice
        case .privateServer(let model): return "Private server, model \(model). " + ProviderSettingsText.privateServerNotice
        case .hosted(let name, let model): return "\(name), model \(model). " + ProviderSettingsText.hostedNotice
        }
    }

    @ViewBuilder
    private var versionsSection: some View {
        Text("Saved profiles").font(.headline)
        if let profiles = model.listing?.profiles, !profiles.isEmpty {
            ForEach(profiles, id: \.profileID) { profile in
                ForEach(profile.versions.reversed(), id: \.profileVersion) { version in
                    versionRow(profile.profileID, version)
                }
            }
        } else {
            note("No profiles saved yet.", identifier: "provider-settings.none")
        }
    }

    private func versionRow(_ profileID: String, _ version: ProviderProfileListAnswer.Version) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(profileID).font(.body)
            Text("\(ProviderSettingsText.protocolName(version.providerProtocol)) · \(version.model)")
                .font(.footnote).foregroundColor(.secondary)
            if let endpoint = version.endpoint {
                Text(endpoint).font(.footnote).foregroundColor(.secondary)
            }
            Text(version.capabilities.map { ProviderSettingsText.capabilityName($0.capability) }.joined(separator: ", "))
                .font(.footnote).foregroundColor(.secondary)
            if version.revoked {
                Text("Revoked").font(.footnote)
            } else if !version.defaultFor.isEmpty {
                Text("Current default").font(.footnote)
            }
            if model.canMakeDefault(version) {
                Button {
                    Task { await model.makeDefault(version) }
                } label: {
                    Text("Make default").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier(Self.makeDefaultIdentifier(version.profileVersion))
            }
        }
        .padding()
        .background(Color(.secondarySystemBackground))
        .cornerRadius(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.versionIdentifier(version.profileVersion))
    }

    @ViewBuilder
    private var formSection: some View {
        Text("New profile version").font(.headline)
        TextField("Profile name", text: $model.draft.profileName)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .accessibilityIdentifier("provider-settings.field.name")
        Picker("Provider type", selection: $model.draft.providerProtocol) {
            ForEach(ProviderSettingsProtocol.allCases, id: \.self) { choice in
                Text(ProviderSettingsText.protocolName(choice.rawValue)).tag(choice)
            }
        }
        .accessibilityIdentifier("provider-settings.field.protocol")
        if model.draft.providerProtocol.takesEndpoint {
            TextField("https://server address", text: $model.draft.endpoint)
                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                .accessibilityIdentifier("provider-settings.field.endpoint")
        }
        TextField("Model", text: $model.draft.model)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .accessibilityIdentifier("provider-settings.field.model")
        TextField("Saved credential name", text: $model.draft.credentialReference)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .accessibilityIdentifier("provider-settings.field.credential")
        note(ProviderSettingsText.credentialNote, identifier: "provider-settings.credential-note")
        Stepper(
            value: $model.draft.timeoutSeconds,
            in: ProviderSettingsModel.minimumTimeoutSeconds...ProviderSettingsModel.maximumTimeoutSeconds
        ) {
            Text("Timeout \(model.draft.timeoutSeconds) seconds")
        }
        .accessibilityIdentifier("provider-settings.field.timeout")
        note(ProviderSettingsText.capabilityNote, identifier: "provider-settings.capability-note")
        if !model.issues.isEmpty {
            note(model.issues.map(ProviderSettingsText.issue).joined(separator: " "), identifier: Self.issuesIdentifier)
        }
        Button {
            Task { await model.save() }
        } label: {
            Text("Save profile").frame(maxWidth: .infinity, minHeight: 44)
        }
        .buttonStyle(.borderedProminent)
        .disabled(!model.canSave)
        .accessibilityIdentifier(Self.saveIdentifier)
        if let message = model.saveState?.message {
            note(message, identifier: Self.saveStateIdentifier)
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(identifier)
    }
}

extension ProviderSettingsModel {
    /// The model as the app assembles it: the root's provider configuration, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> ProviderSettingsModel {
        ProviderSettingsModel(
            controls: root.providerConfiguration, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
