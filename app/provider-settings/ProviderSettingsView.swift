import SwiftUI
import OhAndCoreBridge
import OhAndServices

/// Where an element of the provider settings screen landed and the words it shows, so a hosted test can see what is on
/// screen.
struct ProviderSettingsRenderedElement: Equatable {
    let frame: CGRect
    let text: String
    let isEnabled: Bool
    var isSelected = false
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
    let isSelected: Bool
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.background(GeometryReader { proxy in
            Color.clear.preference(
                key: ProviderSettingsRenderedKey.self,
                value: [identifier: ProviderSettingsRenderedElement(
                    frame: proxy.frame(in: .global), text: text, isEnabled: isEnabled, isSelected: isSelected)])
        })
    }
}

private extension View {
    func reportRendered(_ identifier: String, text: String, isSelected: Bool = false) -> some View {
        modifier(ProviderSettingsRenderReport(identifier: identifier, text: text, isSelected: isSelected))
    }
}

/// Why a typed secret was refused before it reached the credential service.
enum ProviderSecretRejection: Equatable {
    case empty
    case surroundingWhitespace
}

/// Holds a typed secret only until it is submitted. `take` empties the buffer before anything else can read it, whether
/// or not the input is usable, and the owner clears it when the screen goes away or the app leaves the foreground. The
/// secret is never trimmed or otherwise changed: input that would need changing is refused.
@MainActor
final class ProviderSecretBuffer: ObservableObject {
    @Published var text = ""
    /// What the entry's button does. The entry sets it, so a hosted test can press it without a touch.
    var press: (() -> Void)?

    enum Submission: Equatable {
        case secret(Data)
        case rejected(ProviderSecretRejection)
    }

    func take() -> Submission {
        let typed = text
        text = ""
        if typed.isEmpty { return .rejected(.empty) }
        if typed.trimmingCharacters(in: .whitespacesAndNewlines) != typed { return .rejected(.surroundingWhitespace) }
        return .secret(Data(typed.utf8))
    }

    func clear() { text = "" }
}

/// A secure field with a button that hands the secret straight to `onSecret` and empties itself. The text lives only in
/// the buffer; it is never rendered back, reported or kept anywhere else.
private struct ProviderSecretEntry: View {
    let fieldIdentifier: String
    let buttonTitle: String
    let buttonIdentifier: String
    let onSecret: (Data) -> Void
    let onRejected: (ProviderSecretRejection) -> Void
    let bufferProbe: ((String, ProviderSecretBuffer) -> Void)?
    @StateObject private var buffer = ProviderSecretBuffer()
    @Environment(\.scenePhase) private var scenePhase

    private func submit() {
        switch buffer.take() {
        case .secret(let secret): onSecret(secret)
        case .rejected(let rejection): onRejected(rejection)
        }
    }

    var body: some View {
        SecureField("Secret (API key)", text: $buffer.text)
            .textInputAutocapitalization(.never).autocorrectionDisabled().textContentType(.none)
            .submitLabel(.done)
            .onSubmit(submit)
            .providerFieldStyle()
            .accessibilityIdentifier(fieldIdentifier)
            .onAppear {
                buffer.press = submit
                bufferProbe?(fieldIdentifier, buffer)
            }
            .onDisappear { buffer.clear() }
            .onChange(of: scenePhase) { phase in
                if phase != .active { buffer.clear() }
            }
            .onReceive(NotificationCenter.default.publisher(for: UIApplication.willResignActiveNotification)) { _ in
                buffer.clear()
            }
        Button(buttonTitle, action: submit)
            .buttonStyle(DSPillButtonStyle(style: .secondary))
            .reportRendered(buttonIdentifier, text: buttonTitle)
            .accessibilityIdentifier(buttonIdentifier)
    }
}

