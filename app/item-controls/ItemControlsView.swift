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
    static let refreshFailureIdentifier = "item-controls.refresh-failure"
    static let reloadIdentifier = "item-controls.reload"
    static let completeIdentifier = "item-controls.action.complete"
    static let cancelIdentifier = "item-controls.action.cancel"
    static let notNowIdentifier = "item-controls.action.not-now"
    static let stopSuggestingIdentifier = "item-controls.action.stop-suggesting"
    static let actionsIdentifier = "item-controls.actions"
    static let actionsFooterIdentifier = "item-controls.actions.footer"
    static let endedNoticeIdentifier = "item-controls.ended"
    static let reopenIdentifier = "item-controls.action.reopen"
    static let undoIdentifier = "item-controls.action.undo"
    static let reminderOffIdentifier = "item-controls.reminder-off"
    static let setReminderAgainIdentifier = "item-controls.action.set-reminder-again"
    static let reminderAgainDoneIdentifier = "item-controls.reminder-again.done"
    static let reminderOffNotice = "Its reminder was turned off."
    /// Reported as shown while the fields can be edited and as gone while they are disabled.
    static let editableIdentifier = "item-controls.editable"

    static let actionWarning =
        "Applying this makes the item an action. Changing the type never happens on its own."
    static let topicFooter = "The session topic is a label. It does not change whether the item is personal or work."

    static let notNowExplanation =
        "Not now keeps this item out of suggestions for an hour. After that it can be suggested again."
    static let stopSuggestingExplanation =
        "Stop suggesting means this item is only shown when you search or open it. "
        + "It stays searchable and any reminder you set for it is kept."
    static let suggestionsStoppedNotice =
        "Suggestions are stopped for this item. It is only shown when you search or open it, "
        + "and any reminder you set for it is kept."
    static let completeCancelExplanation =
        "Completing or cancelling ends the item and cancels any reminder for it. Your original is kept."

    static func endedNotice(_ lifecycleState: String) -> String {
        lifecycleState == "completed" ? "This item is completed." : "This item is cancelled."
    }

    static func typeTitle(_ type: ItemCorrectionItemType) -> String {
        switch type {
        case .broadIntention: return "Broad intention"
        case .note: return "Note"
        case .idea: return "Idea"
        case .action: return "Action"
        }
    }

    /// `newerValuesShown` is false when the stored values could not be read again after the attempt.
    static func message(for outcome: ItemControlsOutcome, newerValuesShown: Bool = true) -> String {
        switch outcome {
        case .saved: return "Saved. Your original is kept."
        case .savedAndReminderCancelled: return "Saved. The reminder for this item was cancelled."
        case .conflict where !newerValuesShown:
            return "This item changed after you opened it, so nothing was saved. "
                + "Your change is kept, but the newer values could not be loaded yet."
        case .conflict:
            return "This item changed after you opened it, so nothing was saved. "
                + "The newer values are shown; check them and save again if you still want your change."
        case .locked: return "Your saved items are locked. Open the item again to unlock it."
        case .itemGone: return "This item is not available. It may have been deleted."
        case .invalid: return "That change could not be saved. Check it and try again."
        case .failed: return "That change could not be saved right now. Try again."
        case .loadFailed: return "This item could not be read right now."
        case .actionDone(.complete): return "Completed. Any reminder for it was cancelled."
        case .actionDone(.cancel): return "Cancelled. Any reminder for it was cancelled."
        case .actionDone(.notNow): return "Okay. This item will not be suggested for an hour."
        case .actionDone(.stopSuggesting):
            return "Okay. This item will not be suggested again. You can still search for it, and any reminder is kept."
        case .actionConflict where !newerValuesShown:
            return "This item changed after you opened it, so nothing was done. The newer values could not be loaded yet."
        case .actionConflict:
            return "This item changed after you opened it, so nothing was done. "
                + "The newer values are shown; choose again if you still want to."
        case .actionItemEnded(let lifecycle): return Self.endedNotice(lifecycle) + " Nothing was changed."
        case .actionDone(.reopen): return "Reopened."
        case .actionAlreadyOpen: return "This item is already open. Nothing was changed."
        case .actionFailed: return "That could not be done right now. Try again."
        }
    }

    static func message(for failure: ItemControlsRefreshFailure) -> String {
        switch failure {
        case .locked:
            return "Your saved items are locked, so the latest values could not be loaded. "
                + "Editing is paused; unlock and reload to continue."
        case .itemGone: return "This item is no longer available. It may have been deleted."
        case .unreadable:
            return "The latest values could not be loaded, so editing is paused. Your unsaved changes are kept."
        }
    }

    @ObservedObject var model: ItemControlsModel
    /// Told when an identified element comes on or goes off the screen; tests use it to check what is rendered.
    var renderReporter: ((String, Bool) -> Void)?

    var body: some View {
        // A container that always exists, so the load below runs even while there is nothing to show yet.
        VStack(alignment: .leading, spacing: DSSpacing.md) {
            if model.snapshot != nil {
                Text("Correct this item")
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                    .rendered(Self.sectionIdentifier, renderReporter)
                // While locked, the stored words and drafts stay off the screen; they come back after a reload.
                if model.refreshFailure != .locked {
                    VStack(alignment: .leading, spacing: DSSpacing.md) {
                        textControl
                        typeControl
                        topicControl
                        actionsControl
                    }
                    .modifier(EnabledReporting(identifier: Self.editableIdentifier, reporter: renderReporter))
                    // Paused while a correction or its refresh is in flight, so nothing typed then can be replaced.
                    .disabled(!model.canEdit)
                }
            }
            if let outcome = model.outcome {
                Text(Self.message(for: outcome, newerValuesShown: model.refreshFailure == nil))
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .rendered(Self.outcomeIdentifier, renderReporter)
            }
            if model.canUndo {
                Button("Undo") { Task { await model.reopen() } }
                    .buttonStyle(DSPillButtonStyle(style: .secondary))
                    .accessibilityIdentifier(Self.undoIdentifier)
                    .rendered(Self.undoIdentifier, renderReporter)
            }
            if model.reminderAgainPrompt != nil, model.refreshFailure != .locked {
                reminderAgainControl
            }
            if let failure = model.refreshFailure {
                Text(Self.message(for: failure))
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .rendered(Self.refreshFailureIdentifier, renderReporter)
                if model.canReload {
                    Button("Reload") { Task { await model.reload() } }
                        .buttonStyle(DSPillButtonStyle(style: .secondary))
                        .rendered(Self.reloadIdentifier, renderReporter)
                }
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
                .dsFieldStyle()
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

    @ViewBuilder private var reminderAgainControl: some View {
        Text(Self.reminderOffNotice)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .rendered(Self.reminderOffIdentifier, renderReporter)
        if let edit = model.reminderEdit {
            ReminderEditSection(model: edit)
            Button("Done") { model.dismissReminderAgain() }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .accessibilityIdentifier(Self.reminderAgainDoneIdentifier)
                .rendered(Self.reminderAgainDoneIdentifier, renderReporter)
        } else if model.canSetReminderAgain {
            Button("Set a reminder again") { Task { await model.setReminderAgain() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .accessibilityIdentifier(Self.setReminderAgainIdentifier)
                .rendered(Self.setReminderAgainIdentifier, renderReporter)
        }
    }

    @ViewBuilder private var actionsControl: some View {
        if let snapshot = model.snapshot, snapshot.lifecycleState != "active", snapshot.lifecycleState != "deleted" {
            Text(Self.endedNotice(snapshot.lifecycleState))
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .rendered(Self.endedNoticeIdentifier, renderReporter)
        }
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("Finish or quiet")
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
            if model.isClosed {
                Button("Reopen") { Task { await model.reopen() } }
                    .buttonStyle(DSPillButtonStyle(style: .secondary))
                    .disabled(!model.canReopen)
                    .accessibilityIdentifier(Self.reopenIdentifier)
                    .rendered(Self.reopenIdentifier, renderReporter)
            } else {
                Button("Complete") { Task { await model.complete() } }
                    .buttonStyle(DSPillButtonStyle(style: .secondary))
                    .disabled(!model.canAct)
                    .accessibilityIdentifier(Self.completeIdentifier)
                Button("Cancel item") { Task { await model.cancel() } }
                    .buttonStyle(DSPillButtonStyle(style: .secondary))
                    .disabled(!model.canAct)
                    .accessibilityIdentifier(Self.cancelIdentifier)
                Text(Self.completeCancelExplanation)
                    .font(DSFont.footer)
                    .foregroundColor(DSColor.inkMuted.color)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Button("Not now") { Task { await model.notNow() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canQuietSuggestions)
                .accessibilityIdentifier(Self.notNowIdentifier)
            Button("Stop suggesting") { Task { await model.stopSuggesting() } }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canQuietSuggestions)
                .accessibilityIdentifier(Self.stopSuggestingIdentifier)
            Text(model.snapshot?.suggestionsStopped == true
                ? Self.suggestionsStoppedNotice : Self.notNowExplanation + " " + Self.stopSuggestingExplanation)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .rendered(Self.actionsFooterIdentifier, renderReporter)
        }
        .rendered(Self.actionsIdentifier, renderReporter)
    }

    private var topicControl: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text("Session topic")
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
            TextField("Topic", text: $model.topicDraft)
                .dsFieldStyle()
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

/// Reports whether the controls it wraps are enabled, read from the same environment that disables them.
private struct EnabledReporting: ViewModifier {
    @Environment(\.isEnabled) private var isEnabled
    let identifier: String
    let reporter: ((String, Bool) -> Void)?

    func body(content: Content) -> some View {
        content
            .onAppear { reporter?(identifier, isEnabled) }
            .onChange(of: isEnabled) { enabled in reporter?(identifier, enabled) }
            .onDisappear { reporter?(identifier, false) }
    }
}

private extension View {
    func rendered(_ identifier: String, _ reporter: ((String, Bool) -> Void)?) -> some View {
        accessibilityIdentifier(identifier)
            .onAppear { reporter?(identifier, true) }
            .onDisappear { reporter?(identifier, false) }
    }
}

private extension View {
    /// A text field drawn from design-system tokens, so it reads the same in light and dark appearance.
    func dsFieldStyle() -> some View {
        self
            .font(DSFont.body)
            .foregroundColor(DSColor.ink.color)
            .padding(DSSpacing.sm)
            .background(
                RoundedRectangle(cornerRadius: DSRadius.field, style: .continuous).fill(DSColor.surface2.color))
    }
}
