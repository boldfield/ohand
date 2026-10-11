import Foundation
import OhAndCoreBridge

/// The native handler for `transcription_attachment` jobs: it recognizes a queued voice capture on the device and
/// hands every outcome to the core's guarded transcript attachment.
///
/// What it decides, and what it leaves to the core:
///
/// * It reads the running job under its lease, resolves the capture's audio file, hashes it, recognizes it on the
///   device and applies the outcome. The core alone decides what changes: a stale lease, a deleted or already
///   transcribed item, or audio of another identity is refused or retired there and never overwrites content.
/// * **No remote fallback.** A language or model that is not available on the device is applied as `unsupported`:
///   the item stays pending and the audio is kept. A recognizer failure is applied as `failed`, also keeping the
///   audio.
/// * **Interruption and retry converge.** A cancelled run ends `.interrupted` and spends nothing. A fault expected
///   to pass (permission not yet decided, recognizer momentarily unavailable, audio not readable while the device
///   is locked, an uncommitted apply) is a transient failure counted against the job's retry budget. An apply the
///   core could not confirm is repeated with the same outcome, which converges, before it is reported as transient.
/// * **Recognition is bounded.** A recognition that has not answered within `recognitionTimeout` is cancelled and
///   ends as a transient failure, so a recognizer that never calls back cannot stall the drain.
/// * Failure reasons are fixed labels, never captured content.
public final class TranscriptionJobHandler: JobCapabilityHandler, @unchecked Sendable {
    public static let transcriptionJobType = "transcription_attachment"

    private let core: TranscriptionCoreOperations
    private let recognizer: OnDeviceRecognizing
    private let audio: TranscriptionAudioLocating
    private let interpretation: @Sendable (TranscriptionJobSource) -> InterpretationJobRequest?
    private let applyAttempts: Int
    private let applyRetryDelay: TimeInterval
    private let recognitionTimeout: TimeInterval

    /// How long one recognition may run before it is cancelled and retried later.
    public static let defaultRecognitionTimeout: TimeInterval = 300

    public var jobType: String { Self.transcriptionJobType }

    /// `interpretation` may ask the core to queue interpretation work once a transcript is attached; the core
    /// still decides whether it is authorized. By default none is requested.
    public init(
        core: TranscriptionCoreOperations,
        recognizer: OnDeviceRecognizing,
        audio: TranscriptionAudioLocating,
        interpretation: @escaping @Sendable (TranscriptionJobSource) -> InterpretationJobRequest? = { _ in nil },
        applyAttempts: Int = 3,
        applyRetryDelay: TimeInterval = 0.25,
        recognitionTimeout: TimeInterval = TranscriptionJobHandler.defaultRecognitionTimeout
    ) {
        self.core = core
        self.recognizer = recognizer
        self.audio = audio
        self.interpretation = interpretation
        self.applyAttempts = max(1, applyAttempts)
        self.applyRetryDelay = applyRetryDelay
        self.recognitionTimeout = recognitionTimeout
    }

    convenience init(
        core: TranscriptionCoreOperations,
        recognizer: OnDeviceRecognizing,
        layout: ProtectedStorageLayout,
        interpretation: @escaping @Sendable (TranscriptionJobSource) -> InterpretationJobRequest? = { _ in nil }
    ) {
        self.init(
            core: core, recognizer: recognizer, audio: ProtectedStorageAudioLocator(layout: layout),
            interpretation: interpretation)
    }

    public func run(_ job: JobCapabilityRun) async -> JobCapabilityResult {
        if Task.isCancelled { return .interrupted }
        let source: TranscriptionJobSource
        switch await core.readSource(jobID: job.jobID, attempt: job.attempt) {
        case .success(let value):
            source = value
        case .failure(let failure):
            return Self.result(forReadFailure: failure)
        }
        guard let reference = source.audioReference else {
            return .permanentFailure(reason: "no_audio_reference")
        }
        let outcome = TranscriptionOutcomeRequest(
            jobID: job.jobID, leaseAttempt: job.attempt, audioReference: reference, result: .failed)

        if source.lifecycleState == "deleted" || source.transcriptionState == "transcribed" {
            // Nothing to recognize. The core retires the job with its own content-free reason.
            return await apply(outcome)
        }
        guard let audioURL = audio.audioURL(forReference: reference) else {
            return await apply(outcome)
        }
        let digest: String
        switch TranscriptionAudioDigest.read(from: audioURL) {
        case .digest(let value):
            digest = value
        case .missing:
            return await apply(outcome)
        case .unreadable:
            return .transientFailure(reason: "audio_unreadable")
        }

        guard let recognition = await recognizeWithinDeadline(audioURL: audioURL, localeIdentifier: source.locale) else {
            return Task.isCancelled ? .interrupted : .transientFailure(reason: "recognition_timed_out")
        }
        switch recognition {
        case .cancelled:
            return .interrupted
        case .unavailableNow(let reason):
            return .transientFailure(reason: Self.label(reason))
        case .permissionDenied:
            return .permanentFailure(reason: "speech_permission_denied")
        case .unsupported:
            return await apply(outcome.replacing(.unsupported))
        case .failed:
            return await apply(outcome)
        case .transcript(let text, let confidence):
            guard case .digest(let after) = TranscriptionAudioDigest.read(from: audioURL), after == digest else {
                return .transientFailure(reason: "audio_changed")
            }
            let transcript = RecognizedTranscriptRequest(
                text: text, confidence: confidence, detectedLanguage: nil,
                recognizer: recognizer.recognizerName, audioSHA256: digest)
            return await apply(outcome.replacing(.transcript(transcript), interpretation: interpretation(source)))
        }
    }