extension View {
    /// An input field sitting inside a card: `surface2` at the field radius.
    fileprivate func providerFieldStyle() -> some View {
        self.font(DSFont.rowLabel)
            .foregroundColor(DSColor.ink.color)
            .padding(.horizontal, DSSpacing.sm)
            .frame(minHeight: DSLayout.minimumTouchTarget)
            .background(RoundedRectangle(cornerRadius: DSRadius.field, style: .continuous).fill(DSColor.surface2.color))
    }
}


/// The three ways to sort captures. The choice follows the stored default: none is "on this phone", a private server
/// is "my own server" and any other provider is "a cloud provider".
enum ProviderProcessingChoice: String, CaseIterable, Identifiable {
    case onThisPhone = "local"
    case cloudProvider = "cloud"
    case ownServer = "server"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .onThisPhone: return "On this phone"
        case .cloudProvider: return "With a cloud provider"
        case .ownServer: return "With my own server"
        }
    }

    var detail: String {
        switch self {
        case .onThisPhone: return "Nothing leaves the device"
        case .cloudProvider: return "Sent to a provider you set up"
        case .ownServer: return "Sent only to the server you name"
        }
    }

    static let needsProviderDetail = "Add a provider first"
    static let needsCheckDetail = "Run its connection check in Advanced"
}

extension ProviderSettingsModel {
    /// Whether the current operating mode is `choice`.
    func isSelected(_ choice: ProviderProcessingChoice) -> Bool {
        switch (operatingMode, choice) {
        case (.localOnly, .onThisPhone), (.hosted, .cloudProvider), (.privateServer, .ownServer): return true
        default: return false
        }
    }

    /// The newest version of the first stored profile, in listing order, that has a version choosing `choice` would
    /// make the default, if there is one.
    func candidateVersion(for choice: ProviderProcessingChoice) -> ProviderProfileListAnswer.Version? {
        guard choice != .onThisPhone else { return nil }
        let wantsPrivateServer = choice == .ownServer
        return listing?.profiles.flatMap { $0.versions.reversed() }.first {
            ($0.providerProtocol == "self_hosted") == wantsPrivateServer && canMakeDefault($0)
        }
    }

    /// Whether any version of the kind `choice` names is stored, usable or not.
    func hasStoredVersion(for choice: ProviderProcessingChoice) -> Bool {
        guard choice != .onThisPhone else { return false }
        let wantsPrivateServer = choice == .ownServer
        return listing?.profiles.contains { profile in
            profile.versions.contains { ($0.providerProtocol == "self_hosted") == wantsPrivateServer }
        } ?? false
    }

    /// What choosing `choice` does: nothing when it is already in use, otherwise it returns to this phone or makes the
    /// candidate version the default.
    func select(_ choice: ProviderProcessingChoice) async {
        guard !isSelected(choice) else { return }
        if choice == .onThisPhone {
            await useLocalOnly()
        } else if let target = candidateVersion(for: choice) {
            await makeDefault(target)
        }
    }

    /// Drops the draft and deletes a secret stored for it that no saved version names, when the screen that held the
    /// form is left without saving.
    func abandonDraft() {
        draft = ProviderSettingsDraft()
        credentialState = nil
    }
}

/// What a screen shows for each phase of the model, so the three screens answer a lock, a failure and a load the same
/// way. `loaded` is built only once the stored state has been read.
private struct ProviderPhaseContent<Loaded: View>: View {
    @ObservedObject var model: ProviderSettingsModel
    @ViewBuilder let loaded: () -> Loaded

    var body: some View {
        switch model.phase {
        case .idle:
            EmptyView()
        case .loading:
            ProgressView("Loading").frame(maxWidth: .infinity)
        case .locked:
            providerNote(ProviderSettingsText.lockedText, identifier: ProviderSettingsView.lockedIdentifier)
        case .failed(let message):
            providerNote(message, identifier: ProviderSettingsView.failureIdentifier)
            DSPillButton(title: "Try again", style: .secondary) { Task { await model.refresh() } }
                .accessibilityIdentifier(ProviderSettingsView.tryAgainIdentifier)
        case .loaded:
            loaded()
        }
    }
}

