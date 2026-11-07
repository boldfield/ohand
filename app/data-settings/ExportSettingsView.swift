import SwiftUI
import UIKit
import OhAndCoreBridge
import OhAndServices

/// Presents the system share sheet for the pending export straight from UIKit and passes on how it ended.
///
/// The sheet's `completionWithItemsHandler` is the only authority for the ending: it runs after the sheet is dismissed,
/// whether the user cancelled, swiped it away or an activity finished, and for share extensions only once the
/// extension is done. Nesting the sheet in a SwiftUI `.sheet` would add a second, earlier dismissal signal that cannot
/// tell a cancel from a share still in progress, so the sheet is presented here instead. When the model drops the
/// export without an ending (leaving the screen or locking), a sheet still on screen is dismissed with its handler
/// detached.
private struct ExportShareSheetPresenter: UIViewControllerRepresentable {
    /// What to share: an identifier for the pending export and the files in it.
    struct Share: Equatable {
        let id: String
        let items: [URL]
    }

    let export: Share?
    let onEnding: (_ exportID: String, _ ending: ExportShareEnding) -> Void

    final class Coordinator {
        var exportID: String?
        weak var sheet: UIActivityViewController?

        func detachSheet() -> UIActivityViewController? {
            let detached = sheet
            sheet = nil
            detached?.completionWithItemsHandler = nil
            return detached
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIViewController(context: Context) -> UIViewController { UIViewController() }

    func updateUIViewController(_ anchor: UIViewController, context: Context) {
        let coordinator = context.coordinator
        guard coordinator.exportID != export?.id else { return }
        coordinator.exportID = export?.id
        let stale = coordinator.detachSheet()
        let export = self.export
        let onEnding = self.onEnding
        let present = { [weak anchor] in
            guard let export, coordinator.exportID == export.id else { return }
            guard let anchor, anchor.view.window?.windowScene != nil, anchor.presentedViewController == nil else {
                // Without a sheet on screen nothing can be shared and no handler would ever answer; ending it now
                // lets the service remove the files and the screen say so.
                onEnding(export.id, .failed)
                return
            }
            let sheet = UIActivityViewController(activityItems: export.items, applicationActivities: nil)
            sheet.completionWithItemsHandler = { _, completed, _, error in
                onEnding(export.id, ExportShareSheet.ending(completed: completed, error: error))
            }
            sheet.popoverPresentationController?.sourceView = anchor.view
            coordinator.sheet = sheet
            anchor.present(sheet, animated: true)
        }
        DispatchQueue.main.async {
            if let stale, stale.presentingViewController != nil {
                stale.dismiss(animated: false, completion: present)
            } else {
                present()
            }
        }
    }

    static func dismantleUIViewController(_ anchor: UIViewController, coordinator: Coordinator) {
        coordinator.exportID = nil
        if let stale = coordinator.detachSheet(), stale.presentingViewController != nil {
            stale.dismiss(animated: false)
        }
    }
}

/// The export section of the data settings: scope and audio choices, the limits of an export, and how the last export
/// ended. Everything it shows comes from `ExportSettingsModel`.
struct ExportSettingsView: View {
    static let audioToggleIdentifier = "data-settings.export.audio-toggle"
    static let startIdentifier = "data-settings.export.start"
    static let footerIdentifier = "data-settings.export.footer"
    static let stateIdentifier = "data-settings.export.state"
    static let lockedIdentifier = "data-settings.export.locked"

    static func scopeIdentifier(_ scope: CoreExportScope) -> String { "data-settings.export.scope.\(scope.rawValue)" }

    @ObservedObject var model: ExportSettingsModel
    var renderReporter: (([String: RetentionRenderedElement]) -> Void)?

