import SwiftUI

/// Where an element of the search screen landed and the words it shows, so a hosted test can see what is on screen.
struct RetrievalRenderedElement: Equatable {
    let frame: CGRect
    let text: String
}

private struct RetrievalRenderedKey: PreferenceKey {
    static var defaultValue: [String: RetrievalRenderedElement] = [:]

    static func reduce(value: inout [String: RetrievalRenderedElement], nextValue: () -> [String: RetrievalRenderedElement]) {
        value.merge(nextValue()) { _, latest in latest }
    }
}

private extension View {
    func reportRendered(_ identifier: String, text: String) -> some View {
        background(GeometryReader { proxy in
            Color.clear.preference(
                key: RetrievalRenderedKey.self,
                value: [identifier: RetrievalRenderedElement(frame: proxy.frame(in: .global), text: text)])
        })
    }
}

/// The search screen: words, optional date, type and session-topic filters, and the results. Everything it shows comes
/// from `RetrievalSearchModel`, which only fills in while the read session is authenticated.
struct RetrievalSearchView: View {
    static let queryIdentifier = "retrieval.search.query"
    static let topicIdentifier = "retrieval.search.topic"
    static let searchButtonIdentifier = "retrieval.search.submit"
    static let emptyIdentifier = "retrieval.search.empty"
    static let promptIdentifier = "retrieval.search.prompt"
    static let lockedIdentifier = "retrieval.search.locked"
    static let failureIdentifier = "retrieval.search.failure"
    static let problemIdentifier = "retrieval.search.problem"
    static let countIdentifier = "retrieval.search.count"
    static let typeChipIdentifier = "retrieval.search.chip.type"
    static let timeChipIdentifier = "retrieval.search.chip.time"
    static let topicChipIdentifier = "retrieval.search.chip.topic"
    static let loadMoreIdentifier = "retrieval.search.load-more"
    static let filterSheetTitleIdentifier = "retrieval.search.sheet.title"
    static let sinceToggleIdentifier = "retrieval.search.sheet.since"
    static let untilToggleIdentifier = "retrieval.search.sheet.until"
    static let sinceDateIdentifier = "retrieval.search.sheet.since-date"
    static let untilDateIdentifier = "retrieval.search.sheet.until-date"
    static let onlyWithTopicIdentifier = "retrieval.search.sheet.only-with-topic"
    static let sheetDoneIdentifier = "retrieval.search.sheet.done"

    static let promptText = "Search runs on this device. Type words you captured, or choose a date, type or session topic."
    static let emptyText = "Nothing matches. Your captures are saved; try other words or fewer filters."
    static let lockedText = "Your saved items are locked. Go back and open Search again to unlock them."