/// A line of muted explanation or feedback under a section.
private func providerNote(_ text: String, identifier: String) -> some View {
    Text(text)
        .font(DSFont.footer)
        .foregroundColor(DSColor.inkMuted.color)
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, DSSpacing.md)
        .reportRendered(identifier, text: text)
        .accessibilityIdentifier(identifier)
}

/// The Processing screen: how new captures are sorted, with the providers and the finer controls one level down.
/// Everything it shows comes from `ProviderSettingsModel`.
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
    static let addProviderIdentifier = "provider-settings.add-provider"
    static let advancedIdentifier = "provider-settings.advanced"
    static let timeoutIdentifier = "provider-settings.field.timeout"
    static func choiceIdentifier(_ choice: ProviderProcessingChoice) -> String { "provider-settings.choice.\(choice.rawValue)" }
    static func savedSecretFieldIdentifier(_ version: String) -> String { "provider-settings.saved-secret.field.\(version)" }
    static func replaceSavedSecretIdentifier(_ version: String) -> String { "provider-settings.saved-secret.replace.\(version)" }
    static func checkSavedSecretIdentifier(_ version: String) -> String { "provider-settings.saved-secret.check.\(version)" }
    static func removeSavedSecretIdentifier(_ version: String) -> String { "provider-settings.saved-secret.remove.\(version)" }
    static func savedSecretStateIdentifier(_ version: String) -> String { "provider-settings.saved-secret.state.\(version)" }
    static func checkConnectionIdentifier(_ version: String) -> String { "provider-settings.check-connection.\(version)" }
    static func connectionStateIdentifier(_ version: String) -> String { "provider-settings.connection-state.\(version)" }

    static func versionIdentifier(_ version: String) -> String { "provider-settings.version.\(version)" }
    static func makeDefaultIdentifier(_ version: String) -> String { "provider-settings.make-default.\(version)" }
    static func capabilityIdentifier(_ version: String, _ capability: String) -> String {
        "provider-settings.capability.\(version).\(capability)"
    }

    static let footerText = "Applies to new captures. What you saved is never changed."

    @ObservedObject var model: ProviderSettingsModel
    var renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)?
    /// Lets a hosted test see each secret field's buffer. It is called with the field identifier and the buffer.
    var secretBufferProbe: ((String, ProviderSecretBuffer) -> Void)?
    /// Whether a screen one level down is open. Opening one hides this screen, which must not drop the stored state.
    @State private var showsAddProvider = false
    @State private var showsAdvanced = false

    init(
        model: ProviderSettingsModel,
        renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)? = nil,
        secretBufferProbe: ((String, ProviderSecretBuffer) -> Void)? = nil
    ) {
        self.model = model
        self.renderReporter = renderReporter
        self.secretBufferProbe = secretBufferProbe
    }

    /// What the choice is showing, for the info sheet and the summary row one level down.
    static func modeText(_ mode: ProviderSettingsOperatingMode) -> String {
        switch mode {
        case .localOnly: return ProviderSettingsText.localOnlyNotice
        case .privateServer(let model): return "Private server, model \(model). " + ProviderSettingsText.privateServerNotice
        case .hosted(let name, let model): return "\(name), model \(model). " + ProviderSettingsText.hostedNotice
        }
    }

    /// The short form of the current mode, as a row value.
    static func modeSummary(_ mode: ProviderSettingsOperatingMode) -> String {
        switch mode {
        case .localOnly: return ProviderProcessingChoice.onThisPhone.label
        case .privateServer(let model): return "Private server · \(model)"
        case .hosted(let name, let model): return "\(name) · \(model)"
        }
    }

    var body: some View {
        SettingsScreenChrome(
            title: "Processing", infoTitle: "Processing",
            infoParagraphs: [Self.modeText(model.operatingMode), ProviderSettingsText.defaultChangeNote]
        ) {
            ProviderPhaseContent(model: model) { loaded }
        }
        .task { await model.refresh() }
        .onPreferenceChange(ProviderSettingsRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onDisappear { if !showsAddProvider && !showsAdvanced { model.clear() } }
        // Backing out of a screen one level down abandons what was typed there, as leaving the form always has.
        .onChange(of: showsAddProvider) { shown in if !shown { model.abandonDraft() } }
        .onChange(of: showsAdvanced) { shown in if !shown { model.abandonDraft() } }
    }

    @ViewBuilder
    private var loaded: some View {
        DSGroupedSection(title: "Sort my captures", footer: Self.footerText) {
            ForEach(ProviderProcessingChoice.allCases) { choice in
                choiceRow(choice)
            }
        }
        if let availability = model.defaultAvailability, availability != .ready {
            providerNote(ProviderSettingsText.unavailableDefaultNotice, identifier: ProviderSettingsView.defaultUnavailableIdentifier)
        }
        if let message = model.defaultState?.message {
            providerNote(message, identifier: Self.defaultStateIdentifier)
        }
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            DSPillButton(title: "Add provider", style: .secondary) { showsAddProvider = true }
                .reportRendered(Self.addProviderIdentifier, text: "Add provider")
                .accessibilityIdentifier(Self.addProviderIdentifier)
            DSPillButton(title: "Advanced", style: .plain) { showsAdvanced = true }
                .reportRendered(Self.advancedIdentifier, text: "Advanced")
                .accessibilityIdentifier(Self.advancedIdentifier)
        }
        .background(
            Group {
                NavigationLink(isActive: $showsAddProvider) {
                    ProviderAddView(model: model, renderReporter: renderReporter, secretBufferProbe: secretBufferProbe)
                } label: { EmptyView() }
                NavigationLink(isActive: $showsAdvanced) {
                    ProviderAdvancedView(model: model, renderReporter: renderReporter, secretBufferProbe: secretBufferProbe)
                } label: { EmptyView() }
            }
            .hidden()
            .accessibilityHidden(true))
    }

    private func choiceRow(_ choice: ProviderProcessingChoice) -> some View {
        let isSelected = model.isSelected(choice)
        let target = model.candidateVersion(for: choice)
        let isAvailable = choice == .onThisPhone || isSelected || target != nil
        let unavailableDetail = model.hasStoredVersion(for: choice)
            ? ProviderProcessingChoice.needsCheckDetail : ProviderProcessingChoice.needsProviderDetail
        return DSChoiceRow(
            label: choice.label, detail: isAvailable ? choice.detail : unavailableDetail, isSelected: isSelected
        ) {
            Task { await model.select(choice) }
        }
        .reportRendered(Self.choiceIdentifier(choice), text: choice.label, isSelected: isSelected)
        .disabled(model.isChangingDefault || !isAvailable)
        .accessibilityIdentifier(Self.choiceIdentifier(choice))
    }
}

