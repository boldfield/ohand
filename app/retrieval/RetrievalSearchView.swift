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

    @ObservedObject var model: RetrievalSearchModel
    var renderReporter: (([String: RetrievalRenderedElement]) -> Void)?

    init(model: RetrievalSearchModel, renderReporter: (([String: RetrievalRenderedElement]) -> Void)? = nil) {
        self.model = model
        self.renderReporter = renderReporter
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                findSection
                if let problem = model.problem {
                    note(problem, identifier: Self.problemIdentifier)
                }
                resultsSection
            }
            .padding()
        }
        .navigationTitle("Search")
        .navigationBarTitleDisplayMode(.inline)
        .onPreferenceChange(RetrievalRenderedKey.self) { elements in
            renderReporter?(elements)
        }
        .onAppear { model.revalidate() }
        .onDisappear { model.screenDidDisappear() }
    }

    private var findSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Find").font(.headline)
            TextField("Words you captured", text: $model.query)
                .textFieldStyle(.roundedBorder)
                .submitLabel(.search)
                .onSubmit { model.search() }
                .accessibilityIdentifier(Self.queryIdentifier)
            TextField("Session topic (optional)", text: $model.sessionTopic)
                .textFieldStyle(.roundedBorder)
                .submitLabel(.search)
                .onSubmit { model.search() }
                .accessibilityIdentifier(Self.topicIdentifier)
            Toggle("Only items with a session topic", isOn: $model.onlyWithSessionTopic)
            OptionalDayPicker(title: "Since", day: $model.since)
            OptionalDayPicker(title: "Until", day: $model.until)
            Picker("Type", selection: $model.itemType) {
                Text("Any type").tag(RetrievalItemType?.none)
                ForEach(RetrievalItemType.allCases) { type in
                    Text(type.title).tag(RetrievalItemType?.some(type))
                }
            }
            Button {
                model.search()
            } label: {
                Label("Search", systemImage: "magnifyingglass")
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(model.phase == .searching)
            .accessibilityIdentifier(Self.searchButtonIdentifier)
            .reportRendered(Self.searchButtonIdentifier, text: "Search")
        }
    }

    @ViewBuilder
    private var resultsSection: some View {
        switch model.phase {
        case .idle:
            note(Self.promptText, identifier: Self.promptIdentifier)
        case .searching:
            if model.rows.isEmpty {
                ProgressView("Searching").frame(maxWidth: .infinity)
            } else {
                resultRows
            }
        case .results:
            Text("Showing \(model.rows.count) of \(model.totalAccessible)")
                .font(.headline)
                .accessibilityIdentifier(Self.countIdentifier)
                .reportRendered(Self.countIdentifier, text: "Showing \(model.rows.count) of \(model.totalAccessible)")
            resultRows
        case .empty:
            note(Self.emptyText, identifier: Self.emptyIdentifier)
        case .locked:
            note(Self.lockedText, identifier: Self.lockedIdentifier)
        case .failed(let failure):
            note(failure.message, identifier: Self.failureIdentifier)
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
            Divider()
        }
        if model.canLoadMore {
            Button("Load more") { model.loadMore() }
                .frame(maxWidth: .infinity, minHeight: 44)
                .disabled(model.phase == .searching)
                .accessibilityIdentifier(Self.loadMoreIdentifier)
                .reportRendered(Self.loadMoreIdentifier, text: "Load more")
        }
    }

    private func note(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(identifier)
            .reportRendered(identifier, text: text)
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
        VStack(alignment: .leading, spacing: 4) {
            snippetText
                .lineLimit(4)
                .fixedSize(horizontal: false, vertical: true)
                .reportRendered(RetrievalSearchView.rowTextIdentifier(row), text: row.text)
            if let original = row.originalText {
                Text("Original: \(original)")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if row.matched == .supersededOriginal {
                Text("Matched the original words, which a correction replaced.")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text(Self.detailsText(for: row))
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .reportRendered(RetrievalSearchView.rowDetailsIdentifier(row), text: Self.detailsText(for: row))
        }
        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        .contentShape(Rectangle())
    }

    private var snippetText: Text {
        let snippet = row.snippet
        guard !snippet.match.isEmpty else { return Text(row.text) }
        return Text(snippet.before) + Text(snippet.match).bold() + Text(snippet.after)
    }

    /// The line under a result: when it was captured, its type, whether the words are original or corrected, and scope.
    static func detailsText(for row: RetrievalResultRow) -> String {
        [
            row.capturedAt.formatted(date: .abbreviated, time: .shortened), row.typeTitle, row.basisTitle, row.scopeName,
        ].joined(separator: " · ")
    }
}
