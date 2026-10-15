import Foundation
import OhAndCoreBridge
import OhAndServices

/// The item types the core knows. Raw values are the core's labels.
enum RetrievalItemType: String, CaseIterable, Identifiable, Equatable {
    case broadIntention = "broad_intention"
    case idea
    case note
    case action

    var id: String { rawValue }

    var title: String {
        switch self {
        case .broadIntention: return "Broad intention"
        case .idea: return "Idea"
        case .note: return "Note"
        case .action: return "Action"
        }
    }
}

/// An item identifier that came out of a retrieval answer and was checked on the way in: a UUID, written in the core's
/// canonical lowercase form. Detail navigation receives this and nothing else about the hit, so what it opens is never
/// a string the screen assembled.
struct RetrievalItemReference: Hashable {
    let value: String

    init?(candidate: String) {
        guard let parsed = UUID(uuidString: candidate) else { return nil }
        value = parsed.uuidString.lowercased()
    }
}

/// Why a search could not be shown. None of these carry stored content.
enum RetrievalSearchFailure: Error, Equatable {
    /// The read session is capture-only, so nothing was asked of the core.
    case notAuthenticated
    /// The session relocked while the search ran; the answer was dropped undelivered.
    case relocked
    /// The request was outside what the core accepts.
    case invalidRequest
    /// The core could not answer now; the same search can be tried again.
    case temporarilyUnavailable
    case unavailable
    case unreadable

    init(core failure: CoreFailure) {
        switch failure.code {
        case "read_not_authenticated": self = .notAuthenticated
        case "read_relocked": self = .relocked
        case "invalid_request": self = .invalidRequest
        default:
            switch failure.errorClass {
            case .unauthorized: self = .notAuthenticated
            case .transient: self = .temporarilyUnavailable
            default: self = .unavailable
            }
        }
    }

    var message: String {
        switch self {
        case .notAuthenticated, .relocked:
            return "Your saved items are locked. Open search again to unlock them."
        case .invalidRequest:
            return "That search is not one Oh And can run. Shorten the words or check the dates."
        case .temporarilyUnavailable:
            return "Search could not finish just now. Your captures are safe. Try again."
        case .unavailable:
            return "Search is not available right now. Your captures are safe."
        case .unreadable:
            return "Search returned something Oh And could not read, so nothing is shown."
        }
    }
}

/// What the user asked for. A blank query with filters lists the matching items instead of searching words.
struct RetrievalSearchRequest: Equatable {
    static let maximumQueryBytes = 1024
    static let maximumTopicBytes = 256
    static let pageSize = 25
    static let maximumOffset = 1_000_000

    var query = ""
    var sessionTopic = ""
    var onlyWithSessionTopic = false
    /// Calendar days, both inclusive. The day is read in the calendar passed to `privateFilters`.
    var since: Date?
    var until: Date?
    var itemType: RetrievalItemType?
    var offset = 0

    var trimmedQuery: String { query.trimmingCharacters(in: .whitespacesAndNewlines) }
    var trimmedSessionTopic: String { sessionTopic.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// `false` when there is nothing to search for or to filter by; the screen then asks instead of listing everything.
    var hasCriteria: Bool {
        !trimmedQuery.isEmpty || !trimmedSessionTopic.isEmpty || onlyWithSessionTopic || since != nil || until != nil
            || itemType != nil
    }

    var isWordSearch: Bool { !trimmedQuery.isEmpty }

    /// The first problem with the request that the core would refuse, as text for the user; `nil` when it can be sent.
    func problem(calendar: Calendar) -> String? {
        if !hasCriteria { return "Enter words to find, or choose a filter." }
        if trimmedQuery.utf8.count > Self.maximumQueryBytes { return "That search is too long. Shorten it." }
        if trimmedSessionTopic.utf8.count > Self.maximumTopicBytes { return "That session topic is too long." }
        if offset < 0 || offset > Self.maximumOffset { return "There are no more results to load." }
        if let range = dateRange(calendar: calendar), let start = range.start, let end = range.end, start > end {
            return "The start date is after the end date."
        }
        return nil
    }

    /// The filters for the gated read. It names no scope: the gate applies the session's own.
    func privateFilters(calendar: Calendar) -> PrivateRetrievalFilters {
        let range = dateRange(calendar: calendar)
        let topic = trimmedSessionTopic
        return PrivateRetrievalFilters(
            scopes: nil,
            itemTypes: itemType.map { [$0.rawValue] } ?? [],
            sessionTopics: topic.isEmpty ? [] : [topic],
            requireSessionTopic: onlyWithSessionTopic,
            capturedAfter: range?.start.map(Self.instantText),
            capturedBefore: range?.end.map(Self.instantText),
            limit: Self.pageSize,
            offset: offset)
    }

    private struct DateRange {
        let start: Date?
        let end: Date?
    }

    private func dateRange(calendar: Calendar) -> DateRange? {
        guard since != nil || until != nil else { return nil }
        let start = since.map { calendar.startOfDay(for: $0) }
        let end = until.flatMap { day -> Date? in
            let dayStart = calendar.startOfDay(for: day)
            return calendar.date(byAdding: .day, value: 1, to: dayStart)?.addingTimeInterval(-0.001)
        }
        return DateRange(start: start, end: end)
    }

    private static func instantText(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }
}

/// A hit's text split around the first matched word, for bold display. Offsets in the core's answer are Unicode scalar
/// positions in the current text.
struct RetrievalSnippet: Equatable {
    static let leadingContextScalars = 60

