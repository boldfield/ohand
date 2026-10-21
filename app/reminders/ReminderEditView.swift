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
    static let storedTimeIdentifier = "reminder.stored-time"
    static let historyIdentifier = "reminder.history"

    /// What each identified part of the section shows, collected so a hosted test can read the rendered words.
    struct RenderedKey: PreferenceKey {
        static var defaultValue: [String: String] { [:] }
        static func reduce(value: inout [String: String], nextValue: () -> [String: String]) {
            value.merge(nextValue()) { _, new in new }
        }
    }

    static let lockedText = "Your saved items are locked, so the reminder is hidden."
    static let unreadableText = "The reminder could not be read. Try again later."

    @ObservedObject var model: ReminderEditModel
    /// Lets a hosted test see what the section renders; production leaves it unset.
    var renderReporter: (([String: String]) -> Void)?

    init(model: ReminderEditModel, renderReporter: (([String: String]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

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
            .onPreferenceChange(RenderedKey.self) { renderReporter?($0) }
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
                .reportReminderRendered(Self.unavailableIdentifier, text: unavailable == .locked ? Self.lockedText : Self.unreadableText)
        } else {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                standing
                if let zone = model.reminder?.timezoneID, model.reminder?.resolvedDate != nil {
                    Text("Time zone of this reminder: \(zone)")
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.zoneIdentifier)
                        .reportReminderRendered(Self.zoneIdentifier, text: "Time zone of this reminder: \(zone)")
                }
                // The stored time, apart from the draft below: the draft is only what would be saved.
                if let stored = ReminderStoredTime.text(for: model.reminder) {
                    Text(stored)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.storedTimeIdentifier)
                        .reportReminderRendered(Self.storedTimeIdentifier, text: stored)
                }
                DatePicker("Date and time", selection: $model.draftDate, displayedComponents: [.date, .hourAndMinute])
                    .environment(\.timeZone, TimeZone(identifier: model.draftTimeZoneID) ?? .current)
                    .disabled(!model.canSave)
                Picker("Time zone", selection: $model.draftTimeZoneID) {
                    ForEach(zoneChoices, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.menu)
                .disabled(!model.canSave)
                if let message = model.shownMessage {
                    Text(message.text)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.messageIdentifier)
                        .reportReminderRendered(Self.messageIdentifier, text: message.text)
                }
                actions
                if let history = model.history { ReminderHistoryList(model: history) }
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
        .reportReminderRendered(Self.standingIdentifier, text: "\(standing.title) | \(standing.detail)")
    }

    private var actions: some View {
        HStack(spacing: DSSpacing.sm) {
            Button { Task { await model.save() } } label: { Text("Save time") }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .disabled(!model.canSave)
                .accessibilityIdentifier(Self.saveIdentifier)
                .reportReminderRendered(Self.saveIdentifier, text: "Save time")
            if model.standing.offersCancel && !model.isClosed {
                Button { Task { await model.cancel() } } label: { Text("Cancel reminder") }
                    .buttonStyle(DSPillButtonStyle(style: .destructive))
                    .disabled(!model.canCancel)
                    .accessibilityIdentifier(Self.cancelIdentifier)
                    .reportReminderRendered(Self.cancelIdentifier, text: "Cancel reminder")
            }
        }
    }

    /// Every known zone, with the one in use first so a stored zone the system list lacks is still selectable.
    private var zoneChoices: [String] {
        let known = TimeZone.knownTimeZoneIdentifiers
        return known.contains(model.draftTimeZoneID) ? known : [model.draftTimeZoneID] + known
    }
}

private struct ReminderRenderedReport: ViewModifier {
    let identifier: String
    let text: String

    func body(content: Content) -> some View {
        content.background(
            GeometryReader { proxy in
                Color.clear.preference(
                    key: ReminderEditSection.RenderedKey.self,
                    value: proxy.size.width > 0 && proxy.size.height > 0 ? [identifier: text] : [:])
            })
    }
}

extension View {
    fileprivate func reportReminderRendered(_ identifier: String, text: String) -> some View {
        modifier(ReminderRenderedReport(identifier: identifier, text: text))
    }
}

/// The recorded events of a reminder, newest first, as `ReminderHistoryModel` words them.
struct ReminderHistoryList: View {
    @ObservedObject var model: ReminderHistoryModel

    /// Names what this list holds, so it is not confused with the item's own history above it on detail.
    static let headingText = "Reminder history"

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            Text(Self.headingText)
                .font(DSFont.body)
                .foregroundColor(DSColor.ink.color)
                .accessibilityAddTraits(.isHeader)
            if let unavailable = model.unavailable {
                note(unavailable == .locked ? ReminderHistoryText.lockedText : ReminderHistoryText.unreadableText)
            } else if model.rows.isEmpty {
                note(ReminderHistoryText.emptyText)
            } else {
                ForEach(model.summaryLines, id: \.self) { note($0) }
                ForEach(model.rows) { row in
                    VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                        Text(row.occurred.isEmpty ? row.title : "\(row.title) · \(row.occurred)")
                            .font(DSFont.footer)
                            .foregroundColor(DSColor.ink.color)
                            .reportReminderRendered(
                                Self.lineIdentifier(row.occurred.isEmpty ? row.title : "\(row.title) · \(row.occurred)"),
                                text: row.occurred.isEmpty ? row.title : "\(row.title) · \(row.occurred)")
                        if let detail = row.detail, !detail.isEmpty { note(detail) }
                    }
                    .accessibilityElement(children: .combine)
                }
                if model.isTruncated { note(ReminderHistoryText.truncatedText) }
            }
        }
        .accessibilityIdentifier(ReminderEditSection.historyIdentifier)
    }

    /// Each visible line reports itself under this prefix so a hosted test can read what is rendered.
    static let linePrefix = "reminder.history.line:"
    private static func lineIdentifier(_ text: String) -> String { linePrefix + text }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .reportReminderRendered(Self.lineIdentifier(text), text: text)
    }
}
