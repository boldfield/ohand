import Foundation
import OhAndServices

/// Gives the voice recorder's handoff seam the composition root's recording save: staged durably, finalized by move,
/// then imported in one transaction. Only a confirmed import is `saved`.
final class RootVoiceHandoff: VoiceRecordingHandoffReceiving {
    typealias Save = (
        _ handoff: CompositionRecordingHandoff, _ completion: @escaping (CompositionRecordingOutcome) -> Void
    ) -> Void

    private let save: Save

    init(save: @escaping Save) {
        self.save = save
    }

    convenience init(root: ServiceCompositionRoot) {
        self.init(save: { [weak root] handoff, completion in
            guard let root else {
                completion(.notStaged(captureID: handoff.captureID))
                return
            }
            root.saveRecording(handoff, completion: completion)
        })
    }

    func handOff(_ handoff: VoiceRecordingHandoff, completion: @escaping (VoiceHandoffResult) -> Void) {
        let rootHandoff = CompositionRecordingHandoff(
            captureID: handoff.captureID,
            inProgressFileName: handoff.inProgressFileName,
            finalizedFileName: handoff.finalizedFileName,
            startedAt: handoff.startedAt)
        save(rootHandoff) { outcome in
            completion(Self.result(for: outcome))
        }
    }

    static func result(for outcome: CompositionRecordingOutcome) -> VoiceHandoffResult {
        switch outcome {
        case .saved(_, let itemID, let savedAt, let alreadySaved):
            return .saved(itemID: itemID, savedAt: savedAt, alreadyImported: alreadySaved)
        case .keptOnDevice:
            return .keptForRetry
        case .notStaged:
            return .notStaged
        case .audioUnavailable:
            return .stagedAudioUnavailable
        case .itemDeleted:
            return .itemDeleted
        }
    }
}

/// Gives the recovery coordinator the root's restart recovery pass, reported in the recorder's terms. A report whose
/// ownership of the in-progress files is unknown keeps that unknown as nil.
final class RootVoiceRecovery: VoiceRecoveryIngress {
    typealias Recover = (_ completion: @escaping (CompositionRecoveryReport?) -> Void) -> Void

    private let recover: Recover

    init(recover: @escaping Recover) {
        self.recover = recover
    }

    convenience init(root: ServiceCompositionRoot) {
        self.init(recover: { [weak root] completion in
            guard let root else {
                completion(nil)
                return
            }
            root.recoverRecordings(completion: completion)
        })
    }

    func recoverStagedCaptures(completion: @escaping (VoiceIngressRecoveryReport) -> Void) {
        recover { report in
            completion(Self.report(for: report))
        }
    }

    /// A root that is shut down answers nil. Nothing was inspected, so ownership is reported unknown, never "none".
    static func report(for report: CompositionRecoveryReport?) -> VoiceIngressRecoveryReport {
        guard let report else {
            return VoiceIngressRecoveryReport(unclaimedInProgressFileNames: nil)
        }
        return VoiceIngressRecoveryReport(
            pending: report.pending.map { VoicePendingCapture(captureID: $0.captureID, reason: reason(for: $0.reason)) },
            confirmedCaptureIDs: report.confirmedCaptureIDs,
            deletedItemCaptureIDs: report.deletedItemCaptureIDs,
            unclaimedInProgressFileNames: report.unclaimedInProgressFileNames)
    }

    private static func reason(for reason: CompositionPendingReason) -> VoicePendingReason {
        switch reason {
        case .importUnconfirmed: return .importUnconfirmed
        case .coreRejected(let code): return .coreRejected(code: code)
        case .audioUnavailable: return .audioUnavailable
        case .stagingRecordUnreadable: return .stagingRecordUnreadable
        }
    }
}

/// Lets the capture surface and the recovery surface both follow the one recorder. The controller has a single
/// callback slot, and a continuation started from recovery ends through it just like a recording started on capture.
final class VoiceRecorderFanOut: VoiceRecordingControlling {
    private let controller: VoiceRecordingController

    var state: VoiceRecorderState { controller.state }
    var onStateChange: ((VoiceRecorderState) -> Void)?
    var onOutcome: ((VoiceCaptureOutcome) -> Void)?

    init(controller: VoiceRecordingController, recoveryModel: VoiceRecoveryModel) {
        self.controller = controller
        controller.onStateChange = { [weak self, weak recoveryModel] state in
            self?.onStateChange?(state)
            recoveryModel?.recorderStateChanged(state)
        }
        controller.onOutcome = { [weak self, weak recoveryModel] outcome in
            self?.onOutcome?(outcome)
            recoveryModel?.recorderEnded(with: outcome)
        }
    }

    func start(completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void) {
        controller.start(completion: completion)
    }

    func stop() {
        controller.stop()
    }

    func cancel() {
        controller.cancel()
    }
}

/// The voice recorder and its restart recovery, built over the root's durable recording path.
final class VoiceCaptureServices {
    let controller: VoiceRecordingController
    let recovery: VoiceRecoveryCoordinator
    let recoveryModel: VoiceRecoveryModel
    let recorder: VoiceRecordingControlling

    init(
        root: ServiceCompositionRoot,
        engine: VoiceCaptureEngine = AVAudioRecorderVoiceEngine(),
        permission: MicrophonePermissionProviding = SystemMicrophonePermission(),
        environment: VoiceRecordingEnvironment = .live(protectRecordingFile: VoiceCaptureServices.protectRecordingFile),
        notificationCenter: NotificationCenter = .default
    ) {
        let handoff = RootVoiceHandoff(root: root)
        let controller = VoiceRecordingController(
            inProgressDirectory: root.inProgressAudioDirectory,
            engine: engine,
            permission: permission,
            handoff: handoff,
            environment: environment,
            notificationCenter: notificationCenter)
        let recovery = VoiceRecoveryCoordinator(
            inProgressDirectory: root.inProgressAudioDirectory,
            ingress: RootVoiceRecovery(root: root),
            handoff: handoff,
            controller: controller,
            environment: environment)
        self.controller = controller
        self.recovery = recovery
        let recoveryModel = VoiceRecoveryModel(coordinator: recovery, controller: controller)
        self.recoveryModel = recoveryModel
        recorder = VoiceRecorderFanOut(controller: controller, recoveryModel: recoveryModel)
    }

    /// The in-progress audio store's protection class and backup treatment, applied to each new recording file
    /// (`docs/architecture/m1-contracts.md`, data protection): readable while open across a lock, never backed up.
    static func protectRecordingFile(_ url: URL) throws {
        try FileManager.default.setAttributes(
            [.protectionKey: FileProtectionType.completeUnlessOpen], ofItemAtPath: url.path)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableURL = url
        try mutableURL.setResourceValues(values)
    }
}