    // MARK: private

    /// Runs the recognizer, cancelling it and answering nil when it has not answered within the timeout. The
    /// recognizer is expected to honor cancellation; one that does not is abandoned rather than awaited.
    private func recognizeWithinDeadline(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome? {
        let race = RecognitionDeadlineRace()
        let recognizer = self.recognizer
        let timeoutNanoseconds = UInt64(max(0, recognitionTimeout) * 1_000_000_000)
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome?, Never>) in
                race.install(continuation)
                let work = Task {
                    let outcome = await recognizer.recognize(audioURL: audioURL, localeIdentifier: localeIdentifier)
                    race.resolve(outcome)
                }
                let deadline = Task {
                    try? await Task.sleep(nanoseconds: timeoutNanoseconds)
                    if !Task.isCancelled { race.resolve(nil) }
                }
                race.attach(work: work, deadline: deadline)
            }
        } onCancel: {
            race.cancelRecognition()
        }
    }

    private func apply(_ outcome: TranscriptionOutcomeRequest) async -> JobCapabilityResult {
        for attempt in 0..<applyAttempts {
            if attempt > 0 {
                try? await Task.sleep(nanoseconds: UInt64(max(0, applyRetryDelay) * 1_000_000_000))
            }
            switch await core.apply(outcome) {
            case .success:
                return .settled
            case .failure(let failure):
                if let result = Self.result(forApplyFailure: failure) { return result }
            }
        }
        return .transientFailure(reason: "apply_unconfirmed")
    }

    private static func result(forReadFailure failure: CoreFailure) -> JobCapabilityResult {
        switch failure.code {
        case "transcription_lease_not_held", "handle_closed", "client_shut_down":
            return .interrupted
        default:
            break
        }
        switch failure.errorClass {
        case .cancelled:
            return .interrupted
        case .transient:
            return .transientFailure(reason: label(failure.code))
        default:
            return .permanentFailure(reason: label(failure.code))
        }
    }

    /// The final result for an apply failure, or nil when repeating the same apply is right.
    private static func result(forApplyFailure failure: CoreFailure) -> JobCapabilityResult? {
        switch failure.code {
        case "transcription_lease_not_held", "handle_closed", "client_shut_down":
            return .interrupted
        case "transcription_not_committed", "transcription_commit_unknown":
            return nil
        default:
            break
        }
        switch failure.errorClass {
        case .cancelled:
            return .interrupted
        case .transient:
            return nil
        default:
            return .permanentFailure(reason: label(failure.code))
        }
    }

    /// A failure reason is stored on the job, so it is a short lowercase label and never content.
    static func label(_ value: String) -> String {
        let scalars = value.lowercased().unicodeScalars.prefix(64).map { scalar -> Character in
            switch scalar {
            case "a"..."z", "0"..."9", "_":
                return Character(scalar)
            default:
                return "_"
            }
        }
        return scalars.isEmpty ? "unknown" : String(scalars)
    }
}

extension TranscriptionOutcomeRequest {
    fileprivate func replacing(
        _ result: TranscriptionResultRequest,
        interpretation: InterpretationJobRequest? = nil
    ) -> TranscriptionOutcomeRequest {
        TranscriptionOutcomeRequest(
            jobID: jobID, leaseAttempt: leaseAttempt, audioReference: audioReference, result: result,
            interpretation: interpretation)
    }
}

/// Answers the awaiting caller once, with the recognizer's outcome or, when the deadline wins, with nil.
private final class RecognitionDeadlineRace: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<OnDeviceRecognitionOutcome?, Never>?
    private var work: Task<Void, Never>?
    private var deadline: Task<Void, Never>?
    private var isResolved = false
    private var isCancelRequested = false

    func install(_ continuation: CheckedContinuation<OnDeviceRecognitionOutcome?, Never>) {
        lock.lock()
        self.continuation = continuation
        lock.unlock()
    }

    func attach(work: Task<Void, Never>, deadline: Task<Void, Never>) {
        lock.lock()
        let finished = isResolved
        let cancelWork = isCancelRequested
        if !finished {
            self.work = work
            self.deadline = deadline
        }
        lock.unlock()
        if finished {
            work.cancel()
            deadline.cancel()
        } else if cancelWork {
            work.cancel()
        }
    }

    /// Asks the recognizer to stop; the outcome it then returns is still awaited.
    func cancelRecognition() {
        lock.lock()
        isCancelRequested = true
        let running = work
        lock.unlock()
        running?.cancel()
    }

    func resolve(_ outcome: OnDeviceRecognitionOutcome?) {
        lock.lock()
        guard !isResolved, let pending = continuation else {
            lock.unlock()
            return
        }
        isResolved = true
        continuation = nil
        let runningWork = work
        let runningDeadline = deadline
        work = nil
        deadline = nil
        lock.unlock()
        runningWork?.cancel()
        runningDeadline?.cancel()
        pending.resume(returning: outcome)
    }
}
