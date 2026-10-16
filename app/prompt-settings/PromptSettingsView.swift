import SwiftUI

/// The optional daily prompt screen: an on/off switch that starts off, the local time, and plain words about how far
/// ahead prompts are planned. Everything it shows comes from `PromptSettingsModel`.
struct PromptSettingsView: View {
    static let toggleIdentifier = "prompt-settings.toggle"
    static let timeIdentifier = "prompt-settings.time"
    static let noticeIdentifier = "prompt-settings.notice"
    static let nextIdentifier = "prompt-settings.next"
    static let explanationIdentifier = "prompt-settings.explanation"

    static let introText = "A daily prompt is a gentle nudge to look at one saved item. It is off unless you turn it on."
    static let horizonText = "Oh And plans at most the next \(PromptSettingsModel.horizonDays) days of prompts and tops them up each time you open the app. If you do not open it for \(PromptSettingsModel.horizonDays) days, prompts pause until you do. Missed days are skipped, never sent together."
    static let privacyText = "A prompt says only that something is ready. It never shows your items; you see one after you unlock and open the app."
    static let remindersText = "Reminders you set yourself are separate. Changing or turning off prompts never changes them."

    static func text(for notice: PromptSettingsNotice) -> String {
        switch notice {
        case .saved: return "Saved."
        case .savedNotificationsOff: return "Saved, but notifications are not allowed for Oh And, so no prompt will appear. Allow them in iOS Settings; prompts are not sent later to catch up."
        case .savedWaitingForRoom: return "Saved, but your reminders are using every notification slot the system allows, so prompts wait. They start when there is room, only for the next \(PromptSettingsModel.horizonDays) days; days that passed are skipped."
        case .savedNotYetApplied: return "Saved. Prompts could not be updated just now and will be updated the next time you open the app."
        case .notSaved: return "That change could not be saved. Your earlier choice is still in place."
        case .unreadable: return "Your prompt setting could not be read. Prompts stay as they were."
        }
    }

    @ObservedObject var model: PromptSettingsModel

    var body: some View {
        Form {
            Section {
                Toggle("Daily prompt", isOn: Binding(
                    get: { model.isEnabled },
                    set: { value in Task { await model.setEnabled(value) } }))
                    .disabled(model.isBusy || !model.hasLoaded)
                    .accessibilityIdentifier(Self.toggleIdentifier)
                DatePicker("Time", selection: Binding(
                    get: { model.time },
                    set: { value in Task { await model.setTime(value) } }),
                    displayedComponents: .hourAndMinute)
                    .disabled(model.isBusy || !model.isEnabled)
                    .accessibilityIdentifier(Self.timeIdentifier)
                if let next = model.nextPrompt {
                    Text("Next prompt: \(next.formatted(date: .abbreviated, time: .shortened))")
                        .accessibilityIdentifier(Self.nextIdentifier)
                }
                if let notice = model.notice {
                    Text(Self.text(for: notice))
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.noticeIdentifier)
                }
            }
            Section {
                Text("\(Self.introText)\n\n\(Self.horizonText)\n\n\(Self.privacyText)\n\n\(Self.remindersText)")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier(Self.explanationIdentifier)
            }
        }
        .navigationTitle("Daily prompt")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.load() }
    }
}
