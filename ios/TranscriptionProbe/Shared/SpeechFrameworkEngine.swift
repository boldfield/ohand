import Foundation
import Speech

private final class SpeechTaskHandle: RecognitionHandle {
    private let task: SFSpeechRecognitionTask?

    init(task: SFSpeechRecognitionTask?) {
        self.task = task
    }

    func cancel() {
        task?.cancel()
    }
}

/// Accumulates the per-segment results of one recognition and decides when it has ended. On-device recognition
/// delivers one finished result per segment and only the last is final, so the transcript is every accepted result.
/// A "no speech detected" error (`kAFAssistantErrorDomain` 1110) after accepted segments is a silent tail, not a
/// failure. Kept apart from the framework so the tests can drive it.
final class RecognitionResultReducer {
    private let lock = NSLock()
    private var accumulator = RecognitionAccumulator()

    /// Returns the outcome once the recognition has ended, nil while more results are expected.
    func handle(result: RecognitionResultValue?, error: NSError?) -> RecognitionOutcome? {
        lock.lock()
        defer { lock.unlock() }
        if let result = result {
            accumulator.add(result)
            if result.isFinal { return accumulatedOutcome() }
        }
        guard let error = error else { return nil }
        if error.domain == "kAFAssistantErrorDomain", error.code == 1110, accumulator.hasAcceptedResults {
            return accumulatedOutcome()
        }
        let reason = "\(error.domain) \(error.code): \(error.localizedDescription)"
        return .failure(reason: reason)
    }

    private func accumulatedOutcome() -> RecognitionOutcome {
        let confidences = accumulator.segmentConfidences
        let confidence: Float? = confidences.isEmpty ? nil : confidences.reduce(0, +) / Float(confidences.count)
        return .transcript(
            text: accumulator.transcript, confidence: confidence, resultCount: accumulator.receivedCount)
    }
}

/// Real engine over `SFSpeechRecognizer`. It only ever issues requests with `requiresOnDeviceRecognition = true`
/// and refuses to start one when `supportsOnDeviceRecognition` is false, so audio is never sent to Apple servers.
final class SpeechFrameworkEngine: NSObject, SpeechRecognitionEngine, SFSpeechRecognizerDelegate {
    var onAvailabilityChange: (() -> Void)?
    private var recognizersByLocale: [String: SFSpeechRecognizer] = [:]

    private func recognizer(for localeIdentifier: String) -> SFSpeechRecognizer? {
        if let existing = recognizersByLocale[localeIdentifier] {
            return existing
        }
        guard let created = SFSpeechRecognizer(locale: Locale(identifier: localeIdentifier)) else {
            return nil
        }
        created.delegate = self
        recognizersByLocale[localeIdentifier] = created
        return created
    }

    private static func permission(from status: SFSpeechRecognizerAuthorizationStatus) -> SpeechPermission {
        switch status {
        case .notDetermined:
            return .notDetermined
        case .denied:
            return .denied
        case .restricted:
            return .restricted
        case .authorized:
            return .authorized
        @unknown default:
            return .restricted
        }
    }

    func snapshot(localeIdentifier: String) -> RecognizerSnapshot {
        let recognizer = self.recognizer(for: localeIdentifier)
        return RecognizerSnapshot(
            permission: SpeechFrameworkEngine.permission(from: SFSpeechRecognizer.authorizationStatus()),
            recognizerExists: recognizer != nil,
            isAvailable: recognizer?.isAvailable ?? false,
            supportsOnDevice: recognizer?.supportsOnDeviceRecognition ?? false
        )
    }

    func requestPermission(completion: @escaping (SpeechPermission) -> Void) {
        SFSpeechRecognizer.requestAuthorization { status in
            DispatchQueue.main.async {
                completion(SpeechFrameworkEngine.permission(from: status))
            }
        }
    }

    func recognize(
        audioURL: URL,
        localeIdentifier: String,
        completion: @escaping (RecognitionOutcome) -> Void
    ) -> RecognitionHandle {
        guard let recognizer = self.recognizer(for: localeIdentifier), recognizer.supportsOnDeviceRecognition else {
            DispatchQueue.main.async {
                completion(.failure(reason: "On-device recognition unavailable for \(localeIdentifier); no request was made"))
            }
            return SpeechTaskHandle(task: nil)
        }
        let request = SFSpeechURLRecognitionRequest(url: audioURL)
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = false
        let reducer = RecognitionResultReducer()
        let task = recognizer.recognitionTask(with: request) { result, error in
            let value = result.map { RecognitionResultValue($0) }
            if let outcome = reducer.handle(result: value, error: error as NSError?) {
                DispatchQueue.main.async {
                    completion(outcome)
                }
            }
        }
        return SpeechTaskHandle(task: task)
    }

    func speechRecognizer(_ speechRecognizer: SFSpeechRecognizer, availabilityDidChange available: Bool) {
        DispatchQueue.main.async { [weak self] in
            self?.onAvailabilityChange?()
        }
    }
}