/// Adding a provider: its type, address, model and secret, then Save. Timeout and the saved-secret controls are
/// one level further down, on the Advanced screen.
struct ProviderAddView: View {
    @ObservedObject var model: ProviderSettingsModel
    var renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)?
    var secretBufferProbe: ((String, ProviderSecretBuffer) -> Void)?

    var body: some View {
        SettingsScreenChrome(
            title: "Add provider", infoTitle: "Add provider",
            infoParagraphs: [ProviderSettingsText.credentialNote, ProviderSettingsText.capabilityNote]
        ) {
            ProviderPhaseContent(model: model) {
                ProviderProfileForm(model: model, showsAdvancedFields: false, secretBufferProbe: secretBufferProbe)
            }
        }
        .task { if model.phase == .idle { await model.refresh() } }
        .onPreferenceChange(ProviderSettingsRenderedKey.self) { renderReporter?($0) }
    }
}

/// The finer controls: the current provider, every stored profile version with its connection check, secret and default
/// controls, and the full form for a new version including the timeout.
struct ProviderAdvancedView: View {
    @ObservedObject var model: ProviderSettingsModel
    var renderReporter: (([String: ProviderSettingsRenderedElement]) -> Void)?
    var secretBufferProbe: ((String, ProviderSecretBuffer) -> Void)?

