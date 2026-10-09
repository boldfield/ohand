import Foundation

/// State behind the silent capture field. It asks for nothing but text: no tags, project, classification wait or
/// coaching. The field is cleared only when the text is durable, and a retry of the same text reuses its capture ID.
///
/// Confined to the main queue.
final class TextCaptureModel: ObservableObject {
    enum Status: Equatable {
        case idle
        case saving
        /// Confirmed by the core. Nothing has been interpreted yet.
        case saved(alreadySaved: Bool)
        /// Durable on this device but not confirmed saved.
        case keptOnDevice
        /// Nothing durable was written; the text is still in the field.
        case notSaved
        case itemDeleted

        /// The honest, user-visible result. `nil` when there is nothing to report.
        var message: String? {
            switch self {
            case .idle:
                return nil
            case .saving:
                return "Saving\u{2026}"
            case .saved:
                return "Saved. It has not been processed yet, so no reminder has been set."
            case .keptOnDevice:
                return "Kept on this device. It is not confirmed saved yet and will finish saving later. No reminder has been set."
            case .notSaved:
                return "Not saved. Your text is still here."
            case .itemDeleted:
                return "That capture was deleted, so it was not saved again."
            }
        }
    }

    private struct Intent {
        var captureID: String
        var text: String
        var capturedAt: Date
    }

    @Published private(set) var text = ""
    @Published private(set) var status: Status = .idle

    private let ingress: TextCaptureIngress
    private let makeCaptureID: () -> String
    private let now: () -> Date
    /// The capture that the current text was last submitted as. It survives a failed attempt, so a retry of unchanged
    /// text is the same record; it is dropped once the text is durable or edited.
    private var pendingIntent: Intent?

    init(
        ingress: TextCaptureIngress,
        makeCaptureID: @escaping () -> String = { UUID().uuidString.lowercased() },
        now: @escaping () -> Date = Date.init
    ) {
        self.ingress = ingress
        self.makeCaptureID = makeCaptureID
        self.now = now
    }

    var isSaving: Bool { status == .saving }

    var canSave: Bool { !isSaving && hasContent(text) }

    func updateText(_ newText: String) {
        guard newText != text else { return }
        text = newText
        if !isSaving { status = .idle }
    }

    func save() {
        guard canSave else { return }
        let intent = currentIntent()
        pendingIntent = intent
        status = .saving
        ingress.submitText(TextCaptureSubmission(
            captureID: intent.captureID, text: intent.text, capturedAt: intent.capturedAt)
        ) { [weak self] outcome in
            self?.finish(outcome, for: intent)
        }
    }

    private func currentIntent() -> Intent {
        if let pendingIntent, pendingIntent.text == text { return pendingIntent }
        return Intent(captureID: makeCaptureID(), text: text, capturedAt: now())
    }

    private func finish(_ outcome: TextCaptureOutcome, for intent: Intent) {
        switch outcome {
        case .saved(_, _, let alreadySaved):
            release(intent)
            status = .saved(alreadySaved: alreadySaved)
        case .keptOnDevice:
            release(intent)
            status = .keptOnDevice
        case .itemDeleted:
            release(intent)
            status = .itemDeleted
        case .notStaged:
            status = .notSaved
        }
    }

    /// The submitted text is durable, so the field can take the next capture. Text typed while the save was in
    /// flight is a different capture and stays.
    private func release(_ intent: Intent) {
        pendingIntent = nil
        if text == intent.text { text = "" }
    }

    private func hasContent(_ candidate: String) -> Bool {
        !candidate.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}
