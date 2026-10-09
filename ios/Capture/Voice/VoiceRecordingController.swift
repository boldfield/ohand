import AVFoundation
import Foundation
import UIKit

/// One explicit, foreground voice recording at a time, written incrementally into the protected in-progress audio
/// store and handed to the foreground ingress writer when it ends. Recording starts only from `start`, never in the
/// background, and uses no network: the controller holds the file and nothing else.
///
/// Every way a recording can end (user stop, user cancel, duration or size limit, low storage, audio interruption,
/// leaving the foreground, device lock, recorder failure) closes the file first, reads it back, and then reports one
/// honest outcome. A readable recording is handed off and is never reported as saved before the core confirms it. A
/// recording that cannot be read back leaves its bytes in place and says so. Nothing here deletes audio except an empty
/// file that never held any and, after a verified join, the added segment whose audio now lives in the recording it
/// continued.
///
/// Confined to the main queue. Callbacks arrive there.
final class VoiceRecordingController {
    /// The existing recording a continuation extends. It stays untouched until the added audio is verified joined.
    private struct ContinuationBase {
        let url: URL
        let frames: Int64
        let sampleRate: Double
        let fileSizeBytes: Int64

        var durationSeconds: Double { Double(frames) / sampleRate }
    }

    private struct ActiveSession {
        let captureID: String
        /// The name the finished recording keeps. For a continuation this is the continued recording's name.
        let fileName: String
        /// The file the engine writes. For a continuation this is the added segment, not the finished recording.
        let url: URL
        let startedAt: Date
        let protectionApplied: Bool
        let stopPolling: () -> Void
        let base: ContinuationBase?
    }

    private struct JoinedRecording {
        let frames: Int64
        let sampleRate: Double
        let fileSizeBytes: Int64
        let protectionApplied: Bool
    }

    private let inProgressDirectory: URL
    private let engine: VoiceCaptureEngine
    private let permission: MicrophonePermissionProviding
    private let handoffReceiver: VoiceRecordingHandoffReceiving
    private let limits: VoiceRecordingLimits
    private let environment: VoiceRecordingEnvironment
    private let notificationCenter: NotificationCenter
    private let makeCaptureID: () -> String
    private var observers: [NSObjectProtocol] = []
    private var activeSession: ActiveSession?

    private(set) var state: VoiceRecorderState = .idle {
        didSet { if state != oldValue { onStateChange?(state) } }
    }
    /// Closed recordings whose handoff has not answered yet.
    private(set) var pendingHandoffCount = 0
    private(set) var lastOutcome: VoiceCaptureOutcome?

    /// Names of the files the active recording owns, so recovery never offers them while they are being written.
    var activeFileNames: Set<String> {
        guard let session = activeSession else { return [] }
        return [session.fileName, session.url.lastPathComponent]
    }

    var onStateChange: ((VoiceRecorderState) -> Void)?
    /// Called once per recording that ended, whatever ended it. Early ends (limit, interruption, lock) arrive here
    /// without any user action.
    var onOutcome: ((VoiceCaptureOutcome) -> Void)?

