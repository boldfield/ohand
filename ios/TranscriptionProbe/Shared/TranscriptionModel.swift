import Foundation

enum SpeechPermission: Equatable {
    case notDetermined
    case denied
    case restricted
    case authorized
}

/// Everything the readiness decision depends on, read fresh from the speech engine before every transcription.
struct RecognizerSnapshot: Equatable {
    var permission: SpeechPermission
    var recognizerExists: Bool
    var isAvailable: Bool
    var supportsOnDevice: Bool
}

enum PendingReason: Equatable {
    case permissionNotDetermined
    case permissionDenied
    case permissionRestricted
    case languageUnsupported(locale: String)
    case onDeviceUnavailable(locale: String)
    case recognizerUnavailable(locale: String)

    var description: String {
        switch self {
        case .permissionNotDetermined:
            return "speech recognition permission not requested yet"
        case .permissionDenied:
            return "speech recognition permission denied or revoked"
        case .permissionRestricted:
            return "speech recognition restricted on this device"
        case .languageUnsupported(let locale):
            return "language \(locale) is not supported by the speech framework"
        case .onDeviceUnavailable(let locale):
            return "on-device recognition unavailable for \(locale) (model not installed or device unsupported; the framework does not say which)"
        case .recognizerUnavailable(let locale):
            return "recognizer for \(locale) is currently unavailable"
        }
    }
}

/// Returns why on-device transcription cannot run, or nil when it can. There is no cloud fallback: every reason
/// other than nil leaves the audio untouched and the transcript pending.
func evaluateReadiness(_ snapshot: RecognizerSnapshot, localeIdentifier: String) -> PendingReason? {
    switch snapshot.permission {
    case .notDetermined:
        return .permissionNotDetermined
    case .denied:
        return .permissionDenied
    case .restricted:
        return .permissionRestricted
    case .authorized:
        break
    }
    if !snapshot.recognizerExists {
        return .languageUnsupported(locale: localeIdentifier)
    }
    if !snapshot.supportsOnDevice {
        return .onDeviceUnavailable(locale: localeIdentifier)
    }
    if !snapshot.isAvailable {
        return .recognizerUnavailable(locale: localeIdentifier)
    }
    return nil
}

enum RecognitionOutcome: Equatable {
    case transcript(text: String, confidence: Float?)
    case failure(reason: String)
}

protocol RecognitionHandle: AnyObject {
    func cancel()
}

/// Seam over the speech framework. Completion closures must be delivered on the main thread.
protocol SpeechRecognitionEngine: AnyObject {
    func snapshot(localeIdentifier: String) -> RecognizerSnapshot
    func requestPermission(completion: @escaping (SpeechPermission) -> Void)
    func recognize(
        audioURL: URL,
        localeIdentifier: String,
        completion: @escaping (RecognitionOutcome) -> Void
    ) -> RecognitionHandle
}

enum TranscriptionStatus: Equatable {
    case noAudio
    case ready
    case pending(PendingReason)
    case inProgress
    case transcribed(text: String, confidence: Float?, durationSeconds: Double)
    case failed(reason: String, durationSeconds: Double)
    case cancelled

    var description: String {
        switch self {
        case .noAudio:
            return "No audio loaded"
        case .ready:
            return "Ready: on-device recognition is available"
        case .pending(let reason):
            return "Pending: \(reason.description). Audio kept; no transcription was attempted."
        case .inProgress:
            return "Transcribing on device..."
        case .transcribed(let text, let confidence, let durationSeconds):
            let confidenceText = confidence.map { String(format: ", mean confidence %.2f", $0) } ?? ""
            return String(format: "Transcribed on device in %.2fs%@: \"%@\"", durationSeconds, confidenceText, text)
        case .failed(let reason, let durationSeconds):
            return String(format: "Failed after %.2fs: %@. Audio kept; retry is possible.", durationSeconds, reason)
        case .cancelled:
            return "Cancelled. Audio kept."
        }
    }
}

enum TranscriptionInterruption: String, Equatable {
    case applicationBackgrounded = "app moved to background"
    case protectedDataUnavailable = "device locked (protected data unavailable)"
}

enum TranscriptionFixture {
    static let baseName = "transcription-fixture"
    static let generatedExtension = "caf"
    static let acceptedExtensions = ["wav", "caf"]
    static let phrase = "Remind me to call the dentist tomorrow at nine"

    static func generatedURL(in directory: URL) -> URL {
        return directory.appendingPathComponent("\(baseName).\(generatedExtension)")
    }

    /// Fixed lookup order so the same named fixture is chosen every time: wav (operator-supplied) before caf (generated).
    static func locate(in directory: URL, fileManager: FileManager = .default) -> URL? {
        for fileExtension in acceptedExtensions {
            let candidate = directory.appendingPathComponent("\(baseName).\(fileExtension)")
            if fileManager.fileExists(atPath: candidate.path) {
                return candidate
            }
        }
        return nil
    }
}