    static func rowIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row.\(row.id)"
    }

    static func rowTextIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row-text.\(row.id)"
    }

    static func rowOriginalIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row-original.\(row.id)"
    }

    static func rowDetailsIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row-details.\(row.id)"
    }

    /// The width of the results column when the item pane sits beside it.
    static let regularResultsColumnWidth: CGFloat = 274

    @ObservedObject var model: RetrievalSearchModel
    var renderReporter: (([String: RetrievalRenderedElement]) -> Void)?
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.dismiss) private var dismiss
    @FocusState private var queryIsFocused: Bool
    @StateObject private var filterPresentation: RetrievalFilterPresentation
    /// What the screen and an open filter sheet report; a sheet is its own hierarchy, so its parts arrive separately.
    @State private var screenElements: [String: RetrievalRenderedElement] = [:]
    @State private var sheetElements: [String: RetrievalRenderedElement] = [:]
    /// The words of the search that produced the rows on screen, used to show which words of a replaced original matched.
    @State private var searchedQuery = ""

    init(
        model: RetrievalSearchModel, filterPresentation: RetrievalFilterPresentation? = nil,
        renderReporter: (([String: RetrievalRenderedElement]) -> Void)? = nil
    ) {
        self.model = model
        self.renderReporter = renderReporter
        _filterPresentation = StateObject(wrappedValue: filterPresentation ?? RetrievalFilterPresentation())
    }

    private var sideMargin: CGFloat { DSSpacing.sideMargin(for: horizontalSizeClass) }

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.sm) {
            header
            filterChips
            if horizontalSizeClass == .regular {
                HStack(alignment: .top, spacing: DSSpacing.lg) {
                    resultsScroll
                        .frame(width: Self.regularResultsColumnWidth)
                    itemPanePlaceholder
                }
            } else {
                resultsScroll
            }
        }
        .padding(.horizontal, sideMargin)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(DSColor.surface0.color.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .sheet(item: $filterPresentation.sheet) { sheet in
            RetrievalFilterSheetView(sheet: sheet, model: model)
                .onPreferenceChange(RetrievalRenderedKey.self) { elements in
                    sheetElements = elements
                    renderReporter?(screenElements.merging(elements) { _, sheetElement in sheetElement })
                }
                .onDisappear {
                    sheetElements = [:]
                    renderReporter?(screenElements)
                }
        }
        .onPreferenceChange(RetrievalRenderedKey.self) { elements in
            screenElements = elements
            renderReporter?(elements.merging(sheetElements) { _, sheetElement in sheetElement })
        }
        .onAppear {
            model.revalidate()
            queryIsFocused = true
        }
        .onDisappear { model.screenDidDisappear() }
        .onReceive(model.$phase) { phase in
            // A fresh search clears the rows; loading another page keeps them and runs the earlier words, not the draft.
            if phase == .searching, model.rows.isEmpty { searchedQuery = model.query }
        }
    }

    /// Room for the item detail on the right at regular width; the detail card fills it.
    private var itemPanePlaceholder: some View {
        Color.clear
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityHidden(true)
    }

    private var header: some View {
        HStack(spacing: DSSpacing.sm) {
            DSGlassButton(systemName: "chevron.left", accessibilityLabel: "Back") { dismiss() }
            DSGlassPill {
                HStack(spacing: DSSpacing.xs) {
                    Image(systemName: "magnifyingglass")
                        .foregroundColor(DSColor.inkMuted.color)
                        .accessibilityHidden(true)
                    TextField("Words you captured", text: $model.query)
                        .font(DSFont.body)
                        .foregroundColor(DSColor.ink.color)
                        .focused($queryIsFocused)
                        .submitLabel(.search)
                        .onSubmit { model.search() }
                        .accessibilityIdentifier(Self.queryIdentifier)
                    Button {
                        model.search()
                    } label: {
                        Image(systemName: "arrow.right.circle.fill")
                            .font(DSFont.body)
                            .imageScale(.large)
                            .foregroundColor(DSColor.accent.color)
                            .frame(minWidth: DSLayout.minimumTouchTarget, minHeight: DSLayout.minimumTouchTarget)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(model.phase == .searching)
                    .accessibilityLabel("Search")
                    .accessibilityIdentifier(Self.searchButtonIdentifier)
                    .reportRendered(Self.searchButtonIdentifier, text: "Search")
                }
            }
        }
        .padding(.top, DSSpacing.xs)
    }

    private var filterChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: DSSpacing.xs) {
                typeChip
                timeChip
                topicChip
            }
        }
    }

    private var typeChip: some View {
        Menu {
            Picker("Type", selection: $model.itemType) {
                Text("Any type").tag(RetrievalItemType?.none)
                ForEach(RetrievalItemType.allCases) { type in
                    Text(type.title).tag(RetrievalItemType?.some(type))
                }
            }
        } label: {
            menuChipFace(title: model.itemType?.title ?? "Type", isSelected: model.itemType != nil)
        }
        .accessibilityLabel("Type filter")
        .accessibilityValue(model.itemType != nil ? model.itemType?.title ?? "Type" : "Any")
        .accessibilityAddTraits(model.itemType != nil ? .isSelected : [])
        .reportRendered(Self.typeChipIdentifier, text: model.itemType?.title ?? "Type")
    }

    private var timeChip: some View {
        Menu {
            Button("Any time") {
                model.since = nil
                model.until = nil
            }
            Button("Today") {
                model.since = Date()
                model.until = Date()
            }
            Button("Past 7 days") { setSinceDaysAgo(6) }
            Button("Past 30 days") { setSinceDaysAgo(29) }
            Button("Choose dates…") { filterPresentation.sheet = .dates }
        } label: {
            menuChipFace(title: timeTitle, isSelected: model.since != nil || model.until != nil)
        }
        .accessibilityLabel("Time filter")
        .accessibilityValue(model.since != nil || model.until != nil ? timeTitle : "Any")
        .accessibilityAddTraits(model.since != nil || model.until != nil ? .isSelected : [])
        .reportRendered(Self.timeChipIdentifier, text: timeTitle)
    }

    private var topicChip: some View {
        Menu {
            Button("Any topic") {
                model.sessionTopic = ""
                model.onlyWithSessionTopic = false
            }
            Button("Has a session topic") {
                model.sessionTopic = ""
                model.onlyWithSessionTopic = true
            }
            Button("Choose topic…") { filterPresentation.sheet = .topic }
        } label: {
            menuChipFace(title: topicTitle, isSelected: topicIsActive)
        }
        .accessibilityLabel("Topic filter")
        .accessibilityValue(topicIsActive ? topicTitle : "Any")
        .accessibilityAddTraits(topicIsActive ? .isSelected : [])
        .reportRendered(Self.topicChipIdentifier, text: topicTitle)
    }

    /// The design-system Chip as a menu's label: the menu takes the tap, so the chip's own button is not hit-tested.
    private func menuChipFace(title: String, isSelected: Bool) -> some View {
        DSChip(title: title, isSelected: isSelected)
            .allowsHitTesting(false)
    }

    private func setSinceDaysAgo(_ days: Int) {
        model.since = Calendar.current.date(byAdding: .day, value: -days, to: Date())
        model.until = nil
    }

    private var timeTitle: String {
        switch (model.since, model.until) {
        case (nil, nil): return "Time"
        case (let since?, nil): return "From \(since.formatted(date: .abbreviated, time: .omitted))"
        case (nil, let until?): return "Until \(until.formatted(date: .abbreviated, time: .omitted))"
        case (let since?, let until?):
            return "\(since.formatted(date: .abbreviated, time: .omitted)) – \(until.formatted(date: .abbreviated, time: .omitted))"
        }
    }

    private var topicIsActive: Bool {
        model.onlyWithSessionTopic || !model.sessionTopic.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private var topicTitle: String {
        let topic = model.sessionTopic.trimmingCharacters(in: .whitespaces)
        if !topic.isEmpty { return topic }
        return model.onlyWithSessionTopic ? "Has a topic" : "Topic"
    }

    private var resultsScroll: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: DSSpacing.sm) {
                if let problem = model.problem {
                    note(problem, identifier: Self.problemIdentifier)
                }
                resultsSection
            }
            .padding(.bottom, DSSpacing.lg)
        }
    }

    @ViewBuilder
    private var resultsSection: some View {
        switch model.phase {
        case .idle:
            emptyState(title: "Find a thought", message: Self.promptText, identifier: Self.promptIdentifier)
        case .searching:
            if model.rows.isEmpty {
                ProgressView("Searching").frame(maxWidth: .infinity)
            } else {
                resultRows
            }
        case .results:
            let countText = model.totalAccessible == 1 ? "1 result" : "\(model.totalAccessible) results"
            Text(countText)
                .font(DSFont.groupHeading)
                .foregroundColor(DSColor.inkMuted.color)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier(Self.countIdentifier)
                .reportRendered(Self.countIdentifier, text: countText)
            resultRows
        case .empty:
            emptyState(title: "No matches", message: Self.emptyText, identifier: Self.emptyIdentifier)
        case .locked:
            emptyState(title: "Locked", message: Self.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let failure):
            emptyState(title: "Search stopped", message: failure.message, identifier: Self.failureIdentifier)
        }
    }

    @ViewBuilder
    private var resultRows: some View {
        ForEach(model.rows) { row in
            Button {
                model.select(row)
            } label: {
                RetrievalResultRowView(row: row, highlightedQuery: searchedQuery)
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier(Self.rowIdentifier(row))
            .reportRendered(Self.rowIdentifier(row), text: row.text)
        }
        if model.canLoadMore {
            DSPillButton(title: "Load more", style: .secondary) { model.loadMore() }
                .frame(maxWidth: .infinity, minHeight: DSLayout.minimumTouchTarget)
                .disabled(model.phase == .searching)
                .accessibilityIdentifier(Self.loadMoreIdentifier)
                .reportRendered(Self.loadMoreIdentifier, text: "Load more")
        }
    }

    /// The empty-state pattern: the Mark large in a faint tint, a serif line and one muted line.
    private func emptyState(title: String, message: String, identifier: String) -> some View {
        VStack(spacing: DSSpacing.xs) {
            DSMark(size: 88, color: DSColor.tint.color)
            Text(title)
                .font(DSFont.wordmark)
                .foregroundColor(DSColor.ink.color)
            Text(message)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier(identifier)
                .reportRendered(identifier, text: message)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, DSSpacing.xl)
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(DSFont.footer)
            .foregroundColor(DSColor.inkMuted.color)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(identifier)
            .reportRendered(identifier, text: text)
    }
}

