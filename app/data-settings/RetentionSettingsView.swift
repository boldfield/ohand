import SwiftUI
import OhAndServices

/// Where an element of the retention screen landed and the words it shows, so a hosted test can see what is on screen.
struct RetentionRenderedElement: Equatable {
    let frame: CGRect
    let text: String
    let isEnabled: Bool
}

struct RetentionRenderedKey: PreferenceKey {
    static var defaultValue: [String: RetentionRenderedElement] = [:]

    static func reduce(
        value: inout [String: RetentionRenderedElement], nextValue: () -> [String: RetentionRenderedElement]
    ) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

struct RetentionRenderReport: ViewModifier {
    let identifier: String
    let text: String
    @Environment(\.isEnabled) private var isEnabled

    func body(content: Content) -> some View {
        content.background(GeometryReader { proxy in
            Color.clear.preference(
                key: RetentionRenderedKey.self,
                value: [identifier: RetentionRenderedElement(
                    frame: proxy.frame(in: .global), text: text, isEnabled: isEnabled)])
        })
    }
}

extension View {
    func reportDataSettingsRendered(_ identifier: String, text: String) -> some View {
        modifier(RetentionRenderReport(identifier: identifier, text: text))
    }
}

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
    static let loadingIdentifier = "data-settings.retention.loading"
    static let tryAgainIdentifier = "data-settings.retention.try-again"
    static let exportLinkIdentifier = "data-settings.retention.export-link"

    static func caseIdentifier(_ itemID: String) -> String { "data-settings.retention.case.\(itemID)" }
    static func retryIdentifier(_ itemID: String) -> String { "data-settings.retention.retry.\(itemID)" }
    static func deleteNoteIdentifier(_ itemID: String) -> String { "data-settings.retention.delete-note.\(itemID)" }
    static func actionStateIdentifier(_ itemID: String) -> String { "data-settings.retention.action-state.\(itemID)" }

    @ObservedObject var model: RetentionSettingsModel
    var renderReporter: (([String: RetentionRenderedElement]) -> Void)?

    init(model: RetentionSettingsModel, renderReporter: (([String: RetentionRenderedElement]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
                if let exportSettings = model.exportSettings {
                    NavigationLink("Export a copy of your notes") { ExportSettingsView(model: exportSettings, renderReporter: renderReporter) }
                        .frame(minHeight: 44)
                        .accessibilityIdentifier(Self.exportLinkIdentifier)
                        .reportDataSettingsRendered(Self.exportLinkIdentifier, text: "Export a copy of your notes")
                }
            }
            .padding()
        }
        .navigationTitle("Audio retention")
        .navigationBarTitleDisplayMode(.inline)
        .onPreferenceChange(RetentionRenderedKey.self) { elements in
            renderReporter?(elements)
        }
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
                .reportDataSettingsRendered(Self.loadingIdentifier, text: "Loading")
        case .locked:
            note(RetentionSettingsText.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let message):
            note(message, identifier: Self.failureIdentifier)
            Button("Try again") { Task { await model.refresh() } }
                .frame(minHeight: 44)
                .accessibilityIdentifier(Self.tryAgainIdentifier)
                .reportDataSettingsRendered(Self.tryAgainIdentifier, text: "Try again")
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
                .reportDataSettingsRendered(Self.policyIdentifier, text: RetentionSettingsText.periodSummary(status.policy))
            Text(RetentionSettingsText.policyExplanation(status.policy))
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Self.explanationIdentifier)
                .reportDataSettingsRendered(Self.explanationIdentifier, text: RetentionSettingsText.policyExplanation(status.policy))
            Text(RetentionSettingsText.soleSourceExplanation)
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Self.soleSourceIdentifier)
                .reportDataSettingsRendered(Self.soleSourceIdentifier, text: RetentionSettingsText.soleSourceExplanation)
            Stepper(
                value: $model.chosenDays, in: RetentionSettingsModel.minimumDays...RetentionSettingsModel.maximumDays
            ) {
                Text(RetentionSettingsText.dayCount(model.chosenDays))
            }
            .accessibilityIdentifier(Self.periodStepperIdentifier)
            .reportDataSettingsRendered(Self.periodStepperIdentifier, text: RetentionSettingsText.dayCount(model.chosenDays))
            Text(RetentionSettingsText.periodChangeNote)
                .font(.footnote).foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                Task { await model.applyPeriod() }
            } label: {
                Text("Save period").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .reportDataSettingsRendered(Self.applyIdentifier, text: "Save period")
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
                .reportDataSettingsRendered(Self.retryIdentifier(entry.itemID), text: "Retry turning into text")
                .disabled(busy)
                .accessibilityIdentifier(Self.retryIdentifier(entry.itemID))
            }
            // Only sole-source audio is listed here, and the core refuses to delete such audio on its own, so no
            // audio-only delete is offered. Item deletion is the explicit destructive path.
            if entry.options.contains(.deleteAudio) {
                note(RetentionSettingsText.deleteItemNote, identifier: Self.deleteNoteIdentifier(entry.itemID))
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
        .reportDataSettingsRendered(Self.caseIdentifier(entry.itemID), text: RetentionSettingsText.reason(entry.reason))
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(identifier)
            .reportDataSettingsRendered(identifier, text: text)
    }
}

extension RetentionSettingsModel {
    /// The model as the app assembles it: the root's retention controls, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> RetentionSettingsModel {
        RetentionSettingsModel(
            controls: root.retentionControls, readScope: { [weak root] in root?.readScope ?? .captureOnly },
            exportSettings: .assembled(root: root))
    }
}
