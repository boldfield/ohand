import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// Reads one item's recorded reminder history. `ReminderControlService` is the production implementation; its reads
/// need an authenticated session.
protocol ReminderHistoryReading: AnyObject, Sendable {
    func history(itemID: String) async throws -> ReminderHistoryAnswer
}

extension ReminderControlService: ReminderHistoryReading {}

enum ReminderHistoryUnavailable: Equatable {
    case locked
    case unreadable
}

/// The history section's state for one item.
///
/// Rules this type keeps:
/// * It shows what the core recorded and nothing derived from the clock.
/// * A locked answer, or `lock()`, clears every row. Nothing is kept across a lapse, and reading again after one shows
///   the same bounded list as any other read: there is no "while you were away" digest and no replay of old entries as
///   new ones.
/// * A read that is overtaken by a newer read or by a lock is dropped. Confined to the main queue.
final class ReminderHistoryModel: ObservableObject {
    @Published private(set) var rows: [ReminderHistoryRow] = []
    @Published private(set) var summaryLines: [String] = []
    @Published private(set) var isTruncated = false
    @Published private(set) var hasReminder = false
    @Published private(set) var unavailable: ReminderHistoryUnavailable?
    @Published private(set) var isLoading = false

    let itemID: String
    private let reader: ReminderHistoryReading
    private var generation = 0

    init(itemID: String, reader: ReminderHistoryReading) {
        self.itemID = itemID
        self.reader = reader
    }

    @MainActor func load() async {
        generation += 1
        let mine = generation
        isLoading = true
        defer { if mine == generation { isLoading = false } }
        do {
            let answer = try await reader.history(itemID: itemID)
            // The answer must be for this item; anything else is dropped rather than shown.
            guard mine == generation, answer.itemID == itemID else { return }
            unavailable = nil
            hasReminder = answer.reminderID != nil
            rows = ReminderHistoryText.rows(for: answer.entries)
            summaryLines = ReminderHistoryText.summary(answer.summary)
            isTruncated = answer.truncated
        } catch {
            guard mine == generation else { return }
            clear()
            unavailable = (error as? ReminderControlError) == .notAuthenticated ? .locked : .unreadable
        }
    }

    /// Drops what is shown when the read session ends.
    @MainActor func lock() {
        generation += 1
        isLoading = false
        clear()
        unavailable = .locked
    }

    @MainActor private func clear() {
        rows = []
        summaryLines = []
        isTruncated = false
        hasReminder = false
    }
}