/// Which filter sheet is open.
enum RetrievalFilterSheet: String, Identifiable {
    case dates
    case topic

    var id: String { rawValue }

    var title: String {
        switch self {
        case .dates: return "Dates"
        case .topic: return "Session topic"
        }
    }
}

/// The open filter sheet. The time and topic menus set it; a hosted test can hold it to open a sheet the same way.
final class RetrievalFilterPresentation: ObservableObject {
    @Published var sheet: RetrievalFilterSheet?

    init(sheet: RetrievalFilterSheet? = nil) {
        self.sheet = sheet
    }
}

/// The date range and the session topic, which need more than a menu item.
private struct RetrievalFilterSheetView: View {
    let sheet: RetrievalFilterSheet
    @ObservedObject var model: RetrievalSearchModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.md) {
            Text(sheet.title)
                .font(DSFont.sectionTitle)
                .foregroundColor(DSColor.ink.color)
                .accessibilityAddTraits(.isHeader)
                .reportRendered(RetrievalSearchView.filterSheetTitleIdentifier, text: sheet.title)
            switch sheet {
            case .dates:
                OptionalDayPicker(
                    title: "Since", day: $model.since, toggleIdentifier: RetrievalSearchView.sinceToggleIdentifier,
                    dateIdentifier: RetrievalSearchView.sinceDateIdentifier)
                OptionalDayPicker(
                    title: "Until", day: $model.until, toggleIdentifier: RetrievalSearchView.untilToggleIdentifier,
                    dateIdentifier: RetrievalSearchView.untilDateIdentifier)
            case .topic:
                TextField("Session topic", text: $model.sessionTopic)
                    .font(DSFont.body)
                    .padding(DSSpacing.sm)
                    .background(
                        RoundedRectangle(cornerRadius: DSRadius.field, style: .continuous)
                            .fill(DSColor.surface2.color))
                    .submitLabel(.done)
                    .onSubmit { dismiss() }
                    .accessibilityIdentifier(RetrievalSearchView.topicIdentifier)
                    .reportRendered(RetrievalSearchView.topicIdentifier, text: model.sessionTopic)
                Toggle("Only items with a session topic", isOn: $model.onlyWithSessionTopic)
                    .tint(DSColor.accent.color)
                    .reportRendered(
                        RetrievalSearchView.onlyWithTopicIdentifier,
                        text: model.onlyWithSessionTopic ? "Only items with a session topic: on" : "Only items with a session topic: off")
            }
            Spacer(minLength: 0)
            DSPillButton(title: "Done", style: .primary) { dismiss() }
                .frame(maxWidth: .infinity)
                .reportRendered(RetrievalSearchView.sheetDoneIdentifier, text: "Done")
        }
        .padding(DSSpacing.lg)
        .background(DSColor.surface0.color.ignoresSafeArea())
        .presentationDetents([.medium])
    }
}

