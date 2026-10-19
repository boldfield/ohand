import SwiftUI
import OhAndCoreBridge
import OhAndServices

private func deletionNote(_ text: String, identifier: String) -> some View {
    Text(text)
        .font(DSFont.footer)
        .foregroundColor(DSColor.inkMuted.color)
        .fixedSize(horizontal: false, vertical: true)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier(identifier)
        .reportDataSettingsRendered(identifier, text: text)
}

/// The per-item delete control placed in the source-detail screen. Everything it shows comes from `ItemDeletionModel`.
struct ItemDeletionSection: View {
    static let deleteIdentifier = "data-settings.delete-item.delete"
    static let explanationIdentifier = "data-settings.delete-item.explanation"
    static let limitsIdentifier = "data-settings.delete-item.limits"
    static let confirmIdentifier = "data-settings.delete-item.confirm"
    static let cancelIdentifier = "data-settings.delete-item.cancel"
    static let retryIdentifier = "data-settings.delete-item.retry"
    static let checkProgressIdentifier = "data-settings.delete-item.check-progress"
    static let stateIdentifier = "data-settings.delete-item.state"

    @ObservedObject var model: ItemDeletionModel
    /// Told as soon as the deletion has been recorded, so the host can read the item again and drop what it showed.
    var onRecorded: () -> Void = {}
    var renderReporter: (([String: RetentionRenderedElement]) -> Void)?

    private var offersDeletion: Bool { model.revision != nil }

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.sm) {
            content
        }
        .onPreferenceChange(RetentionRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onReceive(model.$isRecorded.filter { $0 }) { _ in onRecorded() }
        .task {
            await model.prepare()
            await model.refreshProgress()
        }
        .onDisappear { model.clear() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.phase {
        case .idle:
            if offersDeletion { deleteButton }
        case .confirming:
            deletionNote(DeletionText.itemExplanation, identifier: Self.explanationIdentifier)
            deletionNote(DeletionText.itemLimitsNote, identifier: Self.limitsIdentifier)
            DSPillButton(title: "Delete permanently", style: .destructive) {
                Task { await model.confirmDeletion() }
            }
            .accessibilityIdentifier(Self.confirmIdentifier)
            .reportDataSettingsRendered(Self.confirmIdentifier, text: "Delete permanently")
            DSPillButton(title: "Cancel", style: .secondary) { model.cancelConfirmation() }
                .accessibilityIdentifier(Self.cancelIdentifier)
                .reportDataSettingsRendered(Self.cancelIdentifier, text: "Cancel")
        case .deleting:
            ProgressView(DeletionText.deletingText).frame(maxWidth: .infinity)
                .reportDataSettingsRendered(Self.stateIdentifier, text: DeletionText.deletingText)
        case .pending(let message):
            deletionNote(message, identifier: Self.stateIdentifier)
            if model.canRetry {
                DSPillButton(title: "Retry cleanup", style: .secondary) { Task { await model.retry() } }
                    .accessibilityIdentifier(Self.retryIdentifier)
                    .reportDataSettingsRendered(Self.retryIdentifier, text: "Retry cleanup")
            }
            if model.canCheckProgress {
                DSPillButton(title: "Check progress", style: .secondary) { Task { await model.refreshProgress() } }
                    .accessibilityIdentifier(Self.checkProgressIdentifier)
                    .reportDataSettingsRendered(Self.checkProgressIdentifier, text: "Check progress")
            }
        case .complete:
            deletionNote(DeletionText.completeText, identifier: Self.stateIdentifier)
        case .failed(let message):
            deletionNote(message, identifier: Self.stateIdentifier)
            if offersDeletion { deleteButton }
        case .locked:
            deletionNote(DeletionText.lockedText, identifier: Self.stateIdentifier)
        }
    }

    private var deleteButton: some View {
        DSPillButton(title: DeletionText.itemHeading, style: .destructive) { model.requestDeletion() }
            .accessibilityIdentifier(Self.deleteIdentifier)
            .reportDataSettingsRendered(Self.deleteIdentifier, text: DeletionText.itemHeading)
    }
}

