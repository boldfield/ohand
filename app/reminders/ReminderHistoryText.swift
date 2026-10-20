import Foundation
import OhAndCoreBridge

/// One line of reminder history. Every word is fixed or a stored value; none is composed from the item's content.
struct ReminderHistoryRow: Equatable, Identifiable {
    let id: Int
    let title: String
    let detail: String?
    /// The recorded time, written in UTC so no zone is guessed. Empty when the record's time cannot be read.
    let occurred: String
}

/// Turns the core's recorded history into words that claim only what was recorded.
///
/// * `delivered` and `opened` appear only because the system reported them. No row is made from the clock, so there is
///   never a "fired", "seen" or "missed" line for a time that merely passed.
/// * Acknowledging is the user saying they have seen the reminder. It is not completing the item and says so.
/// * An unknown delivery stays unknown: the summary says nothing was recorded, not that it failed or succeeded.
enum ReminderHistoryText {
    static func rows(for entries: [ReminderHistoryEntryAnswer]) -> [ReminderHistoryRow] {
        entries.enumerated().map { index, entry in
            let (title, detail) = words(for: entry)
            return ReminderHistoryRow(
                id: index, title: title, detail: detail, occurred: instantText(entry.occurredAt))
        }
    }

    private static func words(for entry: ReminderHistoryEntryAnswer) -> (String, String?) {
        switch entry.kind {
        case "time_requested":
            let due = entry.dueAt.map { "For \(instantText($0))." }
            return ("Time saved", [due, installation(entry.installation)].compactMap { $0 }.joined(separator: " "))
        case "repeat_requested":
            return ("Repeat requested", "Repeats are not supported, so nothing was scheduled.")
        case "not_schedulable":
            return ("Not scheduled", "No notification could be requested for this reminder.")
        case "cancelled":
            return ("Cancelled", nativeCancel(entry.nativeCancel))
        case "delivered":
            return ("Delivered", "The system reported the notification as delivered.")
        case "opened":
            return ("Opened", "The system reported the app was opened from the notification.")
        case "acknowledged":
            return ("Acknowledged", "You acknowledged this reminder. The item is not marked complete.")
        default:
            return ("Recorded event", nil)
        }
    }

    private static func installation(_ status: String?) -> String? {
        switch status {
        case "confirmed": return "The system confirmed it queued the notification."
        case "attempted_unconfirmed": return "It was sent to the system, but the system's answer is not confirmed."
        case "rejected": return "The system refused it."
        case "not_attempted": return "It has not been sent to the system yet."
        case "superseded": return "A later time replaced it."
        default: return nil
        }
    }

    private static func nativeCancel(_ status: String?) -> String? {
        switch status {
        case "confirmed": return "The system confirmed the queued notification was removed."
        case "attempted_unconfirmed": return "Removal was requested; the system's answer is not confirmed."
        case "rejected": return "The system refused to remove it."
        case "not_attempted": return "Removal has not been sent to the system yet."
        default: return nil
        }
    }

    /// What the summary says about delivery and acknowledgment, from recorded states only.
    static func summary(_ summary: ReminderHistorySummaryAnswer?) -> [String] {
        guard let summary else { return [] }
        var lines: [String] = []
        switch summary.deliveryState {
        case "delivered": lines.append("The system reported it delivered.")
        case "opened": lines.append("The system reported it opened.")
        default:
            lines.append(
                summary.elapsedWithoutDeliveryEvidence
                    ? "The time has passed. Nothing recorded whether it was shown."
                    : "No delivery has been recorded.")
        }
        lines.append(
            summary.acknowledgmentState == "acknowledged"
                ? "You acknowledged it. The item is not marked complete."
                : "Not acknowledged.")
        return lines
    }

    static let truncatedText = "Older entries are not shown."
    static let emptyText = "No reminder history has been recorded."
    static let lockedText = "Your saved items are locked, so reminder history is hidden."
    static let unreadableText = "Reminder history could not be read. Try again later."

    private static func instantText(_ string: String) -> String {
        guard let instant = ReminderInstant.date(from: string) else { return "" }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.dateFormat = "yyyy-MM-dd HH:mm 'UTC'"
        return formatter.string(from: instant)
    }
}