/// A day filter the user can switch on and off.
private struct OptionalDayPicker: View {
    let title: String
    @Binding var day: Date?
    let toggleIdentifier: String
    let dateIdentifier: String

    var body: some View {
        Toggle(
            title,
            isOn: Binding(
                get: { day != nil },
                set: { day = $0 ? (day ?? Date()) : nil }))
            .tint(DSColor.accent.color)
            .reportRendered(toggleIdentifier, text: day != nil ? "\(title): on" : "\(title): off")
        if let current = day {
            DatePicker(
                "\(title) date",
                selection: Binding(get: { current }, set: { day = $0 }),
                displayedComponents: .date)
                .reportRendered(dateIdentifier, text: current.formatted(date: .abbreviated, time: .omitted))
        }
    }
}

struct RetrievalResultRowView: View {
    let row: RetrievalResultRow
    /// The searched words, to highlight inside a replaced original that the core matched but gave no span for.
    var highlightedQuery = ""

    var body: some View {
        DSCard {
            VStack(alignment: .leading, spacing: DSSpacing.xs) {
                HStack(spacing: DSSpacing.xs) {
                    Circle()
                        .fill(DSColor.accent.color)
                        .frame(width: 8, height: 8)
                        .accessibilityHidden(true)
                    Text(Self.detailsText(for: row))
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .reportRendered(RetrievalSearchView.rowDetailsIdentifier(row), text: Self.detailsText(for: row))
                }
                snippetText
                    .font(DSFont.body)
                    .foregroundColor(DSColor.ink.color)
                    .lineLimit(4)
                    .fixedSize(horizontal: false, vertical: true)
                    .reportRendered(RetrievalSearchView.rowTextIdentifier(row), text: row.text)
                if let original = row.originalText {
                    originalLine(original)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if row.matched == .supersededOriginal {
                    Text("Matched the original words, which a correction replaced.")
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .frame(minHeight: DSLayout.minimumTouchTarget)
        .contentShape(Rectangle())
    }

    private func originalLine(_ original: String) -> some View {
        let attributed = Self.originalAttributed(
            original, highlighting: row.matched == .supersededOriginal ? highlightedQuery : "")
        return Text(attributed)
            .reportRendered(
                RetrievalSearchView.rowOriginalIdentifier(row), text: Self.highlightedWords(in: attributed).joined(separator: "|"))
    }

    /// The text with the matched words bold on a `tint` highlight.
    private var snippetText: Text {
        let snippet = row.snippet
        guard !snippet.match.isEmpty else { return Text(row.text) }
        var matched = AttributedString(snippet.match)
        matched.backgroundColor = DSColor.tint.color
        matched.inlinePresentationIntent = .stronglyEmphasized
        return Text(AttributedString(snippet.before) + matched + AttributedString(snippet.after))
    }

    /// Where the searched words occur in a replaced original; the core reports no span. Like the core's query contract,
    /// each whitespace piece is a phrase prefix: its letter-and-digit tokens must appear as consecutive tokens of the
    /// original, every token but the last whole and the last as a prefix, ignoring case and diacritics.
    static func highlightRanges(in original: String, query: String) -> [Range<String.Index>] {
        let phrases = query.split(whereSeparator: \.isWhitespace).map { piece in
            tokenRanges(in: piece).map { String(query[$0]) }
        }.filter { !$0.isEmpty }
        guard !phrases.isEmpty else { return [] }
        let words = tokenRanges(in: original[...])
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive]
        var highlighted = [Range<String.Index>?](repeating: nil, count: words.count)
        for start in words.indices {
            for phrase in phrases where start + phrase.count <= words.count {
                var spans: [Range<String.Index>] = []
                for (offset, term) in phrase.enumerated() {
                    let word = words[start + offset]
                    if offset == phrase.count - 1 {
                        guard let found = original.range(of: term, options: options.union(.anchored), range: word) else { break }
                        spans.append(found)
                    } else {
                        guard original[word].compare(term, options: options) == .orderedSame else { break }
                        spans.append(word)
                    }
                }
                guard spans.count == phrase.count else { continue }
                for (offset, span) in spans.enumerated() {
                    let index = start + offset
                    if let existing = highlighted[index], existing.upperBound >= span.upperBound { continue }
                    highlighted[index] = span
                }
            }
        }
        return highlighted.compactMap { $0 }
    }

    /// The maximal letter-and-digit runs of the text, the tokens the search index compares.
    private static func tokenRanges(in text: Substring) -> [Range<String.Index>] {
        var tokens: [Range<String.Index>] = []
        var index = text.startIndex
        while index < text.endIndex {
            guard text[index].isLetter || text[index].isNumber else {
                index = text.index(after: index)
                continue
            }
            var tokenEnd = index
            while tokenEnd < text.endIndex, text[tokenEnd].isLetter || text[tokenEnd].isNumber {
                tokenEnd = text.index(after: tokenEnd)
            }
            tokens.append(index..<tokenEnd)
            index = tokenEnd
        }
        return tokens
    }

    /// "Original: …" with each matched word tinted and bold.
    static func originalAttributed(_ original: String, highlighting query: String) -> AttributedString {
        let prefix = "Original: "
        var attributed = AttributedString(prefix + original)
        for found in highlightRanges(in: original, query: query) {
            let lower = attributed.index(
                attributed.startIndex, offsetByCharacters: prefix.count + original.distance(from: original.startIndex, to: found.lowerBound))
            let upper = attributed.index(lower, offsetByCharacters: original.distance(from: found.lowerBound, to: found.upperBound))
            attributed[lower..<upper].backgroundColor = DSColor.tint.color
            attributed[lower..<upper].inlinePresentationIntent = .stronglyEmphasized
        }
        return attributed
    }

    /// The words of the attributed original that carry the highlight.
    static func highlightedWords(in attributed: AttributedString) -> [String] {
        attributed.runs.filter { $0.backgroundColor != nil }.map { String(attributed[$0.range].characters) }
    }

    /// The line under a result: when it was captured, its type, whether the words are original or corrected, and scope.
    static func detailsText(for row: RetrievalResultRow) -> String {
        [
            row.capturedAt.formatted(date: .abbreviated, time: .shortened), row.typeTitle, row.basisTitle, row.scopeName,
        ].joined(separator: " · ")
    }
}
