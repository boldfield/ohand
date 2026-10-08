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
        let task = recognizer.recognitionTask(with: request) { result, error in
            if let result = result, result.isFinal {
                let segments = result.bestTranscription.segments
                let confidence: Float? = segments.isEmpty
                    ? nil
                    : segments.map { $0.confidence }.reduce(0, +) / Float(segments.count)
                let text = result.bestTranscription.formattedString
                DispatchQueue.main.async {
                    completion(.transcript(text: text, confidence: confidence))
                }
            } else if let error = error {
                let nsError = error as NSError
                let reason = "\(nsError.domain) \(nsError.code): \(nsError.localizedDescription)"
                DispatchQueue.main.async {
                    completion(.failure(reason: reason))
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