extension ItemDeletionModel {
    /// The model as the app assembles it: the root's deletion service, gated by the root's read scope.
    nonisolated static func assembled(itemID: String, root: ServiceCompositionRoot) -> ItemDeletionModel {
        ItemDeletionModel(
            itemID: itemID, controls: root.itemDeletion, reader: root.itemDetails, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}

/// The delete-all section of the data settings: the credential choice, the limits of a reset and how it is going.
/// Everything it shows comes from `ResetSettingsModel`.
struct ResetSettingsView: View {
    static let explanationIdentifier = "data-settings.reset.explanation"
    static let limitsIdentifier = "data-settings.reset.limits"
    static let captureNoteIdentifier = "data-settings.reset.capture-note"
    static let keepIdentifier = "data-settings.reset.credentials.keep"
    static let removeIdentifier = "data-settings.reset.credentials.remove"
    static let choiceRequiredIdentifier = "data-settings.reset.choice-required"
    static let continueIdentifier = "data-settings.reset.continue"
    static let confirmIdentifier = "data-settings.reset.confirm"
    static let cancelIdentifier = "data-settings.reset.cancel"
    static let retryIdentifier = "data-settings.reset.retry"
    static let stateIdentifier = "data-settings.reset.state"
    static let lockedIdentifier = "data-settings.reset.locked"

    @ObservedObject var model: ResetSettingsModel
    var renderReporter: (([String: RetentionRenderedElement]) -> Void)?

    init(model: ResetSettingsModel, renderReporter: (([String: RetentionRenderedElement]) -> Void)? = nil) {
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
        .navigationTitle(DeletionText.resetHeading)
        .navigationBarTitleDisplayMode(.inline)
        .onPreferenceChange(RetentionRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .task { await model.refresh() }
        .onDisappear { model.clear() }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase == .locked {
            deletionNote(DeletionText.lockedText, identifier: Self.lockedIdentifier)
        } else {
            deletionNote(DeletionText.resetExplanation, identifier: Self.explanationIdentifier)
            deletionNote(DeletionText.resetLimitsNote, identifier: Self.limitsIdentifier)
            deletionNote(DeletionText.resetCaptureNote, identifier: Self.captureNoteIdentifier)
            Text(DeletionText.credentialQuestion).font(.headline)
            choice(.keep, title: DeletionText.keepCredentialsTitle, identifier: Self.keepIdentifier)
            choice(.remove, title: DeletionText.removeCredentialsTitle, identifier: Self.removeIdentifier)
            if model.credentialChoice == nil {
                deletionNote(DeletionText.choiceRequiredNote, identifier: Self.choiceRequiredIdentifier)
            }
            actions
        }
    }

    private func choice(_ value: ResetCredentialChoice, title: String, identifier: String) -> some View {
        Button {
            model.credentialChoice = value
        } label: {
            HStack {
                Image(systemName: model.credentialChoice == value ? "largecircle.fill.circle" : "circle")
                Text(title).frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 44)
        }
        .buttonStyle(.plain)
        .disabled(model.isBusy || model.phase == .confirming)
        .accessibilityAddTraits(model.credentialChoice == value ? .isSelected : [])
        .accessibilityIdentifier(identifier)
        .reportDataSettingsRendered(identifier, text: title)
    }

    @ViewBuilder
    private var actions: some View {
        switch model.phase {
        case .idle, .failed:
            if case .failed(let message) = model.phase {
                deletionNote(message, identifier: Self.stateIdentifier)
            }
            DSPillButton(title: DeletionText.resetHeading, style: .destructive) { model.beginConfirmation() }
                .disabled(!model.canContinue)
                .accessibilityIdentifier(Self.continueIdentifier)
                .reportDataSettingsRendered(Self.continueIdentifier, text: DeletionText.resetHeading)
        case .confirming:
            DSPillButton(title: "Delete everything permanently", style: .destructive) {
                Task { await model.confirmReset() }
            }
            .accessibilityIdentifier(Self.confirmIdentifier)
            .reportDataSettingsRendered(Self.confirmIdentifier, text: "Delete everything permanently")
            DSPillButton(title: "Cancel", style: .secondary) { model.cancelConfirmation() }
                .accessibilityIdentifier(Self.cancelIdentifier)
                .reportDataSettingsRendered(Self.cancelIdentifier, text: "Cancel")
        case .resetting:
            ProgressView(DeletionText.resettingText).frame(maxWidth: .infinity)
                .reportDataSettingsRendered(Self.stateIdentifier, text: DeletionText.resettingText)
        case .pending(let message):
            deletionNote(message, identifier: Self.stateIdentifier)
            DSPillButton(title: "Try again", style: .secondary) { Task { await model.retry() } }
                .disabled(!model.canRetry)
                .accessibilityIdentifier(Self.retryIdentifier)
                .reportDataSettingsRendered(Self.retryIdentifier, text: "Try again")
        case .completed:
            deletionNote(DeletionText.completedText, identifier: Self.stateIdentifier)
        case .locked:
            EmptyView()
        }
    }
}

extension ResetSettingsModel {
    /// The model as the app assembles it: the root's reset control, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> ResetSettingsModel {
        ResetSettingsModel(
            controls: root.resetControl, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
