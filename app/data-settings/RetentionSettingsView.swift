import SwiftUI
import OhAndServices

/// The retention screen: the stored period, the storage-pressure notice and the recordings that are an item's only
/// source, each with the choices the core offers. Everything it shows comes from `RetentionSettingsModel`.
struct RetentionSettingsView: View {
    static let policyIdentifier = "data-settings.retention.policy"
    static let explanationIdentifier = "data-settings.retention.explanation"
    static let soleSourceIdentifier = "data-settings.retention.sole-source"
    static let pressureIdentifier = "data-settings.retention.pressure"
    static let periodStepperIdentifier = "data-settings.retention.period-stepper"
    static let applyIdentifier = "data-settings.retention.apply"
    static let periodStateIdentifier = "data-settings.retention.period-state"
    static let lockedIdentifier = "data-settings.retention.locked"
    static let failureIdentifier = "data-settings.retention.failure"
    static let noUnresolvedIdentifier = "data-settings.retention.none-unresolved"

    static func caseIdentifier(_ itemID: String) -> String { "data-settings.retention.case.\(itemID)" }
    static func retryIdentifier(_ itemID: String) -> String { "data-settings.retention.retry.\(itemID)" }
    static func deleteIdentifier(_ itemID: String) -> String { "data-settings.retention.delete.\(itemID)" }
    static func actionStateIdentifier(_ itemID: String) -> String { "data-settings.retention.action-state.\(itemID)" }

    @ObservedObject var model: RetentionSettingsModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
            }
            .padding()
        }
        .navigationTitle("Audio retention")
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
            note(RetentionSettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            Button("Try again") { Task { await model.refresh() } }
                .frame(minHeight: 44)
        case .loaded:
            if let status = model.status {
                policySection(status)
                if status.storagePressure == .low {
                    note(RetentionSettingsText.pressureLowNotice, identifier: Self.pressureIdentifier)
                }
                unresolvedSection(status)
            }
        }
    }

    private func policySection(_ status: RetentionStatus) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Keep recordings for").font(.headline)
            Text(RetentionSettingsText.periodSummary(status.policy))
                .accessibilityIdentifier(Self.policyIdentifier)
            Text(RetentionSettingsText.defaultPolicyExplanation)
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Self.explanationIdentifier)
            Text(RetentionSettingsText.soleSourceExplanation)
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Self.soleSourceIdentifier)
            Stepper(
                value: $model.chosenDays, in: RetentionSettingsModel.minimumDays...RetentionSettingsModel.maximumDays
            ) {
                Text(model.chosenDays == 1 ? "1 day" : "\(model.chosenDays) days")
            }
            .accessibilityIdentifier(Self.periodStepperIdentifier)
            Text(RetentionSettingsText.periodChangeNote)
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                Task { await model.applyPeriod() }
            } label: {
                Text("Save period").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.isApplyingPeriod)
            .accessibilityIdentifier(Self.applyIdentifier)
            if let message = model.periodState?.message {
                note(message, identifier: Self.periodStateIdentifier)
            }
        }
    }

    @ViewBuilder
    private func unresolvedSection(_ status: RetentionStatus) -> some View {
        Text("Recordings without text").font(.headline)
        if status.unresolved.isEmpty {
            note(RetentionSettingsText.noUnresolvedText, identifier: Self.noUnresolvedIdentifier)
        }
        ForEach(status.unresolved, id: \.itemID) { entry in
            caseRow(entry)
        }
    }

    private func caseRow(_ entry: UnresolvedSoleAudioCase) -> some View {
        let busy = model.actions[entry.itemID] == .working
        return VStack(alignment: .leading, spacing: 8) {
            Text(RetentionSettingsText.reason(entry.reason))
                .fixedSize(horizontal: false, vertical: true)
            Text("Captured \(entry.capturedAt)").font(.footnote).foregroundColor(.secondary)
            if entry.options.contains(.retryTranscription) {
                Button {
                    Task { await model.retryTranscription(itemID: entry.itemID) }
                } label: {
                    Text("Retry turning into text").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .disabled(busy)
                .accessibilityIdentifier(Self.retryIdentifier(entry.itemID))
            }
            if entry.options.contains(.deleteAudio) {
                Button(role: .destructive) {
                    Task { await model.deleteAudio(itemID: entry.itemID) }
                } label: {
                    Text("Delete audio").frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .disabled(busy)
                .accessibilityIdentifier(Self.deleteIdentifier(entry.itemID))
            }
            if let message = model.actions[entry.itemID]?.message {
                note(message, identifier: Self.actionStateIdentifier(entry.itemID))
            }
        }
        .padding()
        .background(Color(.secondarySystemBackground))
        .cornerRadius(8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.caseIdentifier(entry.itemID))
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

extension RetentionSettingsModel {
    /// The model as the app assembles it: the root's retention controls, gated by the root's read scope.
    static func assembled(root: ServiceCompositionRoot) -> RetentionSettingsModel {
        RetentionSettingsModel(
            controls: root.retentionControls, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