    var body: some View {
        SettingsScreenChrome(
            title: "Advanced", infoTitle: "Advanced",
            infoParagraphs: [
                ProviderSettingsText.defaultChangeNote, ProviderSettingsText.credentialNote,
                ProviderSettingsText.connectionCheckNote, ProviderSettingsText.capabilityNote,
            ]
        ) {
            ProviderPhaseContent(model: model) {
                currentProviderSection
                savedProfilesSection
                ProviderProfileForm(model: model, showsAdvancedFields: true, secretBufferProbe: secretBufferProbe)
            }
        }
        .task { if model.phase == .idle { await model.refresh() } }
        .onPreferenceChange(ProviderSettingsRenderedKey.self) { renderReporter?($0) }
    }

    private var currentProviderSection: some View {
        let summary = ProviderSettingsView.modeSummary(model.operatingMode)
        return VStack(alignment: .leading, spacing: DSSpacing.xs) {
            DSGroupedSection(title: "Current provider") {
                DSRow(label: "In use", value: summary, showsChevron: false)
                    .reportRendered(ProviderSettingsView.modeIdentifier, text: summary)
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier(ProviderSettingsView.modeIdentifier)
            }
            if model.defaultVersion != nil {
                DSPillButton(title: "Use this device only", style: .secondary) { Task { await model.useLocalOnly() } }
                    .reportRendered(ProviderSettingsView.localOnlyIdentifier, text: "Use this device only")
                    .disabled(model.isChangingDefault)
                    .accessibilityIdentifier(ProviderSettingsView.localOnlyIdentifier)
            }
            if let availability = model.defaultAvailability, availability != .ready {
                providerNote(
                    ProviderSettingsText.unavailableDefaultNotice,
                    identifier: ProviderSettingsView.defaultUnavailableIdentifier)
            }
            if let message = model.defaultState?.message {
                providerNote(message, identifier: ProviderSettingsView.defaultStateIdentifier)
            }
        }
    }

    @ViewBuilder
    private var savedProfilesSection: some View {
        if let profiles = model.listing?.profiles, !profiles.isEmpty {
            ForEach(profiles, id: \.profileID) { profile in
                ForEach(profile.versions.reversed(), id: \.profileVersion) { version in
                    DSGroupedSection(title: profile.profileID) { versionRows(version) }
                }
            }
        } else {
            DSGroupedSection(title: "Saved profiles") {
                Text("No profiles saved yet.")
                    .font(DSFont.rowLabel)
                    .foregroundColor(DSColor.inkMuted.color)
                    .padding(DSSpacing.md)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .reportRendered("provider-settings.none", text: "No profiles saved yet.")
                    .accessibilityIdentifier("provider-settings.none")
            }
        }
    }