    let before: String
    let match: String
    let after: String

    init(text: String, span: CoreRetrievalMatchSpan?) {
        let scalars = text.unicodeScalars
        guard let span, span.start >= 0, span.end > span.start, span.end <= scalars.count else {
            before = text
            match = ""
            after = ""
            return
        }
        let start = scalars.index(scalars.startIndex, offsetBy: span.start)
        let end = scalars.index(scalars.startIndex, offsetBy: span.end)
        var leading = String(scalars[scalars.startIndex..<start])
        if leading.unicodeScalars.count > Self.leadingContextScalars {
            leading = "…" + String(leading.unicodeScalars.suffix(Self.leadingContextScalars))
        }
        before = leading
        match = String(scalars[start..<end])
        after = String(scalars[end..<scalars.endIndex])
    }
}

/// One result row, built only from an answer the gate admitted.
struct RetrievalResultRow: Identifiable, Equatable {
    var id: String { reference.value }

    let reference: RetrievalItemReference
    /// The current text: the correction when there is one.
    let text: String
    /// The words as captured; set only when `basis` is `.corrected`.
    let originalText: String?
    let basis: CoreRetrievalTextBasis
    /// `.supersededOriginal` means only the replaced original matched the words.
    let matched: CoreRetrievalMatchedText
    let snippet: RetrievalSnippet
    let scopeName: String
    let capturedAt: Date
    /// Known only when the request filtered to one type: a hit does not carry its item type.
    let itemType: RetrievalItemType?

    var basisTitle: String {
        switch basis {
        case .original: return "Original words"
        case .transcribed: return "Transcribed"
        case .corrected: return "Corrected"
        }
    }
}

/// One page of results.
struct RetrievalPage: Equatable {
    let totalAccessible: Int
    let rows: [RetrievalResultRow]
    let offset: Int

    var hasMore: Bool { offset + rows.count < totalAccessible && !rows.isEmpty }

    /// Maps an admitted answer. A hit whose identifier or date is malformed makes the whole page unreadable rather than
    /// showing a partial list.
    static func make(from answer: PrivateRetrievalAnswer, for request: RetrievalSearchRequest) throws -> RetrievalPage {
        var rows: [RetrievalResultRow] = []
        rows.reserveCapacity(answer.hits.count)
        for hit in answer.hits {
            guard let reference = RetrievalItemReference(candidate: hit.itemID),
                let capturedAt = RetrievalInstant.parse(hit.capturedAt)
            else { throw RetrievalSearchFailure.unreadable }
            rows.append(
                RetrievalResultRow(
                    reference: reference,
                    text: hit.text,
                    originalText: hit.textBasis == .corrected ? hit.originalText : nil,
                    basis: hit.textBasis,
                    matched: hit.matched,
                    snippet: RetrievalSnippet(text: hit.text, span: hit.matched == .current ? hit.matchSpan : nil),
                    scopeName: hit.scope == "work" ? "Work" : "Personal",
                    capturedAt: capturedAt,
                    itemType: request.itemType))
        }
        return RetrievalPage(totalAccessible: answer.totalAccessible, rows: rows, offset: request.offset)
    }
}

/// Reads the core's stored RFC 3339 instants, with or without fractional seconds of any length.
enum RetrievalInstant {
    static func parse(_ text: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        guard let fractionRange = text.range(of: #"\.\d+"#, options: .regularExpression) else {
            return formatter.date(from: text)
        }
        let fraction = Double("0" + String(text[fractionRange]))
        var whole = text
        whole.removeSubrange(fractionRange)
        guard let base = formatter.date(from: whole), let fraction else { return nil }
        return base.addingTimeInterval(fraction)
    }
}
