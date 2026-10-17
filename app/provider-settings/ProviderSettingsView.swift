import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// Where an element of the provider settings screen landed and the words it shows, so a hosted test can see what is on
/// screen.
struct ProviderSettingsRenderedElement: Equatable {
    let frame: CGRect
    let text: String
    let isEnabled: Bool
}

private struct ProviderSettingsRenderedKey: PreferenceKey {
    static var defaultValue: [String: ProviderSettingsRenderedElement] = [:]

    static func reduce(
        value: inout [String: ProviderSettingsRenderedElement],
        nextValue: () -> [String: ProviderSettingsRenderedElement]
    ) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

private struct ProviderSettingsRenderReport: ViewModifier {
    let identifier: String
    let text: String
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.background(GeometryReader { proxy in
            Color.clear.preference(
                key: ProviderSettingsRenderedKey.self,
                value: [identifier: ProviderSettingsRenderedElement(
                    frame: proxy.frame(in: .global), text: text, isEnabled: isEnabled)])
        })
    }
}

private extension View {
    func reportRendered(_ identifier: String, text: String) -> some View {
        modifier(ProviderSettingsRenderReport(identifier: identifier, text: text))
    }
}

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
    static let defaultUnavailableIdentifier = "provider-settings.default-unavailable"
    static let nameFieldIdentifier = "provider-settings.field.name"
    static let modelFieldIdentifier = "provider-settings.field.model"
    static let credentialFieldIdentifier = "provider-settings.field.credential"
    static let endpointFieldIdentifier = "provider-settings.field.endpoint"
    static let secretFieldIdentifier = "provider-settings.field.secret"
    static let storeSecretIdentifier = "provider-settings.store-secret"
    static let removeSecretIdentifier = "provider-settings.remove-secret"
    static let checkSecretIdentifier = "provider-settings.check-secret"
    static let credentialStateIdentifier = "provider-settings.credential-state"
    static func checkConnectionIdentifier(_ version: String) -> String { "provider-settings.check-connection.\(version)" }
    static func connectionStateIdentifier(_ version: String) -> String { "provider-settings.connection-state.\(version)" }

    static func versionIdentifier(_ version: String) -> String { "provider-settings.version.\(version)" }
    static func makeDefaultIdentifier(_ version: String) -> String { "provider-settings.make-default.\(version)" }
    static func capabilityIdentifier(_ version: String, _ capability: String) -> String {
        "provider-settings.capability.\(version).\(capability)"
    }

    @ObservedObject var model: ProviderSettingsModel
    /// The secret as typed. It lives only here: it is handed to the model as `Data` and emptied at once, whether or not
    /// the store succeeded, and whenever the app leaves the foreground.
    @State private var secretText = ""
    @Environment(\.scenePhase) private var scenePhase
    var renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)?

    init(
        model: ProviderSettingsModel,
        renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)? = nil
    ) {
        self.model = model
        self.renderReporter = renderReporter
    }

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
        .onPreferenceChange(ProviderSettingsRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onDisappear {
            secretText = ""
            model.clear()
        }
        .onChange(of: scenePhase) { phase in
            if phase != .active { secretText = "" }
        }
    }

    /// Hands the typed secret to the model as `Data` and empties the field before anything else can read it.
    private func submitSecret() {
        let trimmed = secretText.trimmingCharacters(in: .whitespacesAndNewlines)
        secretText = ""
        guard !trimmed.isEmpty else { model.noteEmptySecret(); return }
        model.storeCredential(Data(trimmed.utf8))
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
            if let availability = model.defaultAvailability, availability != .ready {
                note(ProviderSettingsText.unavailableDefaultNotice, identifier: Self.defaultUnavailableIdentifier)
            }
            note(ProviderSettingsText.defaultChangeNote, identifier: Self.defaultNoteIdentifier)
            if model.defaultVersion != nil {
                Button {
                    Task { await model.useLocalOnly() }
                } label: {
                    Text("Use this device only").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .reportRendered(Self.localOnlyIdentifier, text: "Use this device only")
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
            ForEach(version.capabilities, id: \.capability) { capability in
                let status = ProviderSettingsText.capabilityStatus(capability.capability, support: capability.support)
                Text(status)
                    .font(.footnote).foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .reportRendered(Self.capabilityIdentifier(version.profileVersion, capability.capability), text: status)
            }
            if version.revoked {
                Text("Revoked").font(.footnote)
            } else if !version.defaultFor.isEmpty {
                Text("Current default").font(.footnote)
            }
            if model.canCheckConnection(version) {
                Button {
                    Task { await model.checkConnection(version) }
                } label: {
                    Text("Check connection").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .reportRendered(Self.checkConnectionIdentifier(version.profileVersion), text: "Check connection")
                .accessibilityIdentifier(Self.checkConnectionIdentifier(version.profileVersion))
            }
            if let state = model.connectionStates[version.profileVersion] {
                note(state.message, identifier: Self.connectionStateIdentifier(version.profileVersion))
            }
            if model.canMakeDefault(version) {
                Button {
                    Task { await model.makeDefault(version) }
                } label: {
                    Text("Make default").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .reportRendered(Self.makeDefaultIdentifier(version.profileVersion), text: "Make default")
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
            .reportRendered("provider-settings.field.name", text: model.draft.profileName)
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
                .reportRendered(Self.endpointFieldIdentifier, text: model.draft.endpoint)
                .accessibilityIdentifier(Self.endpointFieldIdentifier)
        }
        TextField("Model", text: $model.draft.model)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .reportRendered("provider-settings.field.model", text: model.draft.model)
            .accessibilityIdentifier("provider-settings.field.model")
        TextField("Credential name (filled in when you store a secret)", text: $model.draft.credentialReference)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .reportRendered("provider-settings.field.credential", text: model.draft.credentialReference)
            .accessibilityIdentifier("provider-settings.field.credential")
        SecureField("Secret (API key)", text: $secretText)
            .textInputAutocapitalization(.never).autocorrectionDisabled().textContentType(.none)
            .accessibilityIdentifier(Self.secretFieldIdentifier)
        HStack {
            Button("Store secret") { submitSecret() }
                .frame(minHeight: 44)
                .reportRendered(Self.storeSecretIdentifier, text: "Store secret")
                .accessibilityIdentifier(Self.storeSecretIdentifier)
            Button("Check saved") { model.checkCredential() }
                .frame(minHeight: 44)
                .reportRendered(Self.checkSecretIdentifier, text: "Check saved")
                .accessibilityIdentifier(Self.checkSecretIdentifier)
            Button("Remove", role: .destructive) { model.removeCredential() }
                .frame(minHeight: 44)
                .reportRendered(Self.removeSecretIdentifier, text: "Remove")
                .accessibilityIdentifier(Self.removeSecretIdentifier)
        }
        .buttonStyle(.bordered)
        if let state = model.credentialState {
            note(state.message, identifier: Self.credentialStateIdentifier)
        }
        note(ProviderSettingsText.credentialNote, identifier: "provider-settings.credential-note")
        note(ProviderSettingsText.connectionCheckNote, identifier: "provider-settings.connection-note")
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
        .reportRendered(Self.saveIdentifier, text: "Save profile")
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
            .reportRendered(identifier, text: text)
            .accessibilityIdentifier(identifier)
    }
}

extension ProviderSettingsModel {
    /// The model as the app assembles it: the root's provider configuration, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> ProviderSettingsModel {
        ProviderSettingsModel(
            controls: root.providerConfiguration, credentials: root.providerCredentials, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
