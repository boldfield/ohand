import SwiftUI
import OhAndCoreBridge

/// The correction controls on the source-detail screen: edit the words, correct the type, set the session topic.
/// Everything it does goes through `ItemControlsModel`.
struct ItemControlsView: View {
    static let textFieldIdentifier = "item-controls.text.field"
    static let textSaveIdentifier = "item-controls.text.save"
    static let typePickerIdentifier = "item-controls.type.picker"
    static let typeApplyIdentifier = "item-controls.type.apply"
    static let topicFieldIdentifier = "item-controls.topic.field"
    static let topicSaveIdentifier = "item-controls.topic.save"
    static let sectionIdentifier = "item-controls.section"
    static let actionWarningIdentifier = "item-controls.type.action-warning"
    static let outcomeIdentifier = "item-controls.outcome"

    static let actionWarning =
        "Applying this makes the item an action. Changing the type never happens on its own."
    static let topicFooter = "The session topic is a label. It does not change whether the item is personal or work."

    static func typeTitle(_ type: ItemCorrectionItemType) -> String {
        switch type {
        case .broadIntention: return "Broad intention"
        case .note: return "Note"
        case .idea: return "Idea"
        case .action: return "Action"
        }
    }

    static func message(for outcome: ItemControlsOutcome) -> String {
        switch outcome {
        case .saved: return "Saved. Your original is kept."
        case .savedAndReminderCancelled: return "Saved. The reminder for this item was cancelled."
        case .conflict:
            return "This item changed after you opened it, so nothing was saved. "
                + "The newer values are shown; check them and save again if you still want your change."
        case .locked: return "Your saved items are locked. Open the item again to unlock it."
        case .itemGone: return "This item is not available. It may have been deleted."
        case .invalid: return "That change could not be saved. Check it and try again."
        case .failed: return "That change could not be saved right now. Try again."
        case .loadFailed: return "This item could not be read right now."
        }
    }

    @ObservedObject var model: ItemControlsModel
    /// Told when an identified element comes on or goes off the screen; tests use it to check what is rendered.
    var renderReporter: ((String, Bool) -> Void)?

    var body: some View {
        Group {
            if model.snapshot != nil {
                VStack(alignment: .leading, spacing: DSSpacing.md) {
                    Text("Correct this item")
                        .font(DSFont.sectionTitle)
                        .foregroundColor(DSColor.ink.color)
                        .accessibilityAddTraits(.isHeader)
                        .rendered(Self.sectionIdentifier, renderReporter)
                    textControl
                    typeControl
                    topicControl
                    if let outcome = model.outcome {
                        Text(Self.message(for: outcome))
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.inkMuted.color)
                            .fixedSize(horizontal: false, vertical: true)
                            .accessibilityIdentifier(Self.outcomeIdentifier)
                            .rendered(Self.outcomeIdentifier, renderReporter)
                    }
                }
                .disabled(model.snapshot?.isDeleted == true)
            }
        }
        .task { await model.load() }
    }

    private var textControl: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("Words")
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
            TextField("Corrected words", text: $model.textDraft, axis: .vertical)
                .lineLimit(1...8)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Self.textFieldIdentifier)
            Button("Save words") { Task { await model.saveText() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canSaveText)
                .accessibilityIdentifier(Self.textSaveIdentifier)
        }
    }

    private var typeControl: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("Type")
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
            Picker("Type", selection: $model.pendingType) {
                if model.snapshot?.itemType == nil { Text("Not set").tag(ItemCorrectionItemType?.none) }
                ForEach(ItemCorrectionItemType.allCases, id: \.self) { type in
                    Text(Self.typeTitle(type)).tag(ItemCorrectionItemType?.some(type))
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier(Self.typePickerIdentifier)
            if model.pendingTypeMakesAction {
                Text(Self.actionWarning)
                    .rendered(Self.actionWarningIdentifier, renderReporter)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button("Change type") { Task { await model.applyType() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canApplyType)
                .accessibilityIdentifier(Self.typeApplyIdentifier)
        }
    }

    private var topicControl: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("Session topic")
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
            TextField("Topic", text: $model.topicDraft)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Self.topicFieldIdentifier)
            Text(Self.topicFooter)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
            Button("Save topic") { Task { await model.saveSessionTopic() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canSaveTopic)
                .accessibilityIdentifier(Self.topicSaveIdentifier)
        }
    }
}

private extension View {
    func rendered(_ identifier: String, _ reporter: ((String, Bool) -> Void)?) -> some View {
        accessibilityIdentifier(identifier)
            .onAppear { reporter?(identifier, true) }
            .onDisappear { reporter?(identifier, false) }
    }
}
