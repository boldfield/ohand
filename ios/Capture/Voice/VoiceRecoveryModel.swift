import Foundation

/// State behind the recovery surface: what re-entry found, the one action in flight, and the honest result of the last
/// action. It only reads the listing and relays the user's explicit choice for exactly one recording to the coordinator;
/// it never acts on its own, and it never gates normal capture.
///
/// Confined to the main queue.
final class VoiceRecoveryModel: ObservableObject {
    @Published private(set) var rows: [VoiceRecoveryRow] = []
    @Published private(set) var notices: [String] = []
    /// The result of the last action or of the last recording that ended; nil before anything happened.
    @Published private(set) var status: String?
    @Published private(set) var isChecking = false
    /// The recording an action is running for. Only one runs at a time.
    @Published private(set) var busyFileName: String?
    /// A delete the user asked for and has not confirmed yet. Nothing is removed until `confirmDeletion`.
    @Published private(set) var deletionRequestFileName: String?
    @Published private(set) var recorderState: VoiceRecorderState

    private let coordinator: VoiceRecoveryCoordinator
    private let controller: VoiceRecordingController
    private var listing = VoiceRecoveryListing()
    private var refreshGeneration = 0

    init(coordinator: VoiceRecoveryCoordinator, controller: VoiceRecordingController) {
        self.coordinator = coordinator
        self.controller = controller
        recorderState = controller.state
    }

    var isRecording: Bool {
        if case .recording = recorderState { return true }
        return false
    }

    func isEnabled(_ action: VoiceRecoveryAction) -> Bool {
        guard busyFileName == nil else { return false }
        return action != .continueRecording || recorderState == .idle
    }

    // MARK: Re-entry

    /// Looks for unfinished recordings. Call on launch and whenever the recovery surface appears; the answer replaces
    /// the previous list and a slower, older answer never overwrites a newer one.
    func refresh() {
        refreshGeneration += 1
        let generation = refreshGeneration
        isChecking = true
        coordinator.refresh { [weak self] newListing in
            guard let self = self, generation == self.refreshGeneration else { return }
            self.listing = newListing
            self.rows = VoiceRecoveryMessages.rows(for: newListing)
            self.notices = VoiceRecoveryMessages.notices(for: newListing)
            self.isChecking = false
            if let requested = self.deletionRequestFileName, !newListing.recordings.contains(where: { $0.fileName == requested }) {
                self.deletionRequestFileName = nil
            }
        }
    }

    // MARK: Actions

    func perform(_ action: VoiceRecoveryAction, onFileName fileName: String) {
        guard busyFileName == nil else {
            status = VoiceRecoveryMessages.message(for: VoiceRecoveryFailure.alreadyInProgress)
            return
        }
        guard let recording = listing.recordings.first(where: { $0.fileName == fileName }) else {
            status = VoiceRecoveryMessages.message(for: VoiceRecoveryFailure.recordingNotFound)
            refresh()
            return
        }
        switch action {
        case .finish:
            run(on: fileName) { finished in
                self.coordinator.finish(recording) { result in
                    switch result {
                    case .success(let outcome): self.status = VoiceRecoveryMessages.message(for: outcome)
                    case .failure(let failure): self.status = VoiceRecoveryMessages.message(for: failure)
                    }
                    finished()
                }
            }
        case .continueRecording:
            guard recorderState == .idle else {
                status = VoiceRecoveryMessages.message(for: VoiceStartFailure.alreadyActive)
                return
            }
            run(on: fileName) { finished in
                self.coordinator.continueRecording(recording) { result in
                    switch result {
                    case .success:
                        self.status = "Recording more audio onto this recording. Stop to add it, or cancel to keep what was recorded."
                        finished()
                    case .failure(let failure):
                        self.status = VoiceRecoveryMessages.message(for: failure)
                        finished()
                    }
                }
            }
        case .delete:
            deletionRequestFileName = fileName
        }
    }

    func cancelDeletion() {
        deletionRequestFileName = nil
    }

    /// Deletes the one recording the user asked to delete and confirmed. Ignored unless it is the pending request.
    func confirmDeletion(fileName: String) {
        guard deletionRequestFileName == fileName else { return }
        deletionRequestFileName = nil
        guard busyFileName == nil else {
            status = VoiceRecoveryMessages.message(for: VoiceRecoveryFailure.alreadyInProgress)
            return
        }
        guard let recording = listing.recordings.first(where: { $0.fileName == fileName }) else {
            status = VoiceRecoveryMessages.message(for: VoiceRecoveryFailure.recordingNotFound)
            refresh()
            return
        }
        run(on: fileName) { finished in
            self.coordinator.delete(recording) { result in
                switch result {
                case .success(let deleted): self.status = VoiceRecoveryMessages.message(for: deleted)
                case .failure(let failure): self.status = VoiceRecoveryMessages.message(for: failure)
                }
                finished()
            }
        }
    }

    /// Marks one recording busy until `work` calls its completion, then checks the list again.
    private func run(on fileName: String, _ work: (_ finished: @escaping () -> Void) -> Void) {
        busyFileName = fileName
        work { [weak self] in
            guard let self = self else { return }
            self.busyFileName = nil
            self.refresh()
        }
    }

    // MARK: Recorder

    /// Forward `VoiceRecordingController.onStateChange` here so the surface can offer to stop a continuation.
    func recorderStateChanged(_ state: VoiceRecorderState) {
        recorderState = state
    }

    /// Forward `VoiceRecordingController.onOutcome` here: the result is stated honestly and the list is checked again.
    func recorderEnded(with outcome: VoiceCaptureOutcome) {
        status = VoiceRecoveryMessages.message(for: outcome)
        refresh()
    }

    func stopRecording() {
        controller.stop()
    }

    func cancelRecording() {
        controller.cancel()
    }
}
