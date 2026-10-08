import AVFoundation
import Foundation

struct AudioRecordingLimits {
    var maxDurationSeconds: TimeInterval = 300
    var minimumFreeBytes: Int64 = 50_000_000
}

enum AudioRecordingReason {
    static let interrupted = "Audio interrupted"
    static let cancelled = "Cancelled"
    static let finalizationFailed = "Recording finalization failed"
    static let noActiveRecording = "No active recording"
    static let noRecoverableAudio = "no recoverable audio"
}

enum AudioRecordingOutcome: Equatable {
    case saved
    case partial(reason: String)
    case failed(reason: String)
}

struct AudioRecordingResult {
    let outcome: AudioRecordingOutcome
    let durationSeconds: Double
    let filePath: String?
    let fileSize: Int
    let fileProtection: String?

    var success: Bool { outcome == .saved }

    var interruption: String? {
        switch outcome {
        case .saved: return nil
        case .partial(let reason), .failed(let reason): return reason
        }
    }

    var description: String {
        let protection = fileProtection.map { ", protection \($0)" } ?? ""
        let seconds = String(format: "%.2f", durationSeconds)
        switch outcome {
        case .saved:
            return "Saved: \(seconds)s, \(fileSize) bytes\(protection)"
        case .partial(let reason):
            return "Partial: \(seconds)s, \(fileSize) bytes, \(reason)\(protection)"
        case .failed(let reason):
            return "Failed: \(reason)"
        }
    }

    static func failed(_ reason: String) -> AudioRecordingResult {
        AudioRecordingResult(outcome: .failed(reason: reason), durationSeconds: 0, filePath: nil, fileSize: 0, fileProtection: nil)
    }
}

struct AudioCaptureError: Error {
    let reason: String
}

protocol AudioCaptureEngine: AnyObject {
    func start(to destination: URL, maxDuration: TimeInterval) throws
    /// Closes the file synchronously. Returns false when the engine itself reported a failure.
    func stopAndClose() -> Bool
}

/// Main-thread API. A success is reported only when the closed file is read back as non-empty audio.
final class AudioRecorder {
    private final class Session {
        let destination: URL
        let startedAt: Date
        var endedEarlyResult: AudioRecordingResult?

        init(destination: URL, startedAt: Date) {
            self.destination = destination
            self.startedAt = startedAt
        }
    }

    private struct RecoveredAudio {
        let frames: Int64
        let sampleRate: Double
        let fileSize: Int
        let protection: String?
    }

    private let engine: AudioCaptureEngine
    private let limits: AudioRecordingLimits
    private let freeSpaceProvider: (URL) -> Int64?
    private let notificationCenter: NotificationCenter
    private var interruptionObserver: NSObjectProtocol?
    private var session: Session?

    private(set) var lastStartFailure: String?
    var onSessionEndedEarly: ((AudioRecordingResult) -> Void)?

    var isRecording: Bool { session != nil && session?.endedEarlyResult == nil }
    var sessionStartTime: Date? { session?.startedAt }

