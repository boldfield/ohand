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
    /// Told where an identified element sits, in window coordinates; layout tests use it to check for clipping.
    var layoutReporter: ((String, CGRect) -> Void)?
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.dismiss) private var dismiss
    /// Beside the search results the pane has no back button of its own and takes the search screen's margins.
    @Environment(\.isInItemPane) private var isInItemPane

    private var sideMargin: CGFloat { isInItemPane ? 0 : DSSpacing.sideMargin(for: horizontalSizeClass) }

    var body: some View {
        VStack(alignment: .leading, spacing: DSSpacing.sm) {
            if !isInItemPane { header }
            ScrollView {
                VStack(alignment: .leading, spacing: DSSpacing.lg) {
                    if let content = model.content {
                        detailCard(content)
                        history(content)
                    } else if let failure = model.failure {
                        emptyState(
                            title: Self.title(for: failure), message: Self.text(for: failure),
                            identifier: Self.identifier(for: failure))
                    } else {
                        ProgressView()
                            .tint(DSColor.accent.color)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, DSSpacing.xl)
                            .accessibilityLabel("Loading")
                            .reported(Self.loadingIdentifier, renderReporter, layoutReporter)
                    }
                    if let deletion = model.deletion {
                        ItemDeletionSection(model: deletion, onRecorded: { Task { await model.load() } })
                    }
                }
                .padding(.bottom, DSSpacing.lg)
            }
        }
        .padding(.horizontal, sideMargin)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(DSColor.surface0.color.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
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

    private var header: some View {
        HStack(spacing: DSSpacing.sm) {
            DSGlassButton(systemName: "chevron.left", accessibilityLabel: "Back") { dismiss() }
            Spacer(minLength: 0)
        }
        .padding(.top, DSSpacing.xs)
    }

    private static func title(for failure: SourceDetailFailure) -> String {
        switch failure {
        case .locked: return "Locked"
        case .notFound: return "Not here"
        case .unreadable: return "Couldn't open"
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

    /// The design's empty-state pattern: the Mark in a faint tint, a serif line and one muted line.
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
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, DSSpacing.xl)
        .accessibilityElement(children: .combine)
        .reported(identifier, renderReporter, layoutReporter)
    }

    /// The item as one card: type chip and date, the words in force in serif, where they came from, then playback.
    private func detailCard(_ content: SourceDetailContent) -> some View {
        DSCard(padding: DSSpacing.md) {
            VStack(alignment: .leading, spacing: DSSpacing.md) {
                details(content)
                if let current = content.currentText {
                    Text(current)
                        .font(DSFont.detailSerif)
                        .foregroundColor(DSColor.ink.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                        .reported(Self.currentTextIdentifier, renderReporter, layoutReporter)
                }
                textSections(content)
                audioControl
            }
        }
    }

    private func details(_ content: SourceDetailContent) -> some View {
        VStack(alignment: .leading, spacing: DSSpacing.xs) {
            // Side by side when they fit; stacked at the largest text sizes so neither is clipped.
            ViewThatFits(in: .horizontal) {
                HStack(spacing: DSSpacing.xs) { typeAndDate(content) }
                VStack(alignment: .leading, spacing: DSSpacing.xs) { typeAndDate(content) }
            }
            Text(
                ([content.scopeName, content.lifecycleLabel] + [content.sessionTopic.map { "Topic: \($0)" }])
                    .compactMap { $0 }.joined(separator: " · "))
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    @ViewBuilder private func typeAndDate(_ content: SourceDetailContent) -> some View {
        if let typeLabel = content.typeLabel {
            DSChip(title: typeLabel)
                .allowsHitTesting(false)
                .accessibilityRemoveTraits(.isButton)
        }
        if let capturedAt = content.capturedAt {
            Text(capturedAt.formatted(date: .abbreviated, time: .shortened))
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
        }
    }

    @ViewBuilder private func textSections(_ content: SourceDetailContent) -> some View {
        if content.sections.isEmpty {
            Text(Self.noTextText)
                .font(DSFont.footer)
                .foregroundColor(DSColor.inkMuted.color)
                .fixedSize(horizontal: false, vertical: true)
                .reported(Self.noTextIdentifier, renderReporter, layoutReporter)
        } else {
            VStack(alignment: .leading, spacing: DSSpacing.xs) {
                Text("What you wrote")
                    .font(DSFont.sectionTitle)
                    .foregroundColor(DSColor.ink.color)
                    .accessibilityAddTraits(.isHeader)
                VStack(alignment: .leading, spacing: DSSpacing.sm) {
                    ForEach(content.sections) { section in
                        VStack(alignment: .leading, spacing: DSSpacing.xxs) {
                            Text(Self.sectionTitle(section.kind))
                                .font(DSFont.groupHeading)
                                .foregroundColor(DSColor.inkMuted.color)
                            Text(section.text)
                                .font(DSFont.body)
                                .foregroundColor(DSColor.ink.color)
                                .fixedSize(horizontal: false, vertical: true)
                            if let provenance = section.provenance {
                                Text(provenance)
                                    .font(DSFont.footer)
                                    .foregroundColor(DSColor.inkMuted.color)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityElement(children: .combine)
                        .reported(Self.sectionIdentifier(section.kind), renderReporter, layoutReporter)
                    }
                }
                .padding(DSSpacing.sm)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: DSRadius.field, style: .continuous)
                        .fill(DSColor.surface2.color))
            }
        }
    }

    @ViewBuilder private var audioControl: some View {
        switch model.audio {
        case .none:
            EmptyView()
        case .available, .paused:
            Button { Task { await model.play() } } label: { Label("Play audio", systemImage: "play.fill") }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .reported(Self.playButtonIdentifier, renderReporter, layoutReporter)
        case .playing:
            Button { model.pause() } label: { Label("Pause audio", systemImage: "pause.fill") }
                .buttonStyle(DSPillButtonStyle(style: .secondary))
                .reported(Self.pauseButtonIdentifier, renderReporter, layoutReporter)
        case .removed, .unplayable, .locked:
            if let text = Self.audioNotice(for: model.audio) {
                HStack(alignment: .top, spacing: DSSpacing.sm) {
                    DSIconTile(systemName: "speaker.slash")
                    Text(text)
                        .font(DSFont.footer)
                        .foregroundColor(DSColor.inkMuted.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .accessibilityElement(children: .combine)
                .reported(Self.audioNoticeIdentifier, renderReporter, layoutReporter)
            }
        }
    }

    @ViewBuilder private func history(_ content: SourceDetailContent) -> some View {
        if !content.history.isEmpty {
            DSGroupedSection(
                title: "History", footer: content.historyTruncated ? "Older changes are not shown." : nil
            ) {
                ForEach(content.history) { row in
                    DSRow(
                        label: row.summary,
                        value: row.happenedAt?.formatted(date: .abbreviated, time: .shortened),
                        showsChevron: false)
                }
            }
        }
    }
}

private extension View {
    func reported(
        _ identifier: String, _ reporter: ((String, Bool) -> Void)?, _ layoutReporter: ((String, CGRect) -> Void)?
    ) -> some View {
        accessibilityIdentifier(identifier)
            .onAppear { reporter?(identifier, true) }
            .onDisappear { reporter?(identifier, false) }
            .background(
                GeometryReader { proxy in
                    Color.clear
                        .onAppear { layoutReporter?(identifier, proxy.frame(in: .global)) }
                        .onChange(of: proxy.frame(in: .global)) { frame in layoutReporter?(identifier, frame) }
                })
    }
}

/// Owns the model for one item's screen, so that the host rebuilding its views does not start the read over.
struct SourceDetailScreen: View {
    @StateObject private var model: SourceDetailModel
    private let renderReporter: ((String, Bool) -> Void)?

    init(
        itemID: String, renderReporter: ((String, Bool) -> Void)? = nil,
        makeModel: @escaping (String) -> SourceDetailModel
    ) {
        self.renderReporter = renderReporter
        // Built only when SwiftUI first creates the state, not each time the host rebuilds this struct.
        _model = StateObject(wrappedValue: makeModel(itemID))
    }

    var body: some View {
        SourceDetailView(model: model, renderReporter: renderReporter)
    }
}