/// Drives one audio file through readiness checks and on-device recognition. Main thread only.
/// The audio file is never moved, rewritten or deleted by any path in this class.
final class TranscriptionCoordinator {
    private let engine: SpeechRecognitionEngine
    private let now: () -> TimeInterval
    private let fileManager: FileManager
    private var generation = 0
    private var activeHandle: RecognitionHandle?
    private var startedAt: TimeInterval = 0

    private(set) var audioURL: URL?
    private(set) var localeIdentifier: String
    private(set) var interruptions: [TranscriptionInterruption] = []
    private(set) var status: TranscriptionStatus = .noAudio {
        didSet { onChange?() }
    }
    var onChange: (() -> Void)?

    init(
        engine: SpeechRecognitionEngine,
        localeIdentifier: String,
        fileManager: FileManager = .default,
        now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    ) {
        self.engine = engine
        self.localeIdentifier = localeIdentifier
        self.fileManager = fileManager
        self.now = now
    }

    private var isInProgress: Bool {
        if case .inProgress = status { return true }
        return false
    }

    private func audioFileHasContent(_ url: URL) -> Bool {
        guard let attributes = try? fileManager.attributesOfItem(atPath: url.path),
              let size = attributes[.size] as? NSNumber else {
            return false
        }
        return size.intValue > 0
    }

    /// Returns nil on success, otherwise the reason the file was rejected (previous audio, if any, is kept).
    @discardableResult
    func loadAudio(at url: URL) -> String? {
        if isInProgress {
            return "Transcription in progress"
        }
        guard audioFileHasContent(url) else {
            return "Audio file is missing or empty: \(url.lastPathComponent)"
        }
        audioURL = url
        interruptions = []
        applyReadiness()
        return nil
    }

    /// Returns false when the change was refused because a transcription is running.
    @discardableResult
    func setLocale(_ newLocaleIdentifier: String) -> Bool {
        if isInProgress {
            return false
        }
        localeIdentifier = newLocaleIdentifier
        applyReadiness()
        return true
    }

    func requestPermission() {
        engine.requestPermission { [weak self] _ in
            self?.refreshAfterEnvironmentChange()
        }
    }

    /// Re-reads permission and recognizer availability. Never disturbs a running or finished transcription.
    func refreshAfterEnvironmentChange() {
        switch status {
        case .ready, .pending, .failed, .cancelled:
            applyReadiness()
        case .noAudio, .inProgress, .transcribed:
            break
        }
    }

    private func applyReadiness() {
        guard audioURL != nil else {
            status = .noAudio
            return
        }
        let snapshot = engine.snapshot(localeIdentifier: localeIdentifier)
        if let reason = evaluateReadiness(snapshot, localeIdentifier: localeIdentifier) {
            status = .pending(reason)
        } else {
            status = .ready
        }
    }

    func startTranscription() {
        guard let audioURL = audioURL else {
            status = .noAudio
            return
        }
        if isInProgress {
            return
        }
        let snapshot = engine.snapshot(localeIdentifier: localeIdentifier)
        if let reason = evaluateReadiness(snapshot, localeIdentifier: localeIdentifier) {
            status = .pending(reason)
            return
        }
        guard audioFileHasContent(audioURL) else {
            status = .failed(reason: "Audio file is missing or empty", durationSeconds: 0)
            return
        }

        generation += 1
        let runGeneration = generation
        interruptions = []
        startedAt = now()
        status = .inProgress
        let handle = engine.recognize(audioURL: audioURL, localeIdentifier: localeIdentifier) { [weak self] outcome in
            self?.complete(outcome, runGeneration: runGeneration)
        }
        if isInProgress && generation == runGeneration {
            activeHandle = handle
        }
    }

    func cancelTranscription() {
        guard isInProgress else { return }
        generation += 1
        let handle = activeHandle
        activeHandle = nil
        status = .cancelled
        handle?.cancel()
    }

    func noteInterruption(_ interruption: TranscriptionInterruption) {
        guard isInProgress else { return }
        interruptions.append(interruption)
        onChange?()
    }

    private func complete(_ outcome: RecognitionOutcome, runGeneration: Int) {
        guard runGeneration == generation, isInProgress else { return }
        activeHandle = nil
        let elapsedSeconds = max(0, now() - startedAt)
        switch outcome {
        case .transcript(let text, let confidence):
            let trimmedText = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmedText.isEmpty {
                status = .failed(reason: "No speech recognized", durationSeconds: elapsedSeconds)
            } else {
                status = .transcribed(text: trimmedText, confidence: confidence, durationSeconds: elapsedSeconds)
            }
        case .failure(let reason):
            status = .failed(reason: reason, durationSeconds: elapsedSeconds)
        }
    }

    var displayText: String {
        var text = status.description
        if !interruptions.isEmpty {
            let names = interruptions.map { $0.rawValue }.joined(separator: ", ")
            text += "\nInterruptions during run: \(names)"
        }
        return text
    }
}