    init(
        engine: AudioCaptureEngine,
        limits: AudioRecordingLimits = AudioRecordingLimits(),
        freeSpaceProvider: @escaping (URL) -> Int64? = AudioRecorder.availableCapacity,
        notificationCenter: NotificationCenter = .default
    ) {
        self.engine = engine
        self.limits = limits
        self.freeSpaceProvider = freeSpaceProvider
        self.notificationCenter = notificationCenter
        interruptionObserver = notificationCenter.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: nil
        ) { [weak self] notification in
            let typeValue = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            if Thread.isMainThread {
                self?.handleInterruption(typeValue: typeValue)
            } else {
                DispatchQueue.main.async { self?.handleInterruption(typeValue: typeValue) }
            }
        }
    }

    deinit {
        if let interruptionObserver = interruptionObserver {
            notificationCenter.removeObserver(interruptionObserver)
        }
    }

    static func availableCapacity(at directory: URL) -> Int64? {
        let values = try? directory.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        return values?.volumeAvailableCapacityForImportantUsage
    }

    func startRecording(to destination: URL) -> Bool {
        if isRecording {
            lastStartFailure = "Recording already active"
            return false
        }
        session = nil

        guard let freeBytes = freeSpaceProvider(destination.deletingLastPathComponent()) else {
            lastStartFailure = "Free space unknown"
            return false
        }
        guard freeBytes >= limits.minimumFreeBytes else {
            lastStartFailure = "Insufficient free space"
            return false
        }

        do {
            try engine.start(to: destination, maxDuration: limits.maxDurationSeconds)
        } catch let captureError as AudioCaptureError {
            lastStartFailure = captureError.reason
            return false
        } catch {
            lastStartFailure = "Record start failed: \(error)"
            return false
        }

        lastStartFailure = nil
        session = Session(destination: destination, startedAt: Date())
        return true
    }

    func stopRecording() -> AudioRecordingResult {
        endSession(endReason: nil)
    }

    func cancelRecording() -> AudioRecordingResult {
        endSession(endReason: AudioRecordingReason.cancelled)
    }

    private func endSession(endReason: String?) -> AudioRecordingResult {
        guard let activeSession = session else {
            return .failed(AudioRecordingReason.noActiveRecording)
        }
        session = nil
        if let earlyResult = activeSession.endedEarlyResult {
            return earlyResult
        }
        return finalize(activeSession, endReason: endReason)
    }

    private func handleInterruption(typeValue: UInt?) {
        guard let typeValue = typeValue,
              AVAudioSession.InterruptionType(rawValue: typeValue) == .began,
              let activeSession = session,
              activeSession.endedEarlyResult == nil else {
            return
        }
        // Never resume into the same URL: restarting the recorder would truncate the saved prefix.
        let result = finalize(activeSession, endReason: AudioRecordingReason.interrupted)
        activeSession.endedEarlyResult = result
        onSessionEndedEarly?(result)
    }

    private func finalize(_ endedSession: Session, endReason: String?) -> AudioRecordingResult {
        let closedCleanly = engine.stopAndClose()

        guard let recovered = Self.recoverAudio(at: endedSession.destination) else {
            let reason = endReason.map { "\($0); \(AudioRecordingReason.noRecoverableAudio)" }
                ?? (closedCleanly ? AudioRecordingReason.noRecoverableAudio : AudioRecordingReason.finalizationFailed)
            return .failed(reason)
        }

        let outcome: AudioRecordingOutcome
        if let endReason = endReason {
            outcome = .partial(reason: endReason)
        } else if !closedCleanly {
            outcome = .partial(reason: AudioRecordingReason.finalizationFailed)
        } else {
            outcome = .saved
        }

        return AudioRecordingResult(
            outcome: outcome,
            durationSeconds: Double(recovered.frames) / recovered.sampleRate,
            filePath: endedSession.destination.path,
            fileSize: recovered.fileSize,
            fileProtection: recovered.protection
        )
    }

    private static func recoverAudio(at url: URL) -> RecoveredAudio? {
        guard let audioFile = try? AVAudioFile(forReading: url), audioFile.length > 0 else {
            return nil
        }
        let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
        let fileSize = (attributes?[.size] as? NSNumber)?.intValue ?? 0
        guard fileSize > 0 else {
            return nil
        }
        let protection = (attributes?[.protectionKey] as? FileProtectionType)?.rawValue
        return RecoveredAudio(
            frames: audioFile.length,
            sampleRate: audioFile.processingFormat.sampleRate,
            fileSize: fileSize,
            protection: protection
        )
    }
}

final class AVAudioRecorderEngine: NSObject, AudioCaptureEngine, AVAudioRecorderDelegate {
    private enum Permission {
        case granted
        case denied
        case undetermined
    }

    private var recorder: AVAudioRecorder?
    private var reportedFailure = false

    private static func currentPermission() -> Permission {
        if #available(iOS 17.0, *) {
            switch AVAudioApplication.shared.recordPermission {
            case .granted: return .granted
            case .denied: return .denied
            default: return .undetermined
            }
        } else {
            switch AVAudioSession.sharedInstance().recordPermission {
            case .granted: return .granted
            case .denied: return .denied
            default: return .undetermined
            }
        }
    }

    func start(to destination: URL, maxDuration: TimeInterval) throws {
        switch Self.currentPermission() {
        case .granted: break
        case .denied: throw AudioCaptureError(reason: "Microphone permission denied")
        case .undetermined: throw AudioCaptureError(reason: "Microphone permission not granted")
        }

        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false
        ]

        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .default)
            try session.setActive(true)
            let newRecorder = try AVAudioRecorder(url: destination, settings: settings)
            newRecorder.delegate = self
            reportedFailure = false
            guard newRecorder.record(forDuration: maxDuration) else {
                throw AudioCaptureError(reason: "Recording start failed")
            }
            recorder = newRecorder
        } catch let captureError as AudioCaptureError {
            throw captureError
        } catch {
            throw AudioCaptureError(reason: "Record start failed: \(error)")
        }
    }

    func stopAndClose() -> Bool {
        guard let activeRecorder = recorder else {
            return false
        }
        recorder = nil
        activeRecorder.stop()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        return !reportedFailure
    }

    func audioRecorderDidFinishRecording(_ finishedRecorder: AVAudioRecorder, successfully flag: Bool) {
        if !flag && finishedRecorder === recorder {
            reportedFailure = true
        }
    }

    func audioRecorderEncodeErrorDidOccur(_ erroredRecorder: AVAudioRecorder, error: Error?) {
        if erroredRecorder === recorder {
            reportedFailure = true
        }
    }
}
