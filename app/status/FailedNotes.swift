import Foundation
import OhAndCoreBridge
import OhAndServices

/// The two private calls that name failed sorting and run it again. `JobConfigurationService` is the production
/// implementation: it needs an authenticated read session and answers `notAuthenticated` otherwise.
protocol FailedInterpretationsControlling: AnyObject, Sendable {
    func failedInterpretations(limit: Int?, cursor: String?) async throws -> JobConfigurationFailedInterpretations
    func rerunFailedInterpretations(itemIDs: [String]) async throws -> JobConfigurationRerunResult
}

extension JobConfigurationService: FailedInterpretationsControlling {}

/// Reads the first line of a note's text. `nil` when it cannot be read now (locked, deleted, no text yet).
protocol FailedNoteTextReading: AnyObject, Sendable {
    func firstLine(itemID: String) async -> String?
}

/// The existing private item read, narrowed to one line of text.
final class ItemDetailFirstLineReader: FailedNoteTextReading, @unchecked Sendable {
    private let reader: SourceDetailReading

    init(reader: SourceDetailReading) {
        self.reader = reader
    }

    func firstLine(itemID: String) async -> String? {
        guard let detail = try? await reader.detail(itemID: itemID), let text = detail.text else { return nil }
        let line = text.split(whereSeparator: \.isNewline).first.map { $0.trimmingCharacters(in: .whitespaces) }
        return line?.isEmpty == false ? line : nil
    }
}

/// One note whose sorting failed, as the screens show it. Holds no text; the first line is read separately.
struct FailedNote: Equatable, Identifiable {
    let itemID: String
    let capturedAt: Date?
    /// Fixed copy for the machine label, from `StatusText.failureReason`.
    let reason: String
    let recovery: JobConfigurationRecovery

    var id: String { itemID }

    init(_ record: JobConfigurationFailedInterpretation) {
        itemID = record.itemID
        capturedAt = FailedNote.parse(record.capturedAt)
        reason = StatusText.failureReason(record.failureReason)
        recovery = record.recovery
    }

    private static let plainStamp: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()
    private static let fractionalStamp: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    private static func parse(_ stamp: String) -> Date? {
        plainStamp.date(from: stamp) ?? fractionalStamp.date(from: stamp)
    }
}

/// What a "Try again" for failed notes did, in the two counts the screen reports.
enum FailedNotesRerunSummary: Equatable {
    case done(queued: Int, needsSetting: Int)
    /// Some notes were sent and then a later request did not go through; `notRun` notes were not run again.
    case incomplete(queued: Int, needsSetting: Int, notRun: Int)
    /// The request did not go through; nothing is known to have changed.
    case failed

    var text: String {
        switch self {
        case .done(let queued, let needsSetting): return StatusText.rerunSummary(queued: queued, needsSetting: needsSetting)
        case .incomplete(let queued, let needsSetting, let notRun):
            return StatusText.rerunIncomplete(queued: queued, needsSetting: needsSetting, notRun: notRun)
        case .failed: return StatusText.rerunFailed
        }
    }

    /// True when a setting has to change before some note can run, so the screen links to Processing.
    var needsSetting: Bool {
        switch self {
        case .done(_, let count), .incomplete(_, let count, _): return count > 0
        case .failed: return false
        }
    }

    init(_ result: JobConfigurationRerunResult) {
        self = .done(
            queued: result.moved, needsSetting: result.skippedItems.filter { $0.reason == .noProviderAllowed }.count)
    }
}
