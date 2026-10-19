import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// Reads one item's detail. `ItemDetailService` is the production implementation: it needs an authenticated read
/// session and discards an answer that arrives after a relock.
protocol SourceDetailReading: AnyObject, Sendable {
    func detail(itemID: String) async throws -> CoreItemDetail
}

extension ItemDetailService: SourceDetailReading {}

/// One open playback of an item's retained audio. `PlaybackSession` is the production implementation. It exposes no
/// path or file name.
protocol SourceAudioSession: AnyObject {
    var endReason: PlaybackEndReason? { get }
    @discardableResult func play() -> Bool
    func pause()
    func close()
}

extension PlaybackSession: SourceAudioSession {}

/// Opens an item's retained audio. `PlaybackService` is the production implementation: it checks the read session,
/// asks the core whether the item still has audio and holds the file in place while it plays.
protocol SourceAudioOpening: AnyObject, Sendable {
    func openAudio(itemID: String) async throws -> SourceAudioSession
    /// Whether the audio can be played now, without opening it or holding the file.
    func audioAvailability(itemID: String) async -> PlaybackAvailability
}

extension PlaybackService: SourceAudioOpening {
    func openAudio(itemID: String) async throws -> SourceAudioSession {
        try await open(itemID: itemID)
    }

    func audioAvailability(itemID: String) async -> PlaybackAvailability {
        await availability(itemID: itemID)
    }
}

/// Why the screen has no text to show.
enum SourceDetailFailure: Equatable {
    /// Reads are not authenticated, or the session relocked. Nothing is shown until the user unlocks again.
    case locked
    /// No readable item has that identifier. An unknown, a deleted and an out-of-scope item are not told apart.
    case notFound
    case unreadable
}

/// What the screen says about the item's audio. Playback is offered only for `available`, `playing` and `paused`.
enum SourceAudioState: Equatable {
    /// The item never had audio: a typed capture, or a voice capture whose audio was never kept.
    case none
    /// Retained audio that can be played.
    case available
    case playing
    case paused
    /// The audio was removed or expired, or the item was deleted. Nothing is played.
    case removed
    /// The core says there is audio but the file is not there, or cannot be played.
    case unplayable
    /// The read session ended while the audio was open or being opened; the control is off until the screen is reopened.
    case locked
}

/// One section of text on the screen, labelled with where the words came from.
struct SourceDetailTextSection: Equatable, Identifiable {
    enum Kind: Equatable {
        /// Words the user typed or the first saved text of the capture.
        case original
        /// A machine transcript of the audio; never presented as the user's words.
        case transcript
        /// The user's own latest correction.
        case correction
    }

    let kind: Kind
    let text: String
    /// Recognizer, language and confidence, for a transcript only.
    let provenance: String?
    var id: Kind { kind }
}

/// One entry in the item's history, as the core recorded it.
struct SourceDetailHistoryRow: Equatable, Identifiable {
    let id: String
    let summary: String
    let happenedAt: Date?
}

/// What the screen shows for a readable item. Every string comes from the core's answer or from fixed labels; nothing
/// is composed from words the item does not hold.
struct SourceDetailContent: Equatable {
    let itemID: String
    let capturedAt: Date?
    let typeLabel: String?
    let scopeName: String
    let sessionTopic: String?
    let lifecycleLabel: String
    /// The words currently in force, which is what search shows. Absent for an audio capture with no transcript yet.
    let currentText: String?
    /// Original and corrected text side by side. Empty when the item holds no text yet.
    let sections: [SourceDetailTextSection]
    let history: [SourceDetailHistoryRow]
    let historyTruncated: Bool
    let hasRetainedAudioReference: Bool
    let isDeleted: Bool
}

/// The source-detail screen's state.
///
/// Rules this type keeps:
/// * Text and playback both go through services that need an authenticated read session. A locked answer clears the
///   text and ends open playback; the host also leaves the screen on a relock.
/// * Playback is offered only when the core's answer carries an audio reference and the item is not deleted. The audio
///   is opened when the user presses play, through `PlaybackService`, which asks the core again and refuses removed,
///   deleted or missing audio. A refusal turns the control off with the reason; it is never left as a button that does
///   nothing.
/// * Original and corrected text are labelled by where they come from. A transcript is machine text, not a quotation of
///   the user. Nothing is generated.
/// Confined to the main queue.
final class SourceDetailModel: ObservableObject {
    @Published private(set) var content: SourceDetailContent?
    @Published private(set) var failure: SourceDetailFailure?
    @Published private(set) var isLoading = false
    @Published private(set) var audio: SourceAudioState = .none

