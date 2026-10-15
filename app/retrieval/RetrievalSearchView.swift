import SwiftUI

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

    static func rowIdentifier(_ row: RetrievalResultRow) -> String {
        "retrieval.search.row.\(row.id)"
    }

    @ObservedObject var model: RetrievalSearchModel

    var body: some View {
        List {
            Section("Find") {
                TextField("Words you captured", text: $model.query)
                    .submitLabel(.search)
                    .onSubmit { model.search() }
                    .accessibilityIdentifier(Self.queryIdentifier)
                TextField("Session topic (optional)", text: $model.sessionTopic)
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
                        .frame(maxWidth: .infinity)
                }
                .disabled(model.phase == .searching)
                .accessibilityIdentifier(Self.searchButtonIdentifier)
            }

            if let problem = model.problem {
                Section {
                    Text(problem)
                        .foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier(Self.problemIdentifier)
                }
            }

            resultsSection
        }
        .navigationTitle("Search")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { model.revalidate() }
        .onDisappear { model.clear() }
    }

    @ViewBuilder
    private var resultsSection: some View {
        switch model.phase {
        case .idle:
            Section {
                note("Search runs on this device. Type words you captured, or choose a date, type or session topic.")
                    .accessibilityIdentifier(Self.promptIdentifier)
            }
        case .searching:
            Section {
                if model.rows.isEmpty {
                    ProgressView("Searching").frame(maxWidth: .infinity)
                } else {
                    resultRows
                }
            }
        case .results:
            Section {
                resultRows
            } header: {
                Text("Showing \(model.rows.count) of \(model.totalAccessible)")
                    .accessibilityIdentifier(Self.countIdentifier)
            }
        case .empty:
            Section {
                note("Nothing matches. Your captures are saved; try other words or fewer filters.")
                    .accessibilityIdentifier(Self.emptyIdentifier)
            }
        case .locked:
            Section {
                note("Your saved items are locked. Go back and open Search again to unlock them.")
                    .accessibilityIdentifier(Self.lockedIdentifier)
            }
        case .failed(let failure):
            Section {
                note(failure.message)
                    .accessibilityIdentifier(Self.failureIdentifier)
            }
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
        }
        if model.canLoadMore {
            Button("Load more") { model.loadMore() }
                .disabled(model.phase == .searching)
                .accessibilityIdentifier(Self.loadMoreIdentifier)
        }
    }

    private func note(_ text: String) -> some View {
        Text(text)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
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
            Text(details)
                .font(.caption)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private var snippetText: Text {
        let snippet = row.snippet
        guard !snippet.match.isEmpty else { return Text(row.text) }
        return Text(snippet.before) + Text(snippet.match).bold() + Text(snippet.after)
    }

    private var details: String {
        var parts = [row.capturedAt.formatted(date: .abbreviated, time: .shortened)]
        if let type = row.itemType { parts.append(type.title) }
        parts.append(row.basisTitle)
        parts.append(row.scopeName)
        return parts.joined(separator: " · ")
    }
}