    init(model: ExportSettingsModel, renderReporter: (([String: RetentionRenderedElement]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

    /// The full explanation behind the info button: what an export is, its limits and what happens to audio.
    static let infoParagraphs = [
        ExportSettingsText.explanation, ExportSettingsText.limitsNote, ExportSettingsText.audioNote,
    ]

    var body: some View {
        SettingsScreenChrome(title: ExportSettingsText.heading, infoTitle: "About exports", infoParagraphs: Self.infoParagraphs) {
            content
        }
        .onPreferenceChange(RetentionRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onDisappear { model.clear() }
        .background(
            ExportShareSheetPresenter(
                export: model.pendingShare.map { .init(id: $0.id, items: $0.shareItems) }
            ) { exportID, ending in
                model.shareEnded(ending, exportID: exportID)
            }
            .frame(width: 0, height: 0)
            .accessibilityHidden(true)
        )
    }

    @ViewBuilder
    private var content: some View {
        if model.phase == .locked {
            note(ExportSettingsText.lockedText, identifier: Self.lockedIdentifier)
        } else {
            DSGroupedSection(title: "What to export", footer: scopeFooter) {
                ForEach(ExportSettingsModel.selectableScopes, id: \.self) { scope in
                    DSToggleRow(
                        label: ExportSettingsText.scopeTitle(scope),
                        isOn: Binding(
                            get: { model.isSelected(scope) }, set: { model.setScope(scope, selected: $0) })
                    )
                    .accessibilityIdentifier(Self.scopeIdentifier(scope))
                    .reportDataSettingsRendered(Self.scopeIdentifier(scope), text: ExportSettingsText.scopeTitle(scope))
                    .disabled(isBusy)
                }
                DSToggleRow(label: "Include audio recordings", isOn: $model.includeAudio)
                    .accessibilityIdentifier(Self.audioToggleIdentifier)
                    .reportDataSettingsRendered(Self.audioToggleIdentifier, text: "Include audio recordings")
                    .disabled(isBusy)
            }
            .reportDataSettingsRendered(Self.footerIdentifier, text: scopeFooter)
            if let message = stateMessage {
                DSGroupedSection(footer: message) { exportButton }
                    .reportDataSettingsRendered(Self.stateIdentifier, text: message)
            } else {
                DSGroupedSection { exportButton }
            }
            if let metrics = model.metrics {
                MetricsExportSection(model: metrics)
            }
        }
    }

    private var exportButton: some View {
        DSPillButton(title: "Export") { Task { await model.startExport() } }
            .frame(maxWidth: .infinity)
            .padding(DSSpacing.sm)
            .accessibilityIdentifier(Self.startIdentifier)
            .reportDataSettingsRendered(Self.startIdentifier, text: "Export")
            .disabled(!model.canStart)
    }

    private var scopeFooter: String {
        ExportSettingsText.footer(hasScope: !model.selectedScopes.isEmpty, includeAudio: model.includeAudio)
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
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
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
            controls: root.exportService, readScope: { [weak root] in root?.readScope ?? .captureOnly },
            metrics: .assembled(root: root))
    }
}

/// The metrics part of the export screen: the content-free report export and the recording controls. It is offered
/// only in an authenticated read session; a capture-only session sees why instead.
struct MetricsExportSection: View {
    static let exportIdentifier = "data-settings.export.metrics.export"
    static let pauseIdentifier = "data-settings.export.metrics.pause"
    static let clearIdentifier = "data-settings.export.metrics.clear"
    static let footerIdentifier = "data-settings.export.metrics.footer"
    static let stateIdentifier = "data-settings.export.metrics.state"
    static let lockedIdentifier = "data-settings.export.metrics.locked"
    static let holdsIdentifier = "data-settings.export.metrics.holds"

    @ObservedObject var model: MetricsExportSettingsModel

    var body: some View {
        Group {
            if model.isAvailable {
                DSGroupedSection(title: ExportSettingsText.metricsHeading, footer: footer) {
                    Text(ExportSettingsText.metricsHolds + " " + ExportSettingsText.metricsHoldsNot)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(DSSpacing.sm)
                        .accessibilityIdentifier(Self.holdsIdentifier)
                        .reportDataSettingsRendered(
                            Self.holdsIdentifier, text: ExportSettingsText.metricsHolds + " " + ExportSettingsText.metricsHoldsNot)
                    button(ExportSettingsText.metricsExportTitle, Self.exportIdentifier, enabled: model.canExport) {
                        await model.startExport()
                    }
                    button(
                        model.isRecording ? ExportSettingsText.metricsPauseTitle : ExportSettingsText.metricsResumeTitle,
                        Self.pauseIdentifier, enabled: model.canExport
                    ) { await model.setRecording(!model.isRecording) }
                    button(ExportSettingsText.metricsClearTitle, Self.clearIdentifier, enabled: model.canExport) {
                        await model.clearMetrics()
                    }
                }
                .reportDataSettingsRendered(Self.footerIdentifier, text: footer)
            } else {
                Text(ExportSettingsText.metricsLockedText)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Self.lockedIdentifier)
                    .reportDataSettingsRendered(Self.lockedIdentifier, text: ExportSettingsText.metricsLockedText)
            }
        }
        .task { await model.refresh() }
        .onDisappear { model.clear() }
        .background(
            ExportShareSheetPresenter(
                export: model.pendingShare.map { .init(id: $0.id, items: [$0.file]) }
            ) { exportID, ending in
                model.shareEnded(ending, exportID: exportID)
            }
            .frame(width: 0, height: 0)
            .accessibilityHidden(true)
        )
    }

    private var footer: String {
        if case .finished(let text) = model.phase { return text }
        switch model.phase {
        case .preparing: return ExportSettingsText.preparingText
        case .sharing: return ExportSettingsText.readyText
        default:
            return model.isRecording
                ? ExportSettingsText.metricsRecordingOnFooter : ExportSettingsText.metricsRecordingOffFooter
        }
    }

    private func button(
        _ title: String, _ identifier: String, enabled: Bool, action: @escaping () async -> Void
    ) -> some View {
        DSPillButton(title: title) { Task { await action() } }
            .frame(maxWidth: .infinity)
            .padding(DSSpacing.sm)
            .accessibilityIdentifier(identifier)
            .reportDataSettingsRendered(identifier, text: title)
            .disabled(!enabled)
    }
}