    init(
        inProgressDirectory: URL,
        engine: VoiceCaptureEngine,
        permission: MicrophonePermissionProviding,
        handoff: VoiceRecordingHandoffReceiving,
        environment: VoiceRecordingEnvironment,
        limits: VoiceRecordingLimits = VoiceRecordingLimits(),
        notificationCenter: NotificationCenter = .default,
        makeCaptureID: @escaping () -> String = VoiceRecordingController.generateCaptureID
    ) {
        self.inProgressDirectory = inProgressDirectory
        self.engine = engine
        self.permission = permission
        self.handoffReceiver = handoff
        self.environment = environment
        self.limits = limits
        self.notificationCenter = notificationCenter
        self.makeCaptureID = makeCaptureID
        engine.onEvent = { [weak self] event in self?.handleEngineEvent(event) }
        observe(AVAudioSession.interruptionNotification) { [weak self] notification in
            let typeValue = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            if typeValue.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) == .began {
                self?.endRecording(.audioInterrupted)
            }
        }
        observe(UIApplication.didEnterBackgroundNotification) { [weak self] _ in self?.endRecording(.leftForeground) }
        observe(UIApplication.protectedDataWillBecomeUnavailableNotification) { [weak self] _ in
            self?.endRecording(.deviceLocking)
        }
    }

    deinit {
        observers.forEach { notificationCenter.removeObserver($0) }
        activeSession?.stopPolling()
    }

    static func generateCaptureID() -> String {
        "voice-" + UUID().uuidString.lowercased()
    }

    // MARK: Start

    /// Starts one recording after an explicit user action. Completes synchronously unless the microphone permission
    /// prompt has to be shown.
    func start(completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void) {
        startSession(continuing: nil, completion: completion)
    }

    /// Adds audio to a recording found in the in-progress store, within the same duration and size bounds as a new
    /// recording. The added audio is written to its own file and joined only after it reads back; until then, and if
    /// the join fails, the existing recording is untouched. Explicit and foreground-only, like `start`.
    func continueRecording(
        _ recording: RecoverableVoiceRecording,
        completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void
    ) {
        startSession(continuing: recording, completion: completion)
    }

    private func startSession(
        continuing recording: RecoverableVoiceRecording?,
        completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void
    ) {
        guard state == .idle else {
            completion(.failure(.alreadyActive))
            return
        }
        guard environment.isForeground() else {
            completion(.failure(.notInForeground))
            return
        }
        switch permission.status {
        case .denied:
            completion(.failure(.microphoneDenied))
        case .granted:
            completion(begin(continuing: recording))
        case .undetermined:
            state = .requestingPermission
            permission.request { [weak self] granted in
                guard let self = self else { return }
                self.state = .idle
                guard granted else {
                    completion(.failure(.microphoneDenied))
                    return
                }
                guard self.environment.isForeground() else {
                    completion(.failure(.notInForeground))
                    return
                }
                completion(self.begin(continuing: recording))
            }
        }
    }

    private func begin(continuing recording: RecoverableVoiceRecording?) -> Result<VoiceRecordingStarted, VoiceStartFailure> {
        guard state == .idle else { return .failure(.alreadyActive) }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: inProgressDirectory.path, isDirectory: &isDirectory),
              isDirectory.boolValue else {
            return .failure(.storageUnavailable)
        }
        guard let freeBytes = environment.freeSpaceBytes(inProgressDirectory) else {
            return .failure(.storageSpaceUnknown)
        }
        guard freeBytes >= limits.minimumFreeBytesToStart else {
            return .failure(.insufficientStorage)
        }

        let captureID: String
        let fileName: String
        let url: URL
        var base: ContinuationBase?
        var maxDuration = limits.maxDurationSeconds
        if let recording = recording {
            captureID = recording.captureID
            fileName = recording.fileName
            guard Self.isSafeCaptureID(captureID), fileName == "\(captureID).wav" else {
                return .failure(.recordingNotContinuable)
            }
            let existingURL = inProgressDirectory.appendingPathComponent(fileName)
            guard let existingBytes = environment.fileSizeBytes(existingURL), existingBytes > 0,
                  let inspection = environment.inspectAudio(existingURL), inspection.sampleRate > 0 else {
                return .failure(.recordingNotContinuable)
            }
            let existing = ContinuationBase(
                url: existingURL, frames: inspection.frames, sampleRate: inspection.sampleRate, fileSizeBytes: existingBytes)
            maxDuration = limits.maxDurationSeconds - existing.durationSeconds
            let minimumBytes = Int64(limits.minimumContinuationSeconds) * limits.bytesPerSecond
            guard maxDuration >= limits.minimumContinuationSeconds,
                  existingBytes + minimumBytes <= limits.maxFileBytes else {
                return .failure(.nothingLeftToRecord)
            }
            let segmentName = "\(captureID)-continued.wav"
            let joinedName = "\(captureID)-joined.wav"
            url = inProgressDirectory.appendingPathComponent(segmentName)
            guard !FileManager.default.fileExists(atPath: url.path),
                  !FileManager.default.fileExists(atPath: inProgressDirectory.appendingPathComponent(joinedName).path) else {
                return .failure(.storageUnavailable)
            }
            base = existing
        } else {
            captureID = makeCaptureID()
            fileName = "\(captureID).wav"
            url = inProgressDirectory.appendingPathComponent(fileName)
            guard Self.isSafeCaptureID(captureID), !FileManager.default.fileExists(atPath: url.path) else {
                return .failure(.storageUnavailable)
            }
        }

        do {
            try engine.start(writingTo: url, maxDuration: maxDuration)
        } catch let startError as VoiceEngineStartError {
            removeIfEmpty(url)
            return .failure(startError.reason == .microphoneDenied ? .microphoneDenied : .recorderUnavailable)
        } catch {
            removeIfEmpty(url)
            return .failure(.recorderUnavailable)
        }

        var protectionApplied = true
        do {
            try environment.protectRecordingFile(url)
        } catch {
            protectionApplied = false
        }

        let startedAt = environment.now()
        let stopPolling = environment.schedulePoll(limits.pollIntervalSeconds) { [weak self] in self?.poll() }
        activeSession = ActiveSession(
            captureID: captureID, fileName: fileName, url: url, startedAt: startedAt,
            protectionApplied: protectionApplied, stopPolling: stopPolling, base: base)
        state = .recording(captureID: captureID, startedAt: startedAt)
        return .success(VoiceRecordingStarted(captureID: captureID, startedAt: startedAt))
    }

    // MARK: Ending

    /// Ends the recording and keeps the audio. A no-op when nothing is recording.
    func stop() {
        endRecording(.stoppedByUser)
    }

    /// Ends the recording without handing it off. The audio stays in the in-progress store, so a cancel never erases
    /// what was said. Idempotent.
    func cancel() {
        endRecording(.cancelledByUser)
    }

    private func poll() {
        guard case .recording = state, let session = activeSession else { return }
        let baseSeconds = session.base?.durationSeconds ?? 0
        let baseBytes = session.base?.fileSizeBytes ?? 0
        if environment.now().timeIntervalSince(session.startedAt) >= limits.maxDurationSeconds - baseSeconds {
            endRecording(.durationLimit)
        } else if let size = environment.fileSizeBytes(session.url), size + baseBytes >= limits.maxFileBytes {
            endRecording(.sizeLimit)
        } else if let free = environment.freeSpaceBytes(inProgressDirectory), free < limits.minimumFreeBytesWhileRecording {
            endRecording(.lowStorage)
        }
    }

    private func handleEngineEvent(_ event: VoiceEngineEvent) {
        switch event {
        case .reachedDurationLimit: endRecording(.durationLimit)
        case .failed: endRecording(.recorderFailed)
        }
    }

    private func endRecording(_ end: VoiceRecordingEnd) {
        guard case .recording = state, let session = activeSession else { return }
        activeSession = nil
        session.stopPolling()
        state = .finishing(captureID: session.captureID)

        let closedCleanly = engine.stopAndClose()
        let inspection = environment.inspectAudio(session.url)
        let sizeBytes = environment.fileSizeBytes(session.url) ?? 0

        guard let inspection = inspection, inspection.sampleRate > 0, sizeBytes > 0 else {
            let retained = sizeBytes > 0
            if !retained { environment.removeFile(session.url) }
            state = .idle
            if session.base != nil {
                deliver(.continuationNotJoined(
                    captureID: session.captureID, end: end, reason: .noReadableAudio, segmentRetained: retained))
            } else {
                deliver(.unrecoverable(captureID: session.captureID, end: end, fileRetained: retained))
            }
            return
        }

        var frames = inspection.frames
        var sampleRate = inspection.sampleRate
        var finishedSizeBytes = sizeBytes
        var protectionApplied = session.protectionApplied
        var startedAt = session.startedAt
        if let base = session.base {
            guard let joined = join(base: base, segmentURL: session.url, segmentInspection: inspection, session: session) else {
                state = .idle
                deliver(.continuationNotJoined(
                    captureID: session.captureID, end: end, reason: .joinFailed, segmentRetained: true))
                return
            }
            frames = joined.frames
            sampleRate = joined.sampleRate
            finishedSizeBytes = joined.fileSizeBytes
            protectionApplied = joined.protectionApplied
            startedAt = session.startedAt.addingTimeInterval(-base.durationSeconds)
        }

        let summary = VoiceRecordingSummary(
            captureID: session.captureID,
            inProgressFileName: session.fileName,
            startedAt: startedAt,
            durationSeconds: Double(frames) / sampleRate,
            fileSizeBytes: Int(finishedSizeBytes),
            end: end,
            closedCleanly: closedCleanly,
            protectionApplied: protectionApplied)

        guard end != .cancelledByUser else {
            state = .idle
            deliver(.retainedUnsubmitted(summary))
            return
        }

        let handoff = VoiceRecordingHandoff(
            captureID: summary.captureID,
            inProgressFileName: summary.inProgressFileName,
            finalizedFileName: summary.inProgressFileName,
            startedAt: summary.startedAt,
            durationSeconds: summary.durationSeconds,
            fileSizeBytes: summary.fileSizeBytes,
            end: end)
        state = .idle
        pendingHandoffCount += 1
        var answered = false
        handoffReceiver.handOff(handoff) { [weak self] result in
            guard !answered else { return }
            answered = true
            guard let self = self else { return }
            self.pendingHandoffCount -= 1
            self.deliver(Self.outcome(for: result, summary: summary))
        }
    }

    /// Joins the added segment onto the existing recording through a separate file, verifies the result by reading it
    /// back, and only then replaces the existing recording with it and removes the segment. Any failure removes only
    /// the unverified joined file and leaves the existing recording and the segment in place.
    private func join(
        base: ContinuationBase,
        segmentURL: URL,
        segmentInspection: VoiceAudioInspection,
        session: ActiveSession
    ) -> JoinedRecording? {
        let joinedURL = inProgressDirectory.appendingPathComponent("\(session.captureID)-joined.wav")
        guard !FileManager.default.fileExists(atPath: joinedURL.path) else { return nil }
        guard environment.joinAudio(base.url, segmentURL, joinedURL),
              let joinedInspection = environment.inspectAudio(joinedURL),
              joinedInspection.sampleRate == base.sampleRate,
              segmentInspection.sampleRate == base.sampleRate,
              joinedInspection.frames == base.frames + segmentInspection.frames else {
            environment.removeFile(joinedURL)
            return nil
        }
        guard environment.replaceFile(base.url, joinedURL) else {
            environment.removeFile(joinedURL)
            return nil
        }
        environment.removeFile(segmentURL)
        var protectionApplied = true
        do {
            try environment.protectRecordingFile(base.url)
        } catch {
            protectionApplied = false
        }
        return JoinedRecording(
            frames: joinedInspection.frames,
            sampleRate: joinedInspection.sampleRate,
            fileSizeBytes: environment.fileSizeBytes(base.url) ?? 0,
            protectionApplied: protectionApplied)
    }

    static func outcome(for result: VoiceHandoffResult, summary: VoiceRecordingSummary) -> VoiceCaptureOutcome {
        switch result {
        case .saved(let itemID, let savedAt, let alreadyImported):
            return .saved(summary, VoiceSaveAcknowledgment(itemID: itemID, savedAt: savedAt, alreadyImported: alreadyImported))
        case .keptForRetry:
            return .keptForRetry(summary, .importUnconfirmed)
        case .notStaged:
            return .keptForRetry(summary, .notStaged)
        case .itemDeleted:
            return .itemDeleted(summary)
        }
    }

    private func deliver(_ outcome: VoiceCaptureOutcome) {
        lastOutcome = outcome
        onOutcome?(outcome)
    }

    private func removeIfEmpty(_ url: URL) {
        if FileManager.default.fileExists(atPath: url.path), (environment.fileSizeBytes(url) ?? 0) == 0 {
            environment.removeFile(url)
        }
    }

    // MARK: Notifications

    private func observe(_ name: Notification.Name, handler: @escaping (Notification) -> Void) {
        let token = notificationCenter.addObserver(forName: name, object: nil, queue: nil) { notification in
            if Thread.isMainThread {
                handler(notification)
            } else {
                DispatchQueue.main.async { handler(notification) }
            }
        }
        observers.append(token)
    }

    /// Letters, digits, `-` and `_` only: the capture ID names the recording file, so it must not name a path.
    static func isSafeCaptureID(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 128
            && value.unicodeScalars.allSatisfy { scalar in
                switch scalar {
                case "a"..."z", "A"..."Z", "0"..."9", "-", "_": return true
                default: return false
                }
            }
    }
}
