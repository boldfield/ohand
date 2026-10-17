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
    static let footerIdentifier = "data-settings.retention.footer"
    static let pressureIdentifier = "data-settings.retention.pressure"
    static let periodStepperIdentifier = "data-settings.retention.period-stepper"
    static let stepperIdentifier = "data-settings.retention.stepper"
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

    static let periodChoiceDays = [1, 3, 7, 14, 30, 90]
    static let footerText = "The text is always kept. A recording with no text yet is never removed."

    /// The full explanation behind the info button: the policy, the sole-source rule and what a period change does.
    static func infoParagraphs(for status: RetentionStatus?) -> [String] {
        var paragraphs = [RetentionSettingsText.soleSourceExplanation, RetentionSettingsText.periodChangeNote]
        if let status {
            paragraphs.insert(RetentionSettingsText.policyExplanation(status.policy), at: 0)
        }
        return paragraphs
    }

    var body: some View {
        SettingsScreenChrome(title: "Audio", infoTitle: "Audio retention", infoParagraphs: Self.infoParagraphs(for: model.status)) {
            content
            if let exportSettings = model.exportSettings {
                NavigationLink {
                    ExportSettingsView(model: exportSettings, renderReporter: renderReporter)
                } label: {
                    DSRow(label: "Export a copy of your notes", iconSystemName: "square.and.arrow.up")
                        .allowsHitTesting(false)
                }
                .buttonStyle(.plain)
                .background(RoundedRectangle(cornerRadius: DSRadius.card, style: .continuous).fill(DSColor.surface1.color))
                .accessibilityIdentifier(Self.exportLinkIdentifier)
                .reportDataSettingsRendered(Self.exportLinkIdentifier, text: "Export a copy of your notes")
            }
        }
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
            DSPillButton(title: "Try again", style: .secondary) { Task { await model.refresh() } }
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

    /// The preset periods, plus the stored and chosen ones when they are not presets (a stored 45 days, say).
    private func periodChoices(_ status: RetentionStatus) -> [Int] {
        Set(Self.periodChoiceDays + [model.chosenDays, storedDays(status)]).sorted()
    }

    private func storedDays(_ status: RetentionStatus) -> Int {
        Int(status.policy.periodSeconds / 86_400)
    }

    /// The stored period is named with the core's summary, which marks the default; the other rows give the length.
    private func choiceLabel(days: Int, status: RetentionStatus) -> String {
        days == storedDays(status) ? RetentionSettingsText.periodSummary(status.policy) : RetentionSettingsText.dayCount(days)
    }

    private func policySection(_ status: RetentionStatus) -> some View {
        VStack(alignment: .leading, spacing: DSSpacing.sm) {
            DSGroupedSection(title: "Keep recordings after they become text", footer: Self.footerText) {
                ForEach(periodChoices(status), id: \.self) { days in
                    DSChoiceRow(label: choiceLabel(days: days, status: status), isSelected: model.chosenDays == days) {
                        model.chosenDays = days
                    }
                }
                // Any whole number of days the core supports can be chosen, not only the presets above.
                Stepper(
                    value: $model.chosenDays, in: RetentionSettingsModel.minimumDays...RetentionSettingsModel.maximumDays
                ) {
                    Text("Another length: \(RetentionSettingsText.dayCount(model.chosenDays))")
                        .font(DSFont.rowLabel)
                        .foregroundColor(DSColor.ink.color)
                }
                .padding(.horizontal, DSSpacing.md)
                .frame(minHeight: DSLayout.rowMinimumHeight)
                .accessibilityIdentifier(Self.stepperIdentifier)
                .reportDataSettingsRendered(Self.stepperIdentifier, text: RetentionSettingsText.dayCount(model.chosenDays))
            }
            .accessibilityIdentifier(Self.periodStepperIdentifier)
            .reportDataSettingsRendered(Self.periodStepperIdentifier, text: RetentionSettingsText.dayCount(model.chosenDays))
            .reportDataSettingsRendered(Self.footerIdentifier, text: Self.footerText)
            .reportDataSettingsRendered(Self.policyIdentifier, text: choiceLabel(days: storedDays(status), status: status))
            DSPillButton(title: "Save period") { Task { await model.applyPeriod() } }
                .frame(maxWidth: .infinity)
                .accessibilityIdentifier(Self.applyIdentifier)
                .reportDataSettingsRendered(Self.applyIdentifier, text: "Save period")
                .disabled(model.isApplyingPeriod)
            if let message = model.periodState?.message {
                note(message, identifier: Self.periodStateIdentifier)
            }
        }
    }

    @ViewBuilder
    private func unresolvedSection(_ status: RetentionStatus) -> some View {
        if status.unresolved.isEmpty {
            DSGroupedSection(title: "Waiting for text") {
                Text(RetentionSettingsText.noUnresolvedText)
                    .font(DSFont.rowLabel)
                    .foregroundColor(DSColor.inkMuted.color)
                    .padding(.horizontal, DSSpacing.md)
                    .frame(maxWidth: .infinity, minHeight: DSLayout.rowMinimumHeight, alignment: .leading)
                    .accessibilityIdentifier(Self.noUnresolvedIdentifier)
                    .reportDataSettingsRendered(Self.noUnresolvedIdentifier, text: RetentionSettingsText.noUnresolvedText)
            }
        } else {
            DSGroupedSection(title: "Waiting for text") {
                ForEach(status.unresolved, id: \.itemID) { entry in
                    caseRow(entry)
                }
            }
        }
    }

    private func caseRow(_ entry: UnresolvedSoleAudioCase) -> some View {
        let busy = model.actions[entry.itemID] == .working
        return VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text(RetentionSettingsText.reason(entry.reason))
                .font(DSFont.rowLabel)
                .foregroundColor(DSColor.ink.color)
                .fixedSize(horizontal: false, vertical: true)
            Text("Captured \(entry.capturedAt)")
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
            if entry.options.contains(.retryTranscription) {
                DSPillButton(title: "Retry turning into text", style: .secondary) {
                    Task { await model.retryTranscription(itemID: entry.itemID) }
                }
                .accessibilityIdentifier(Self.retryIdentifier(entry.itemID))
                .reportDataSettingsRendered(Self.retryIdentifier(entry.itemID), text: "Retry turning into text")
                .disabled(busy)
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
        .padding(DSSpacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Self.caseIdentifier(entry.itemID))
        .reportDataSettingsRendered(Self.caseIdentifier(entry.itemID), text: RetentionSettingsText.reason(entry.reason))
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, DSSpacing.md)
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
