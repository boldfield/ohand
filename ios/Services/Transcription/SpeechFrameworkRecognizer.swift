import Foundation
import Speech

/// The real on-device recognizer over `SFSpeechRecognizer`.
///
/// Every request sets `requiresOnDeviceRecognition = true`, and no request is made at all when the recognizer does
/// not report `supportsOnDeviceRecognition` for the locale, so audio never reaches Apple's servers. A language the
/// framework does not know and a language whose on-device model is not installed are both `unsupported`.
public final class SpeechFrameworkRecognizer: OnDeviceRecognizing, @unchecked Sendable {
    public let recognizerName: String
    private let mayPromptForPermission: @Sendable () -> Bool

    /// `mayPromptForPermission` is asked when speech permission has not been decided; answer `true` only while
    /// the app is in the foreground, where the system can show its prompt. When it answers `false` the attempt is
    /// retried later instead.
    public init(mayPromptForPermission: @escaping @Sendable () -> Bool = { true }) {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        recognizerName = "apple-sfspeech-on-device/\(version.majorVersion).\(version.minorVersion)"
        self.mayPromptForPermission = mayPromptForPermission
    }

    private enum Permission {
        case authorized
        case denied
        case notDetermined
    }

    public func recognize(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome {
        switch await permission() {
        case .denied:
            return .permissionDenied
        case .notDetermined:
            return .unavailableNow(reason: "speech_permission_not_determined")
        case .authorized:
            break
        }
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: localeIdentifier)),
            recognizer.supportsOnDeviceRecognition
        else {
            return .unsupported
        }
        guard recognizer.isAvailable else {
            return .unavailableNow(reason: "recognizer_unavailable")
        }
        let session = RecognitionSession()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
                guard session.install(continuation) else { return }
                let request = SFSpeechURLRecognitionRequest(url: audioURL)
                request.requiresOnDeviceRecognition = true
                request.shouldReportPartialResults = false
                let task = recognizer.recognitionTask(with: request) { result, error in
                    if let result = result, result.isFinal {
                        session.finish(
                            .transcript(
                                text: result.bestTranscription.formattedString,
                                confidence: SpeechFrameworkRecognizer.confidence(of: result.bestTranscription)))
                    } else if let error = error {
                        session.finish(SpeechFrameworkRecognizer.outcome(for: error as NSError))
                    }
                }
                session.attach(task)
            }
        } onCancel: {
            session.cancel()
        }
    }

    private func permission() async -> Permission {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized:
            return .authorized
        case .denied, .restricted:
            return .denied
        case .notDetermined:
            guard mayPromptForPermission() else { return .notDetermined }
            let status = await withCheckedContinuation {
                (continuation: CheckedContinuation<SFSpeechRecognizerAuthorizationStatus, Never>) in
                SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
            }
            switch status {
            case .authorized:
                return .authorized
            case .denied, .restricted:
                return .denied
            default:
                return .notDetermined
            }
        @unknown default:
            return .denied
        }
    }

    /// The mean segment confidence. The framework reports 0 for segments it did not score, so an all-zero mean means
    /// "unknown" and is not reported as a score.
    static func confidence(of transcription: SFTranscription) -> Double? {
        let segments = transcription.segments
        guard !segments.isEmpty else { return nil }
        let mean = segments.map { Double($0.confidence) }.reduce(0, +) / Double(segments.count)
        guard mean > 0 else { return nil }
        return min(max(mean, 0), 1)
    }

    /// `kAFAssistantErrorDomain` 1110 is "No speech detected" and 216 is a cancelled request, as observed on device
    /// (see docs/validation/transcription-probe.md); everything else is a recognition failure.
    static func outcome(for error: NSError) -> OnDeviceRecognitionOutcome {
        if error.domain == "kAFAssistantErrorDomain" {
            if error.code == 1110 { return .transcript(text: "", confidence: nil) }
            if error.code == 216 { return .cancelled }
        }
        return .failed
    }
}

/// Resumes the recognition's continuation exactly once, whether recognition finishes, fails or is cancelled first.
private final class RecognitionSession: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>?
    private var task: SFSpeechRecognitionTask?
    private var isCancelled = false

    /// Returns false, having resumed with `.cancelled`, when the calling task was cancelled before it began.
    func install(_ continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) -> Bool {
        lock.lock()
        if isCancelled {
            lock.unlock()
            continuation.resume(returning: .cancelled)
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    func attach(_ task: SFSpeechRecognitionTask) {
        lock.lock()
        self.task = task
        let cancelNow = isCancelled
        lock.unlock()
        if cancelNow { task.cancel() }
    }

    func finish(_ outcome: OnDeviceRecognitionOutcome) {
        lock.lock()
        let pending = continuation
        continuation = nil
        lock.unlock()
        pending?.resume(returning: outcome)
    }

    func cancel() {
        lock.lock()
        isCancelled = true
        let running = task
        lock.unlock()
        running?.cancel()
        finish(.cancelled)
    }
}