    let itemID: String
    /// The per-item delete control, when the app assembles one.
    let deletion: ItemDeletionModel?
    private let reader: SourceDetailReading
    private let opener: SourceAudioOpening
    private let readScope: () -> CompositionReadScope
    private var session: SourceAudioSession?
    private var loadGeneration = 0
    private var openGeneration = 0
    private let timestampParser: ISO8601DateFormatter
    private let fractionalTimestampParser: ISO8601DateFormatter

    init(
        itemID: String, reader: SourceDetailReading, opener: SourceAudioOpening,
        readScope: @escaping () -> CompositionReadScope = { .authenticated }, deletion: ItemDeletionModel? = nil
    ) {
        self.itemID = itemID
        self.deletion = deletion
        self.reader = reader
        self.opener = opener
        self.readScope = readScope
        timestampParser = ISO8601DateFormatter()
        timestampParser.formatOptions = [.withInternetDateTime]
        fractionalTimestampParser = ISO8601DateFormatter()
        fractionalTimestampParser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    }

    var canPlay: Bool { audio == .available || audio == .paused }
    var canPause: Bool { audio == .playing }

    // MARK: loading

    /// Reads the item. Anything shown earlier is dropped first when the read fails, so stale text never stays.
    @MainActor func load() async {
        loadGeneration += 1
        let generation = loadGeneration
        isLoading = true
        do {
            let detail = try await reader.detail(itemID: itemID)
            guard generation == loadGeneration else { return }
            // Retention removes the file without clearing the core's audio reference, so ask before offering Play.
            var availability: PlaybackAvailability?
            if detail.audioReference != nil, detail.lifecycleState != "deleted", session == nil {
                availability = await opener.audioAvailability(itemID: itemID)
                guard generation == loadGeneration else { return }
            }
            adopt(detail, availability: availability)
        } catch {
            guard generation == loadGeneration else { return }
            reject(error)
        }
        isLoading = false
    }

    @MainActor private func adopt(_ detail: CoreItemDetail, availability: PlaybackAvailability?) {
        failure = nil
        let built = makeContent(detail)
        content = built
        refreshAudio(afterRead: built, availability: availability)
        if audio == .locked { dropText() }
    }

    @MainActor private func reject(_ error: Error) {
        endAudio()
        content = nil
        audio = .none
        switch error as? ItemDetailError {
        case .some(.notAuthenticated), .some(.relocked): failure = .locked
        case .some(.notFound), .some(.invalidItemID): failure = .notFound
        default: failure = .unreadable
        }
    }

    @MainActor private func refreshAudio(afterRead built: SourceDetailContent, availability: PlaybackAvailability?) {
        if built.isDeleted {
            endAudio()
            audio = .removed
        } else if !built.hasRetainedAudioReference {
            // The audio is gone (removed, expired) or never existed; a session open on it must stop.
            let hadAudio = session != nil || audio != .none
            endAudio()
            audio = hadAudio ? .removed : .none
        } else if session == nil {
            switch availability ?? .available {
            case .available: if audio == .none || audio == .removed || audio == .unplayable { audio = .available }
            case .noAudio, .missing: audio = .removed
            case .unreadable: audio = .unplayable
            case .notAuthenticated: audio = .locked
            }
        }
    }

    // MARK: playback

    /// Starts or resumes playback. The first press opens the audio through the playback service.
    @MainActor func play() async {
        guard canPlay else { return }
        if let session {
            checkSession()
            if self.session != nil {
                if session.play() { audio = .playing } else { checkSession(playRefused: true) }
                return
            }
            // The session ended on its own (finished or closed) and the control is playable again: open it afresh.
            guard canPlay else { return }
        }
        openGeneration += 1
        let generation = openGeneration
        let opened: SourceAudioSession
        do {
            opened = try await opener.openAudio(itemID: itemID)
        } catch {
            guard generation == openGeneration else { return }
            audio = Self.audioState(after: error)
            if audio == .locked { dropText() }
            return
        }
        guard generation == openGeneration, audio == .available else {
            opened.close()
            return
        }
        session = opened
        if opened.play() { audio = .playing } else { checkSession(playRefused: true) }
    }

    @MainActor func pause() {
        guard let session, audio == .playing else { return }
        session.pause()
        audio = .paused
    }

