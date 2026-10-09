import Foundation
@testable import OhAnd
@testable import OhAndServices

/// Scripted ingress: records every submission and answers from a queue, or holds completions until `release()`.
final class ScriptedTextCaptureIngress: TextCaptureIngress {
    private(set) var submissions: [TextCaptureSubmission] = []
    var outcomes: [TextCaptureOutcome] = []
    var holdsCompletions = false
    private var held: [() -> Void] = []

    func submitText(_ submission: TextCaptureSubmission, completion: @escaping (TextCaptureOutcome) -> Void) {
        submissions.append(submission)
        let outcome = outcomes.isEmpty ? .notStaged(captureID: submission.captureID) : outcomes.removeFirst()
        if holdsCompletions {
            held.append { completion(outcome) }
        } else {
            completion(outcome)
        }
    }

    func release() {
        let pending = held
        held = []
        pending.forEach { $0() }
    }
}

/// Maps the real foreground ingress service onto the capture surface's seam the way a composition root must: a
/// confirmed import is `saved`, a staged but unconfirmed one is `keptOnDevice`, and a failure to stage is `notStaged`.
final class ForegroundIngressTextCaptureAdapter: TextCaptureIngress {
    private let service: ForegroundIngressService
    private let context: (Date) -> IngressCaptureContext

    init(service: ForegroundIngressService, context: @escaping (Date) -> IngressCaptureContext) {
        self.service = service
        self.context = context
    }

    func submitText(_ submission: TextCaptureSubmission, completion: @escaping (TextCaptureOutcome) -> Void) {
        let record = IngressRecord(
            captureID: submission.captureID, text: submission.text, audio: nil, context: context(submission.capturedAt))
        service.submit(record) { outcome in
            completion(Self.outcome(for: outcome))
        }
    }

    static func outcome(for outcome: IngressOutcome) -> TextCaptureOutcome {
        switch outcome {
        case .saved(let acknowledgment):
            return .saved(
                captureID: acknowledgment.captureID, itemID: acknowledgment.itemID,
                alreadySaved: acknowledgment.alreadyImported)
        case .keptForRetry(let captureID, _):
            return .keptOnDevice(captureID: captureID)
        case .notStaged(let captureID, _):
            return .notStaged(captureID: captureID)
        case .itemDeleted(let captureID):
            return .itemDeleted(captureID: captureID)
        }
    }
}
