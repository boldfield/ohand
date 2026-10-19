import SwiftUI

/// The reminder section of an item: what is stored and what the system is known to hold, the date, time and zone to
/// choose, and cancellation. Everything it says comes from `ReminderEditModel`.
struct ReminderEditSection: View {
    static let sectionIdentifier = "reminder.section"
    static let standingIdentifier = "reminder.standing"
    static let zoneIdentifier = "reminder.zone"
    static let messageIdentifier = "reminder.message"
    static let saveIdentifier = "reminder.save"
    static let cancelIdentifier = "reminder.cancel"
    static let unavailableIdentifier = "reminder.unavailable"

    static let lockedText = "Your saved items are locked, so the reminder is hidden."
    static let unreadableText = "The reminder could not be read. Try again later."

    @ObservedObject var model: ReminderEditModel

    var body: some View {
        if model.isShown {
            VStack(alignment: .leading, spacing: DSSpacing.xs) {
                Text("Reminder")
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                    .padding(.horizontal, DSSpacing.md)
                DSCard(padding: DSSpacing.md) { content }
            }
            .accessibilityIdentifier(Self.sectionIdentifier)
            .task { await model.load() }
        } else {
            // An item that is not an action has no reminder section.
            Color.clear.frame(height: 0).task { await model.load() }
        }
    }

    @ViewBuilder private var content: some View {
        if let unavailable = model.unavailable {
            Text(unavailable == .locked ? Self.lockedText : Self.unreadableText)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(Self.unavailableIdentifier)
        } else {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                standing
                if let zone = model.reminder?.timezoneID, model.reminder?.requestState == "resolved" {
                    Text("Time zone of this reminder: \(zone)")
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.zoneIdentifier)
                }
                DatePicker("Date and time", selection: $model.draftDate, displayedComponents: [.date, .hourAndMinute])
                    .environment(\.timeZone, TimeZone(identifier: model.draftTimeZoneID) ?? .current)
                    .disabled(!model.canSave)
                Picker("Time zone", selection: $model.draftTimeZoneID) {
                    ForEach(zoneChoices, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.menu)
                .disabled(!model.canSave)
                if let message = model.message {
                    Text(message.text)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.messageIdentifier)
                }
                actions
            }
        }
    }

    private var standing: some View {
        let standing = model.standing
        return VStack(alignment: .leading, spacing: DSSpacing.xxs) {
            Text(standing.title)
                .font(DSFont.body)
                .foregroundColor(DSColor.ink.color)
            Text(standing.detail)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier(Self.standingIdentifier)
    }

    private var actions: some View {
        HStack(spacing: DSSpacing.sm) {
            Button { Task { await model.save() } } label: { Text("Save time") }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canSave)
                .accessibilityIdentifier(Self.saveIdentifier)
            if model.standing.offersCancel {
                Button { Task { await model.cancel() } } label: { Text("Cancel reminder") }
                    .buttonStyle(DSPillButtonStyle(style: .destructive))
                    .disabled(!model.canCancel)
                    .accessibilityIdentifier(Self.cancelIdentifier)
            }
        }
    }

    /// Every known zone, with the one in use first so a stored zone the system list lacks is still selectable.
    private var zoneChoices: [String] {
        let known = TimeZone.knownTimeZoneIdentifiers
        return known.contains(model.draftTimeZoneID) ? known : [model.draftTimeZoneID] + known
    }
}
