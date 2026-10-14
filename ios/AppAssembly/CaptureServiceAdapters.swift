import Foundation
import OhAndCoreBridge
import OhAndServices

/// Gives the capture surface's text seam the composition root's save. A confirmed import is `saved`, a staged but
/// unconfirmed one is `keptOnDevice`, and a failure to stage is `notStaged`.
final class RootTextCaptureIngress: TextCaptureIngress {
    typealias Save = (
        _ captureID: String, _ text: String, _ capturedAt: Date,
        _ completion: @escaping (CompositionSaveOutcome) -> Void
    ) -> Void

    private let save: Save

    init(save: @escaping Save) {
        self.save = save
    }

    convenience init(root: ServiceCompositionRoot) {
        self.init(save: { [weak root] captureID, text, capturedAt, completion in
            guard let root else {
                completion(.notStaged(captureID: captureID))
                return
            }
            root.saveText(captureID: captureID, text: text, capturedAt: capturedAt, completion: completion)
        })
    }

    func submitText(_ submission: TextCaptureSubmission, completion: @escaping (TextCaptureOutcome) -> Void) {
        save(submission.captureID, submission.text, submission.capturedAt) { outcome in
            completion(Self.outcome(for: outcome))
        }
    }

    static func outcome(for outcome: CompositionSaveOutcome) -> TextCaptureOutcome {
        switch outcome {
        case .saved(let captureID, let itemID, let alreadySaved):
            return .saved(captureID: captureID, itemID: itemID, alreadySaved: alreadySaved)
        case .keptOnDevice(let captureID):
            return .keptOnDevice(captureID: captureID)
        case .notStaged(let captureID):
            return .notStaged(captureID: captureID)
        case .itemDeleted(let captureID):
            return .itemDeleted(captureID: captureID)
        }
    }
}

/// Reads a saved item's status through the root, which refuses the read unless the session is authenticated.
///
/// The due time of an installed reminder is not known here: the core's report carries none and the pending
/// notification request is keyed by an opaque target, not the item. So the status never claims a reminder is set; the
/// acknowledgment keeps saying it is not confirmed.
final class RootCaptureProgressReader: CaptureProgressReading {
    typealias ReadItem = (
        _ itemID: String, _ completion: @escaping (Result<ItemStatusReport, CoreFailure>) -> Void
    ) -> Void

    private let readItem: ReadItem

    init(readItem: @escaping ReadItem) {
        self.readItem = readItem
    }

    convenience init(root: ServiceCompositionRoot) {
        self.init(readItem: { [weak root] itemID, completion in
            guard let root else {
                completion(.failure(.handleClosed))
                return
            }
            root.itemStatus(itemID: itemID, completion: completion)
        })
    }

    func readStatus(itemID: String, completion: @escaping (CaptureProgressReadResult) -> Void) {
        readItem(itemID) { result in
            let answer = Self.answer(for: result)
            if Thread.isMainThread {
                completion(answer)
            } else {
                DispatchQueue.main.async { completion(answer) }
            }
        }
    }

    static func answer(for result: Result<ItemStatusReport, CoreFailure>) -> CaptureProgressReadResult {
        switch result {
        case .success(let report):
            return .status(
                CaptureItemStatus(
                    processingState: report.processingState,
                    transcriptionState: report.transcriptionState,
                    reminderRequestState: report.reminderRequestState,
                    reminderScheduleState: report.reminderScheduleState,
                    unschedulableReason: report.unschedulableReason,
                    installedDueInstant: nil))
        case .failure:
            return .unavailable
        }
    }
}

/// Stands in for the recorder until a voice save path exists. The composition root saves text only: it has no audio
/// ingress, so a recording made now could not be imported. Starting says so, and nothing is recorded.
final class VoiceRecorderUnavailable: VoiceRecordingControlling {
    var state: VoiceRecorderState { .idle }
    var onStateChange: ((VoiceRecorderState) -> Void)?
    var onOutcome: ((VoiceCaptureOutcome) -> Void)?

    func start(completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void) {
        completion(.failure(.recorderUnavailable))
    }

    func stop() {}

    func cancel() {}
}