    /// Notices that the open playback ended on its own (deletion requested, relock, shutdown) and turns the control off.
    @MainActor func checkSession(playRefused: Bool = false) {
        // The text is private too: a session that is no longer authenticated drops it even when nothing is playing.
        if content != nil, readScope() != .authenticated {
            endAudio()
            audio = .locked
            dropText()
            return
        }
        guard let session else { return }
        switch session.endReason {
        case .some(.relocked):
            self.session = nil
            audio = .locked
            dropText()
        case .some(.deletionRequested):
            self.session = nil
            audio = .removed
        case .some(.closed), .some(.finished):
            // Closed or played to the end: the file is released and the clip can be played again from the start.
            self.session = nil
            audio = .available
        case .none:
            if playRefused {
                // The output refused to start but nothing ended it.
                audio = .unplayable
                endAudio()
            }
        }
    }

    /// Releases the audio when the screen goes away, so the file is no longer held in place.
    @MainActor func close() {
        loadGeneration += 1
        openGeneration += 1
        endAudio()
        if audio == .playing || audio == .paused { audio = .available }
    }

    @MainActor private func endAudio() {
        session?.close()
        session = nil
    }

    @MainActor private func dropText() {
        content = nil
        failure = .locked
    }

    private static func audioState(after error: Error) -> SourceAudioState {
        guard let playback = error as? PlaybackError else { return .unplayable }
        switch playback {
        case .notAuthenticated, .shutDown: return .locked
        case .noAudio, .itemDeleted, .itemNotFound: return .removed
        case .invalidRequest, .audioReferenceUnresolvable, .audioMissing, .audioUnreadable, .core: return .unplayable
        }
    }

    // MARK: presentation

    private func makeContent(_ detail: CoreItemDetail) -> SourceDetailContent {
        var sections: [SourceDetailTextSection] = []
        if let original = detail.originalText, !original.isEmpty {
            sections.append(SourceDetailTextSection(kind: .original, text: original, provenance: nil))
        }
        if let transcript = detail.transcript, !transcript.text.isEmpty {
            sections.append(SourceDetailTextSection(
                kind: .transcript, text: transcript.text, provenance: Self.provenance(of: transcript)))
        }
        if detail.textBasis == .corrected, let corrected = detail.correctedText, !corrected.isEmpty {
            sections.append(SourceDetailTextSection(kind: .correction, text: corrected, provenance: nil))
        }
        let history = detail.history.map { event in
            SourceDetailHistoryRow(
                id: event.eventID, summary: Self.summary(of: event), happenedAt: date(from: event.happenedAt))
        }
        return SourceDetailContent(
            itemID: detail.itemID,
            capturedAt: date(from: detail.capturedAt),
            typeLabel: detail.itemType.map(Self.typeLabel),
            scopeName: detail.scope == "work" ? "Work" : detail.scope == "personal" ? "Personal" : detail.scope,
            sessionTopic: detail.sessionTopic,
            lifecycleLabel: Self.lifecycleLabel(detail.lifecycleState),
            currentText: detail.text,
            sections: sections,
            history: history,
            historyTruncated: detail.historyTruncated,
            hasRetainedAudioReference: detail.audioReference != nil,
            isDeleted: detail.lifecycleState == "deleted")
    }

    private func date(from stamp: String) -> Date? {
        timestampParser.date(from: stamp) ?? fractionalTimestampParser.date(from: stamp)
    }

    private static func provenance(of transcript: CoreItemDetailTranscript) -> String {
        var parts = ["Transcribed on this device by \(transcript.recognizer)"]
        if let language = transcript.detectedLanguage { parts.append("language \(language)") }
        if let confidence = transcript.confidence { parts.append("confidence \(Int((confidence * 100).rounded()))%") }
        return parts.joined(separator: ", ")
    }

    private static func summary(of event: CoreItemDetailEvent) -> String {
        if let correction = event.correction {
            switch correction.kind {
            case "text": return "You corrected the text"
            case "type": return "You changed the type"
            case "session_topic": return "You changed the session topic"
            default: return "You made a correction"
            }
        }
        if event.suggestionControl != nil { return "You answered a suggestion" }
        return event.eventType.replacingOccurrences(of: "_", with: " ").capitalizedFirst
    }

    private static func typeLabel(_ raw: String) -> String {
        raw.replacingOccurrences(of: "_", with: " ").capitalizedFirst
    }

    private static func lifecycleLabel(_ raw: String) -> String {
        raw.replacingOccurrences(of: "_", with: " ").capitalizedFirst
    }

    static func assembled(itemID: String, root: ServiceCompositionRoot) -> SourceDetailModel {
        SourceDetailModel(
            itemID: itemID, reader: root.itemDetails, opener: root.playback, readScope: { root.readScope },
            deletion: .assembled(itemID: itemID, root: root))
    }
}

private extension String {
    var capitalizedFirst: String {
        prefix(1).uppercased() + dropFirst()
    }
}
