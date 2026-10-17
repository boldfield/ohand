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
    static let loadMoreIdentifier = "retrieval.search.load-more"

    static let promptText = "Search runs on this device. Type words you captured, or choose a date, type or session topic."
    static let emptyText = "Nothing matches. Your captures are saved; try other words or fewer filters."
    static let lockedText = "Your saved items are locked. Go back and open Search again to unlock them."

    static func rowIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row.\(row.id)"
    }

    static func rowTextIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row-text.\(row.id)"
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
    @State private var presentedFilterSheet: RetrievalFilterSheet?

    init(model: RetrievalSearchModel, renderReporter: (([String: RetrievalRenderedElement]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
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
        .sheet(item: $presentedFilterSheet) { sheet in
            RetrievalFilterSheetView(sheet: sheet, model: model)
        }
        .onPreferenceChange(RetrievalRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onAppear {
            model.revalidate()
            queryIsFocused = true
        }
        .onDisappear { model.screenDidDisappear() }
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
                            .font(.system(size: 22))
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
            RetrievalFilterChipLabel(title: model.itemType?.title ?? "Type", isSelected: model.itemType != nil)
        }
        .accessibilityLabel("Type filter")
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
            Button("Choose dates…") { presentedFilterSheet = .dates }
        } label: {
            RetrievalFilterChipLabel(title: timeTitle, isSelected: model.since != nil || model.until != nil)
        }
        .accessibilityLabel("Time filter")
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
            Button("Choose topic…") { presentedFilterSheet = .topic }
        } label: {
            RetrievalFilterChipLabel(title: topicTitle, isSelected: topicIsActive)
        }
        .accessibilityLabel("Topic filter")
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
                RetrievalResultRowView(row: row)
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
private enum RetrievalFilterSheet: String, Identifiable {
    case dates
    case topic

    var id: String { rawValue }
}

/// A filter chip's face, as a 34 pt capsule. Wrapped in a `Menu`, so it is not a `Button` itself.
private struct RetrievalFilterChipLabel: View {
    let title: String
    let isSelected: Bool

    var body: some View {
        HStack(spacing: DSSpacing.xxs) {
            Text(title).lineLimit(1)
            Image(systemName: "chevron.down")
                .font(.system(size: 11, weight: .semibold))
                .accessibilityHidden(true)
        }
        .font(DSFont.chip)
        .foregroundColor((isSelected ? DSColor.onAccent : DSColor.ink).color)
        .padding(.horizontal, DSSpacing.md)
        .frame(height: DSLayout.chipHeight)
        .background(Capsule().fill((isSelected ? DSColor.accent : DSColor.surface2).color))
        .frame(minWidth: DSLayout.minimumTouchTarget, minHeight: DSLayout.minimumTouchTarget)
        .contentShape(Rectangle())
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }
}

/// The date range and the session topic, which need more than a menu item.
private struct RetrievalFilterSheetView: View {
    let sheet: RetrievalFilterSheet
    @ObservedObject var model: RetrievalSearchModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.md) {
            switch sheet {
            case .dates:
                Text("Dates").font(DSFont.sectionTitle).foregroundColor(DSColor.ink.color)
                OptionalDayPicker(title: "Since", day: $model.since)
                OptionalDayPicker(title: "Until", day: $model.until)
            case .topic:
                Text("Session topic").font(DSFont.sectionTitle).foregroundColor(DSColor.ink.color)
                TextField("Session topic", text: $model.sessionTopic)
                    .font(DSFont.body)
                    .padding(DSSpacing.sm)
                    .background(
                        RoundedRectangle(cornerRadius: DSRadius.field, style: .continuous)
                            .fill(DSColor.surface2.color))
                    .submitLabel(.done)
                    .onSubmit { dismiss() }
                    .accessibilityIdentifier(RetrievalSearchView.topicIdentifier)
                Toggle("Only items with a session topic", isOn: $model.onlyWithSessionTopic)
                    .tint(DSColor.accent.color)
            }
            Spacer(minLength: 0)
            DSPillButton(title: "Done", style: .primary) { dismiss() }
                .frame(maxWidth: .infinity)
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

    var body: some View {
        Toggle(
            title,
            isOn: Binding(
                get: { day != nil },
                set: { day = $0 ? (day ?? Date()) : nil }))
        if let current = day {
            DatePicker(
                "\(title) date",
                selection: Binding(get: { current }, set: { day = $0 }),
                displayedComponents: .date)
        }
    }
}

struct RetrievalResultRowView: View {
    let row: RetrievalResultRow

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
                    Text("Original: \(original)")
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

    /// The text with the matched words bold on a `tint` highlight.
    private var snippetText: Text {
        let snippet = row.snippet
        guard !snippet.match.isEmpty else { return Text(row.text) }
        var matched = AttributedString(snippet.match)
        matched.backgroundColor = DSColor.tint.color
        matched.inlinePresentationIntent = .stronglyEmphasized
        return Text(AttributedString(snippet.before) + matched + AttributedString(snippet.after))
    }

    /// The line under a result: when it was captured, its type, whether the words are original or corrected, and scope.
    static func detailsText(for row: RetrievalResultRow) -> String {
        [
            row.capturedAt.formatted(date: .abbreviated, time: .shortened), row.typeTitle, row.basisTitle, row.scopeName,
        ].joined(separator: " · ")
    }
}
