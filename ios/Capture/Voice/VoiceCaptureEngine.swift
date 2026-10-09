import AVFoundation
import Foundation

enum VoiceEngineEvent: Equatable {
    /// The recorder stopped itself at the duration it was given.
    case reachedDurationLimit
    case failed
}

struct VoiceEngineStartError: Error, Equatable {
    enum Reason: Equatable {
        case microphoneDenied
        case unavailable
    }

    var reason: Reason
}

/// The device recorder behind the session. Tests inject a synthetic engine to produce failures a simulator cannot.
protocol VoiceCaptureEngine: AnyObject {
    var onEvent: ((VoiceEngineEvent) -> Void)? { get set }
    /// Starts writing audio to `destination`, which the caller has checked does not exist.
    func start(writingTo destination: URL, maxDuration: TimeInterval) throws
    /// Closes the file. Returns false unless the engine confirmed a successful finish.
    func stopAndClose() -> Bool
}

enum MicrophonePermissionStatus: Equatable {
    case granted
    case denied
    case undetermined
}

protocol MicrophonePermissionProviding: AnyObject {
    var status: MicrophonePermissionStatus { get }
    /// Asks the user. `completion` runs on the main queue.
    func request(completion: @escaping (Bool) -> Void)
}

final class SystemMicrophonePermission: MicrophonePermissionProviding {
    var status: MicrophonePermissionStatus {
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

    func request(completion: @escaping (Bool) -> Void) {
        let deliver: (Bool) -> Void = { granted in
            if Thread.isMainThread {
                completion(granted)
            } else {
                DispatchQueue.main.async { completion(granted) }
            }
        }
        if #available(iOS 17.0, *) {
            AVAudioApplication.requestRecordPermission(completionHandler: deliver)
        } else {
            AVAudioSession.sharedInstance().requestRecordPermission(deliver)
        }
    }
}

/// Records 16 kHz mono 16-bit linear PCM straight into the destination with `AVAudioRecorder`. The file grows as audio
/// arrives, so an interruption leaves a readable prefix. Confined to the main queue.
final class AVAudioRecorderVoiceEngine: NSObject, VoiceCaptureEngine, AVAudioRecorderDelegate {
    var onEvent: ((VoiceEngineEvent) -> Void)?

    private let permission: MicrophonePermissionProviding
    private let finishWaitSeconds: TimeInterval
    private var recorder: AVAudioRecorder?
    private var isClosing = false
    private var reportedFailure = false
    private var finishReported = false

    init(permission: MicrophonePermissionProviding = SystemMicrophonePermission(), finishWaitSeconds: TimeInterval = 1.0) {
        self.permission = permission
        self.finishWaitSeconds = finishWaitSeconds
        super.init()
    }

    func start(writingTo destination: URL, maxDuration: TimeInterval) throws {
        guard permission.status == .granted else {
            throw VoiceEngineStartError(reason: .microphoneDenied)
        }
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false,
        ]
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .default)
            try session.setActive(true)
            let newRecorder = try AVAudioRecorder(url: destination, settings: settings)
            newRecorder.delegate = self
            isClosing = false
            reportedFailure = false
            finishReported = false
            recorder = newRecorder
            guard newRecorder.record(forDuration: maxDuration) else {
                recorder = nil
                throw VoiceEngineStartError(reason: .unavailable)
            }
        } catch let startError as VoiceEngineStartError {
            throw startError
        } catch {
            throw VoiceEngineStartError(reason: .unavailable)
        }
    }

    func stopAndClose() -> Bool {
        guard let activeRecorder = recorder else { return false }
        isClosing = true
        activeRecorder.stop()
        awaitFinishCallback()
        recorder = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        return finishReported && !reportedFailure
    }

    /// Serves the run loop instead of blocking so a delegate callback queued on the main thread can arrive.
    private func awaitFinishCallback() {
        let deadline = Date().addingTimeInterval(finishWaitSeconds)
        while !finishReported && Date() < deadline {
            let sliceEnd = min(deadline, Date().addingTimeInterval(0.01))
            if !RunLoop.current.run(mode: .default, before: sliceEnd) {
                Thread.sleep(forTimeInterval: 0.005)
            }
        }
    }

    func audioRecorderDidFinishRecording(_ finishedRecorder: AVAudioRecorder, successfully flag: Bool) {
        runOnMain { [self] in
            guard finishedRecorder === recorder else { return }
            finishReported = true
            if !flag { reportedFailure = true }
            guard !isClosing else { return }
            onEvent?(flag ? .reachedDurationLimit : .failed)
        }
    }

    func audioRecorderEncodeErrorDidOccur(_ erroredRecorder: AVAudioRecorder, error: Error?) {
        runOnMain { [self] in
            guard erroredRecorder === recorder else { return }
            reportedFailure = true
            guard !isClosing else { return }
            onEvent?(.failed)
        }
    }

    private func runOnMain(_ work: @escaping () -> Void) {
        if Thread.isMainThread {
            work()
        } else {
            DispatchQueue.main.async(execute: work)
        }
    }
}
