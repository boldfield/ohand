import SwiftUI

/// The authenticated source-detail screen: where the item's words came from, the original and corrected text, and
/// playback when the audio is retained. Everything it shows comes from `SourceDetailModel`.
struct SourceDetailView: View {
    static let loadingIdentifier = "source-detail.loading"
    static let lockedIdentifier = "source-detail.locked"
    static let notFoundIdentifier = "source-detail.not-found"
    static let unreadableIdentifier = "source-detail.unreadable"
    static let currentTextIdentifier = "source-detail.current-text"
    static let noTextIdentifier = "source-detail.no-text"
    static let playButtonIdentifier = "source-detail.audio.play"
    static let pauseButtonIdentifier = "source-detail.audio.pause"
    static let audioNoticeIdentifier = "source-detail.audio.notice"

    static let lockedText = "Your saved items are locked. Go back and open the item again to unlock it."
    static let notFoundText = "This item is not available. It may have been deleted."
    static let unreadableText = "This item could not be read. Try again later."
    static let noTextText = "There is no text for this item yet."

    static func sectionIdentifier(_ kind: SourceDetailTextSection.Kind) -> String {
        "source-detail.section.\(kind)"
    }

    /// What the audio control says when it is off, or nil when there is nothing to say.
    static func audioNotice(for state: SourceAudioState) -> String? {
        switch state {
        case .none, .available, .playing, .paused: return nil
        case .removed: return "The audio for this item is no longer kept, so it cannot be played."
        case .unplayable: return "The audio for this item cannot be played right now."
        case .locked: return "Playback stopped because your saved items locked."
        }
    }

    static func sectionTitle(_ kind: SourceDetailTextSection.Kind) -> String {
        switch kind {
        case .original: return "Original text"
        case .transcript: return "Transcript of the audio"
        case .correction: return "Your correction"
        }
    }

    @ObservedObject var model: SourceDetailModel
    /// Told when an identified element comes on or goes off the screen; tests use it to check what is rendered.
    var renderReporter: ((String, Bool) -> Void)?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let content = model.content {
                    details(content)
                    textSections(content)
                    audioControl
                    history(content)
                } else if let failure = model.failure {
                    notice(Self.text(for: failure), identifier: Self.identifier(for: failure))
                } else {
                    notice("Loading…", identifier: Self.loadingIdentifier)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
        }
        .navigationTitle("Item")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.load() }
        .onDisappear { model.close() }
        .task {
            // Watches for a relock, deletion or the clip ending for as long as the screen is shown.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 500_000_000)
                model.checkSession()
            }
        }
    }

    private static func text(for failure: SourceDetailFailure) -> String {
        switch failure {
        case .locked: return lockedText
        case .notFound: return notFoundText
        case .unreadable: return unreadableText
        }
    }

    private static func identifier(for failure: SourceDetailFailure) -> String {
        switch failure {
        case .locked: return lockedIdentifier
        case .notFound: return notFoundIdentifier
        case .unreadable: return unreadableIdentifier
        }
    }

    private func notice(_ text: String, identifier: String) -> some View {
        Text(text)
            .font(.body)
            .foregroundColor(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            .reported(identifier, renderReporter)
    }

    private func details(_ content: SourceDetailContent) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let capturedAt = content.capturedAt {
                Text("Captured \(capturedAt.formatted(date: .abbreviated, time: .shortened))")
            }
            Text([content.typeLabel, content.scopeName, content.lifecycleLabel].compactMap { $0 }.joined(separator: " · "))
            if let topic = content.sessionTopic { Text("Session topic: \(topic)") }
        }
        .font(.subheadline)
        .foregroundColor(.secondary)
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder private func textSections(_ content: SourceDetailContent) -> some View {
        if content.sections.isEmpty {
            notice(Self.noTextText, identifier: Self.noTextIdentifier)
        } else {
            if let current = content.currentText {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Current text").font(.headline)
                    Text(current).fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .combine)
                .reported(Self.currentTextIdentifier, renderReporter)
            }
            ForEach(content.sections) { section in
                VStack(alignment: .leading, spacing: 4) {
                    Text(Self.sectionTitle(section.kind)).font(.headline)
                    Text(section.text).fixedSize(horizontal: false, vertical: true)
                    if let provenance = section.provenance {
                        Text(provenance).font(.footnote).foregroundColor(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
                .reported(Self.sectionIdentifier(section.kind), renderReporter)
            }
        }
    }

    @ViewBuilder private var audioControl: some View {
        switch model.audio {
        case .none:
            EmptyView()
        case .available, .paused:
            Button { Task { await model.play() } } label: { Label("Play audio", systemImage: "play.fill") }
                .reported(Self.playButtonIdentifier, renderReporter)
        case .playing:
            Button { model.pause() } label: { Label("Pause audio", systemImage: "pause.fill") }
                .reported(Self.pauseButtonIdentifier, renderReporter)
        case .removed, .unplayable, .locked:
            if let text = Self.audioNotice(for: model.audio) {
                Label(text, systemImage: "speaker.slash")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .reported(Self.audioNoticeIdentifier, renderReporter)
            }
        }
    }

    @ViewBuilder private func history(_ content: SourceDetailContent) -> some View {
        if !content.history.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("History").font(.headline)
                ForEach(content.history) { row in
                    Text(row.happenedAt.map { "\(row.summary) · \($0.formatted(date: .abbreviated, time: .shortened))" } ?? row.summary)
                        .font(.footnote)
                }
                if content.historyTruncated {
                    Text("Older changes are not shown.").font(.footnote).foregroundColor(.secondary)
                }
            }
        }
    }
}

private extension View {
    func reported(_ identifier: String, _ reporter: ((String, Bool) -> Void)?) -> some View {
        accessibilityIdentifier(identifier)
            .onAppear { reporter?(identifier, true) }
            .onDisappear { reporter?(identifier, false) }
    }
}

/// Owns the model for one item's screen, so that the host rebuilding its views does not start the read over.
struct SourceDetailScreen: View {
    @StateObject private var model: SourceDetailModel
    private let renderReporter: ((String, Bool) -> Void)?

    init(itemID: String, renderReporter: ((String, Bool) -> Void)? = nil, makeModel: (String) -> SourceDetailModel) {
        self.renderReporter = renderReporter
        let built = makeModel(itemID)
        _model = StateObject(wrappedValue: built)
    }

    var body: some View {
        SourceDetailView(model: model, renderReporter: renderReporter)
    }
}