    private func versionRows(_ version: ProviderProfileListAnswer.Version) -> some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("\(ProviderSettingsText.protocolName(version.providerProtocol)) · \(version.model)")
                .font(DSFont.rowLabel).foregroundColor(DSColor.ink.color)
            if let endpoint = version.endpoint {
                Text(endpoint).font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
            }
            ForEach(version.capabilities, id: \.capability) { capability in
                let status = ProviderSettingsText.capabilityStatus(capability.capability, support: capability.support)
                Text(status)
                    .font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .reportRendered(ProviderSettingsView.capabilityIdentifier(version.profileVersion, capability.capability), text: status)
            }
            if version.revoked {
                Text("Revoked").font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
            } else if !version.defaultFor.isEmpty {
                Text("Current default").font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
            }
            if model.canCheckConnection(version) {
                DSPillButton(title: "Check connection", style: .secondary) { Task { await model.checkConnection(version) } }
                    .reportRendered(ProviderSettingsView.checkConnectionIdentifier(version.profileVersion), text: "Check connection")
                    .accessibilityIdentifier(ProviderSettingsView.checkConnectionIdentifier(version.profileVersion))
            }
            if let state = model.connectionStates[version.profileVersion] {
                versionNote(state.message, identifier: ProviderSettingsView.connectionStateIdentifier(version.profileVersion))
            }
            if !version.revoked { savedSecretControls(version) }
            if model.canMakeDefault(version) {
                DSPillButton(title: "Make default", style: .secondary) { Task { await model.makeDefault(version) } }
                    .reportRendered(ProviderSettingsView.makeDefaultIdentifier(version.profileVersion), text: "Make default")
                    .accessibilityIdentifier(ProviderSettingsView.makeDefaultIdentifier(version.profileVersion))
            }
        }
        .padding(DSSpacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(ProviderSettingsView.versionIdentifier(version.profileVersion))
    }

    /// Feedback inside a version's card: it already has the card's padding.
    private func versionNote(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .reportRendered(identifier, text: text)
            .accessibilityIdentifier(identifier)
    }

    /// Replace, check or remove the secret of a stored version. The services layer finds the version's own secret; the
    /// screen never learns its name.
    @ViewBuilder
    private func savedSecretControls(_ version: ProviderProfileListAnswer.Version) -> some View {
        let profileVersion = version.profileVersion
        ProviderSecretEntry(
            fieldIdentifier: ProviderSettingsView.savedSecretFieldIdentifier(profileVersion), buttonTitle: "Replace secret",
            buttonIdentifier: ProviderSettingsView.replaceSavedSecretIdentifier(profileVersion),
            onSecret: { secret in Task { await model.replaceSavedCredential(secret, for: version) } },
            onRejected: { model.noteRejectedSecret($0, for: version) }, bufferProbe: secretBufferProbe
        )
        // A second tap while the operation runs would empty the new buffer and have the model drop the secret.
        .disabled(model.isChangingSavedCredential(version))
        HStack {
            DSPillButton(title: "Check secret", style: .plain) { Task { await model.checkSavedCredential(version) } }
                .reportRendered(ProviderSettingsView.checkSavedSecretIdentifier(profileVersion), text: "Check secret")
                .accessibilityIdentifier(ProviderSettingsView.checkSavedSecretIdentifier(profileVersion))
            DSPillButton(title: "Remove secret", style: .destructive) { Task { await model.removeSavedCredential(version) } }
                .reportRendered(ProviderSettingsView.removeSavedSecretIdentifier(profileVersion), text: "Remove secret")
                .accessibilityIdentifier(ProviderSettingsView.removeSavedSecretIdentifier(profileVersion))
        }
        .disabled(model.isChangingSavedCredential(version))
        if let state = model.savedCredentialStates[profileVersion] {
            versionNote(state.message, identifier: ProviderSettingsView.savedSecretStateIdentifier(profileVersion))
        }
    }
}

