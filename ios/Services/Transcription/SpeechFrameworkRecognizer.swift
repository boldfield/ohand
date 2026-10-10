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
        let session = RecognitionSession(keepingAlive: recognizer)
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
        confidence(ofSegmentConfidences: transcription.segments.map(\.confidence))
    }

    static func confidence(ofSegmentConfidences segmentConfidences: [Float]) -> Double? {
        guard !segmentConfidences.isEmpty else { return nil }
        let mean = segmentConfidences.map { Double($0) }.reduce(0, +) / Double(segmentConfidences.count)
        guard mean > 0 else { return nil }
        return min(max(mean, 0), 1)
    }

    /// `kAFAssistantErrorDomain` 1110 is "No speech detected" and 216 is a request the framework ended, as observed
    /// on device (see docs/validation/transcription-probe.md). A cancellation this code asked for is reported by
    /// `RecognitionSession` as `.cancelled`, so a 216 reaching here was raised by the system, not by us: it is a
    /// transient fault that spends retry budget rather than an interruption that spends none. Everything else is a
    /// recognition failure.
    static func outcome(for error: NSError) -> OnDeviceRecognitionOutcome {
        if error.domain == "kAFAssistantErrorDomain" {
            if error.code == 1110 { return .transcript(text: "", confidence: nil) }
            if error.code == 216 { return .unavailableNow(reason: "recognition_ended_by_system") }
        }
        return .failed
    }
}

/// Something a running recognition can be told to stop.
protocol RecognitionCancelling: AnyObject {
    func cancel()
}

extension SFSpeechRecognitionTask: RecognitionCancelling {}

/// Resumes the recognition's continuation exactly once, whether recognition finishes, fails or is cancelled first.
/// It also holds `keepingAlive` (the `SFSpeechRecognizer`) until the recognition ends, because the framework may stop
/// delivering results once the recognizer that made a task is released.
final class RecognitionSession: @unchecked Sendable {
    private let lock = NSLock()
    private var keepAlive: AnyObject?
    private var continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>?
    private var task: RecognitionCancelling?
    private var isCancelled = false

    init(keepingAlive: AnyObject) {
        keepAlive = keepingAlive
    }

    /// Returns false, having resumed with `.cancelled`, when the calling task was cancelled before it began.
    func install(_ continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) -> Bool {
        lock.lock()
        if isCancelled {
            keepAlive = nil
            lock.unlock()
            continuation.resume(returning: .cancelled)
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    func attach(_ task: RecognitionCancelling) {
        lock.lock()
        let cancelNow = isCancelled || continuation == nil
        if !cancelNow { self.task = task }
        lock.unlock()
        if cancelNow { task.cancel() }
    }

    /// Ends the recognition with `outcome` unless it already ended; later calls do nothing.
    func finish(_ outcome: OnDeviceRecognitionOutcome) {
        lock.lock()
        let pending = continuation
        continuation = nil
        task = nil
        keepAlive = nil
        lock.unlock()
        pending?.resume(returning: outcome)
    }

    /// Stops the recognition because the caller asked to; this is the only way the session reports `.cancelled`.
    func cancel() {
        lock.lock()
        isCancelled = true
        let running = task
        lock.unlock()
        running?.cancel()
        finish(.cancelled)
    }
}
