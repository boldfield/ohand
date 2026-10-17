import SwiftUI
import UIKit
import OhAndCoreBridge
import OhAndServices

/// Presents the system share sheet for a prepared export and reports how it ended.
private struct ExportShareSheetPresenter: UIViewControllerRepresentable {
    let export: PreparedExport
    let onEnding: (ExportShareEnding) -> Void

    func makeUIViewController(context: Context) -> UIActivityViewController {
        ExportShareSheet.makeController(for: export, onEnding: onEnding)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

/// The export section of the data settings: scope and audio choices, the limits of an export, and how the last export
/// ended. Everything it shows comes from `ExportSettingsModel`.
struct ExportSettingsView: View {
    static let explanationIdentifier = "data-settings.export.explanation"
    static let limitsIdentifier = "data-settings.export.limits"
    static let audioNoteIdentifier = "data-settings.export.audio-note"
    static let audioToggleIdentifier = "data-settings.export.audio-toggle"
    static let startIdentifier = "data-settings.export.start"
    static let noScopeIdentifier = "data-settings.export.no-scope"
    static let stateIdentifier = "data-settings.export.state"
    static let lockedIdentifier = "data-settings.export.locked"

    static func scopeIdentifier(_ scope: CoreExportScope) -> String { "data-settings.export.scope.\(scope.rawValue)" }

    @ObservedObject var model: ExportSettingsModel
    var renderReporter: (([String: RetentionRenderedElement]) -> Void)?

    init(model: ExportSettingsModel, renderReporter: (([String: RetentionRenderedElement]) -> Void)? = nil) {
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
        .navigationTitle("Export")
        .navigationBarTitleDisplayMode(.inline)
        .onPreferenceChange(RetentionRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onDisappear { model.clear() }
        .sheet(
            isPresented: Binding(
                get: { model.pendingShare != nil },
                set: { presented in if !presented { model.shareEnded(.cancelled) } })
        ) {
            if let export = model.pendingShare {
                ExportShareSheetPresenter(export: export) { ending in model.shareEnded(ending) }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase == .locked {
            note(ExportSettingsText.lockedText, identifier: Self.lockedIdentifier)
        } else {
            note(ExportSettingsText.explanation, identifier: Self.explanationIdentifier)
            note(ExportSettingsText.limitsNote, identifier: Self.limitsIdentifier)
            Text("Areas to export").font(.headline)
            ForEach(ExportSettingsModel.selectableScopes, id: \.self) { scope in
                Toggle(
                    ExportSettingsText.scopeTitle(scope),
                    isOn: Binding(
                        get: { model.isSelected(scope) }, set: { model.setScope(scope, selected: $0) })
                )
                .frame(minHeight: 44)
                .disabled(isBusy)
                .accessibilityIdentifier(Self.scopeIdentifier(scope))
                .reportDataSettingsRendered(Self.scopeIdentifier(scope), text: ExportSettingsText.scopeTitle(scope))
            }
            Toggle("Include audio recordings", isOn: $model.includeAudio)
                .frame(minHeight: 44)
                .disabled(isBusy)
                .accessibilityIdentifier(Self.audioToggleIdentifier)
                .reportDataSettingsRendered(Self.audioToggleIdentifier, text: "Include audio recordings")
            note(ExportSettingsText.audioNote, identifier: Self.audioNoteIdentifier)
            if model.selectedScopes.isEmpty {
                note(ExportSettingsText.noScopeNote, identifier: Self.noScopeIdentifier)
            }
            Button {
                Task { await model.startExport() }
            } label: {
                Text("Export").frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .reportDataSettingsRendered(Self.startIdentifier, text: "Export")
            .disabled(!model.canStart)
            .accessibilityIdentifier(Self.startIdentifier)
            if let message = stateMessage {
                note(message, identifier: Self.stateIdentifier)
            }
        }
    }

    private var isBusy: Bool { model.phase == .preparing || model.phase == .sharing }

    private var stateMessage: String? {
        switch model.phase {
        case .idle, .locked: return nil
        case .preparing: return ExportSettingsText.preparingText
        case .sharing: return ExportSettingsText.readyText
        case .finished(let outcome):
            switch outcome {
            case .shared(let text), .cancelled(let text), .failed(let text): return text
            }
        }
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

extension ExportSettingsModel {
    /// The model as the app assembles it: the root's export service, gated by the root's read scope.
    nonisolated static func assembled(root: ServiceCompositionRoot) -> ExportSettingsModel {
        ExportSettingsModel(
            controls: root.exportService, readScope: { [weak root] in root?.readScope ?? .captureOnly })
    }
}