/// The form for a new profile version. The Add provider screen shows the short form; the Advanced screen adds the
/// credential name, the saved-secret check and removal, and the timeout.
private struct ProviderProfileForm: View {
    @ObservedObject var model: ProviderSettingsModel
    let showsAdvancedFields: Bool
    var secretBufferProbe: ((String, ProviderSecretBuffer) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            DSGroupedSection(title: showsAdvancedFields ? "New profile version" : nil) {
                VStack(alignment: .leading, spacing: DSSpacing.xs) { fields }
                    .padding(DSSpacing.md)
            }
            if !model.issues.isEmpty {
                providerNote(
                    model.issues.map(ProviderSettingsText.issue).joined(separator: " "),
                    identifier: ProviderSettingsView.issuesIdentifier)
            }
            DSPillButton(title: "Save profile", style: .primary) { Task { await model.save() } }
                .reportRendered(ProviderSettingsView.saveIdentifier, text: "Save profile")
                .disabled(!model.canSave)
                .accessibilityIdentifier(ProviderSettingsView.saveIdentifier)
            if let message = model.saveState?.message {
                providerNote(message, identifier: ProviderSettingsView.saveStateIdentifier)
            }
        }
    }

    @ViewBuilder
    private var fields: some View {
        Picker("Provider type", selection: $model.draft.providerProtocol) {
            ForEach(ProviderSettingsProtocol.allCases, id: \.self) { choice in
                Text(ProviderSettingsText.protocolName(choice.rawValue)).tag(choice)
            }
        }
        .pickerStyle(.menu)
        .tint(DSColor.accent.color)
        .accessibilityIdentifier("provider-settings.field.protocol")
        TextField("Profile name", text: $model.draft.profileName)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .providerFieldStyle()
            .reportRendered(ProviderSettingsView.nameFieldIdentifier, text: model.draft.profileName)
            .accessibilityIdentifier(ProviderSettingsView.nameFieldIdentifier)
        if model.draft.providerProtocol.takesEndpoint {
            TextField("https://server address", text: $model.draft.endpoint)
                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                .providerFieldStyle()
                .reportRendered(ProviderSettingsView.endpointFieldIdentifier, text: model.draft.endpoint)
                .accessibilityIdentifier(ProviderSettingsView.endpointFieldIdentifier)
        }
        TextField("Model", text: $model.draft.model)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .providerFieldStyle()
            .reportRendered(ProviderSettingsView.modelFieldIdentifier, text: model.draft.model)
            .accessibilityIdentifier(ProviderSettingsView.modelFieldIdentifier)
        if showsAdvancedFields {
            Text(model.draft.credentialReference.isEmpty
                ? "Credential name: filled in when you store a secret" : "Credential name: \(model.draft.credentialReference)")
                .font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
                .reportRendered(ProviderSettingsView.credentialFieldIdentifier, text: model.draft.credentialReference)
                .accessibilityIdentifier(ProviderSettingsView.credentialFieldIdentifier)
        }
        ProviderSecretEntry(
            fieldIdentifier: ProviderSettingsView.secretFieldIdentifier, buttonTitle: "Store secret",
            buttonIdentifier: ProviderSettingsView.storeSecretIdentifier,
            onSecret: { model.storeCredential($0) }, onRejected: { model.noteRejectedSecret($0) },
            bufferProbe: secretBufferProbe)
        if showsAdvancedFields {
            HStack {
                DSPillButton(title: "Check saved", style: .plain) { model.checkCredential() }
                    .reportRendered(ProviderSettingsView.checkSecretIdentifier, text: "Check saved")
                    .accessibilityIdentifier(ProviderSettingsView.checkSecretIdentifier)
                DSPillButton(title: "Remove", style: .destructive) { model.removeCredential() }
                    .reportRendered(ProviderSettingsView.removeSecretIdentifier, text: "Remove")
                    .accessibilityIdentifier(ProviderSettingsView.removeSecretIdentifier)
            }
        }
        if let state = model.credentialState {
            Text(state.message)
                .font(DSFont.footer).foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .reportRendered(ProviderSettingsView.credentialStateIdentifier, text: state.message)
                .accessibilityIdentifier(ProviderSettingsView.credentialStateIdentifier)
        }
        if showsAdvancedFields {
            Stepper(
                value: $model.draft.timeoutSeconds,
                in: ProviderSettingsModel.minimumTimeoutSeconds...ProviderSettingsModel.maximumTimeoutSeconds
            ) {
                Text("Timeout \(model.draft.timeoutSeconds) seconds")
                    .font(DSFont.rowLabel).foregroundColor(DSColor.ink.color)
            }
            .accessibilityIdentifier(ProviderSettingsView.timeoutIdentifier)
        }
    }
}

extension ProviderSettingsModel {
    /// The model as the app assembles it: the root's provider configuration, credentials and credential sweep, gated by
    /// the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> ProviderSettingsModel {
        ProviderSettingsModel(
            controls: root.providerConfiguration, credentials: root.providerCredentials,
            sweep: root.providerCredentialSweep, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
