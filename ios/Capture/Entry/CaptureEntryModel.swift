import Combine
import Foundation
import OhAndCoreBridge

/// What the entry surface needs from the voice recorder. `VoiceRecordingController` conforms; tests script it.
/// Confined to the main queue.
protocol VoiceRecordingControlling: AnyObject {
    var state: VoiceRecorderState { get }
    var onStateChange: ((VoiceRecorderState) -> Void)? { get set }
    var onOutcome: ((VoiceCaptureOutcome) -> Void)? { get set }
    func start(completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void)
    func stop()
    func cancel()
    /// The microphone permission right now, read without asking.
    var microphonePermission: MicrophonePermissionStatus { get }
}

extension VoiceRecordingController: VoiceRecordingControlling {}

/// Lets the model see each text outcome (it needs the saved item's ID) while the text model still receives it
/// unchanged.
private final class OutcomeReportingTextCaptureIngress: TextCaptureIngress {
    private let base: TextCaptureIngress
    var willSubmit: (() -> Void)?
    var didFinish: ((TextCaptureOutcome) -> Void)?

    init(base: TextCaptureIngress) {
        self.base = base
    }

    func submitText(_ submission: TextCaptureSubmission, completion: @escaping (TextCaptureOutcome) -> Void) {
        willSubmit?()
        base.submitText(submission) { [weak self] outcome in
            self?.didFinish?(outcome)
            completion(outcome)
        }
    }
}

/// State of the direct capture surface that a system entry opens: native voice or silent capture, then one honest
/// acknowledgment. It holds no management shell, webview or history, so saving never waits for any of them.
///
/// Rules this type keeps:
/// * Capture input is never discarded by an entry: a new presentation switches mode or clears the previous
///   acknowledgment only while nothing is being typed, recorded or saved.
/// * Stored data (the saved item's status) is read only while `access` allows it, re-checked when an answer arrives,
///   and dropped when it stops. Nothing here reads stored data to show history.
/// * The acknowledgment starts as "saved, not processed, no reminder confirmed" and only a successful status read can
///   change that. No other event upgrades it.
/// * Latency is measured on two separate clocks and handed to `metrics`, fire and forget: trigger-to-ready (the
///   system trigger until the surface can take input, `surfaceDidBecomeReady`) and end-of-input-to-save (the user
///   finishing a text save or a recording until a durable acknowledgment is shown). Only a durable end is measured,
///   and nothing about the input is recorded.
///
/// Confined to the main queue.
final class CaptureEntryModel: ObservableObject {
    @Published private(set) var presentation: CaptureEntryPresentation?
    @Published private(set) var mode: CaptureEntryMode
    @Published private(set) var acknowledgment: CaptureAcknowledgment?
    @Published private(set) var recorderState: VoiceRecorderState = .idle
    @Published private(set) var voiceNotice: String?
    @Published private(set) var access: CaptureEntryAccess
    /// True from the moment a recording starts until the view has spoken the start. It is model state, not a view
    /// change, because a cold-launch entry starts recording before the first render.
    @Published private(set) var recordingStartAnnouncementPending = false

    let textModel: TextCaptureModel

    /// Called right after a capture became durable on this device (saved or kept), while still in the foreground. The
    /// composition root connects it to the bounded background completion.
    var captureDidEnd: (() -> Void)?

    private let voice: VoiceRecordingControlling
    private let progressReader: CaptureProgressReading?
    private let currentAccess: () -> CaptureEntryAccess
    private let wording: CaptureAcknowledgmentWording
    private let metrics: MetricsRecording?
    private let uptimeNanoseconds: () -> UInt64
    private var readyReportedEntryID: String?
    private var textInputEndedAt: UInt64?
    private var voiceInputEndedAt: UInt64?
    private var progressGeneration = 0
    private var cancellables: [AnyCancellable] = []

    init(
        textIngress: TextCaptureIngress,
        voice: VoiceRecordingControlling,
        progressReader: CaptureProgressReading? = nil,
        defaultMode: CaptureEntryMode,
        access: @escaping () -> CaptureEntryAccess,
        wording: CaptureAcknowledgmentWording = .standard,
        makeCaptureID: @escaping () -> String = { UUID().uuidString.lowercased() },
        now: @escaping () -> Date = Date.init,
        metrics: MetricsRecording? = nil,
        uptimeNanoseconds: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }
    ) {
        let reporting = OutcomeReportingTextCaptureIngress(base: textIngress)
        self.textModel = TextCaptureModel(ingress: reporting, makeCaptureID: makeCaptureID, now: now)
        self.voice = voice
        self.progressReader = progressReader
        self.currentAccess = access
        self.wording = wording
        self.metrics = metrics
        self.uptimeNanoseconds = uptimeNanoseconds
        self.mode = defaultMode
        self.access = access()

        reporting.willSubmit = { [weak self] in self?.textSaveStarted() }
        reporting.didFinish = { [weak self] outcome in self?.textSaveFinished(outcome) }
        voice.onStateChange = { [weak self] state in self?.recorderStateChanged(to: state) }
        voice.onOutcome = { [weak self] outcome in self?.voiceRecordingEnded(outcome) }
        textModel.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)
        textModel.$text
            .dropFirst()
            .sink { [weak self] newText in self?.textChanged(to: newText) }
            .store(in: &cancellables)
    }

    var acknowledgmentMessage: String? {
        acknowledgment.map { wording.message(for: $0) }
    }

    /// The acknowledgment's words for a value that was just published (`$acknowledgment` publishes before it is set).
    func message(for acknowledgment: CaptureAcknowledgment) -> String {
        wording.message(for: acknowledgment)
    }

    /// Shown before the first unlock after a restart, when protected storage cannot be written yet.
    var protectedDataNotice: String? {
        access.protectedDataAvailable
            ? nil
            : "The device has not been unlocked since it restarted, so saving may fail. What you type or record is kept."
    }

    var isRecording: Bool {
        if case .recording = recorderState { return true }
        return false
    }

    /// Nothing is being typed, recorded or saved, so a new entry may reset the surface.
    var isIdle: Bool {
        recorderState == .idle && textModel.text.isEmpty && !textModel.isSaving && !(acknowledgment?.isSaving ?? false)
    }

    // MARK: Presentation

    func present(_ newPresentation: CaptureEntryPresentation) {
        presentation = newPresentation
        refreshAccess()
        guard isIdle else { return }
        mode = newPresentation.mode
        clearAcknowledgment()
        voiceNotice = nil
        startRecordingIfExplicitlyRequested(newPresentation)
    }

    /// An explicit voice request is the user's own action, so it starts the recording a mic tap would. It never asks
    /// for permission: unless the microphone is already allowed, the voice surface is shown and the mic tap decides.
    private func startRecordingIfExplicitlyRequested(_ presentation: CaptureEntryPresentation) {
        guard presentation.startsRecording, voice.microphonePermission == .granted else { return }
        startRecording()
    }

    /// The capture surface is on screen and can take input. Measures the wait since the system trigger of the current
    /// presentation, once per entry; a presentation without a trigger (nothing asked for it) measures nothing.
    func surfaceDidBecomeReady() {
        guard let presentation = presentation, presentation.entryID != readyReportedEntryID else { return }
        readyReportedEntryID = presentation.entryID
        guard let triggeredAt = presentation.triggeredAtUptimeNanoseconds else { return }
        let launch: CoreMetricsLaunchKind
        if !presentation.access.protectedDataAvailable {
            launch = .lockedHandoff
        } else {
            launch = presentation.launchKind == .cold ? .cold : .warm
        }
        metrics?.recordTriggerToReady(launch, elapsed: Self.seconds(from: triggeredAt, to: uptimeNanoseconds()))
    }

    func switchMode(to newMode: CaptureEntryMode) {
        guard isIdle, newMode != mode else { return }
        mode = newMode
        voiceNotice = nil
    }

    /// Re-reads what the device allows. Losing read access removes everything that was learned from stored data.
    func refreshAccess() {
        access = currentAccess()
        if !access.canReadStoredStatus {
            progressGeneration += 1
            if var current = acknowledgment, current.progress != nil {
                current.progress = nil
                acknowledgment = current
            }
        }
    }

    // MARK: Silent capture

    func saveText() {
        textModel.save()
    }

    private func textSaveStarted() {
        textInputEndedAt = uptimeNanoseconds()
        voiceNotice = nil
        setAcknowledgment(CaptureAcknowledgment(source: .text, save: .saving, progress: nil, earlyEnd: nil))
    }

    private func textSaveFinished(_ outcome: TextCaptureOutcome) {
        let save: CaptureSaveState
        switch outcome {
        case .saved(_, let itemID, let alreadySaved): save = .saved(itemID: itemID, alreadySaved: alreadySaved)
        case .keptOnDevice: save = .keptOnDevice
        case .notStaged: save = .notSaved
        case .itemDeleted: save = .itemDeleted
        }
        setAcknowledgment(CaptureAcknowledgment(source: .text, save: save, progress: nil, earlyEnd: nil))
        recordEndOfInputToSave(.text, save: save)
        finishIfDurable(save)
    }

    private func textChanged(to newText: String) {
        guard let current = acknowledgment, current.source == .text, !current.isSaving, !newText.isEmpty else { return }
        clearAcknowledgment()
    }

    // MARK: Voice capture

    /// Hands the pending "recording started" announcement to the view, once. The view calls it on appear and on change.
    func consumeRecordingStartAnnouncement() -> Bool {
        guard recordingStartAnnouncementPending else { return false }
        recordingStartAnnouncementPending = false
        return true
    }

    func startRecording() {
        guard recorderState == .idle, !textModel.isSaving else { return }
        voiceNotice = nil
        voice.start { [weak self] result in
            guard let self = self else { return }
            switch result {
            case .success:
                self.voiceInputEndedAt = nil
                self.clearAcknowledgment()
            case .failure(let failure):
                self.voiceNotice = Self.startFailureMessage(failure)
            }
        }
    }

    func stopRecording() {
        voiceInputEndedAt = voiceInputEndedAt ?? uptimeNanoseconds()
        voice.stop()
    }

    func cancelRecording() {
        voiceInputEndedAt = nil
        voice.cancel()
    }

    private func recorderStateChanged(to state: VoiceRecorderState) {
        if case .finishing = state { voiceInputEndedAt = voiceInputEndedAt ?? uptimeNanoseconds() }
        if case .recording = state, !isRecording { recordingStartAnnouncementPending = true }
        if case .idle = state { recordingStartAnnouncementPending = false }
        recorderState = state
    }

    private func voiceRecordingEnded(_ outcome: VoiceCaptureOutcome) {
        let early: (VoiceRecordingEnd) -> VoiceRecordingEnd? = { $0 == .stoppedByUser ? nil : $0 }
        let acknowledgment: CaptureAcknowledgment
        switch outcome {
        case .saved(let summary, let saved):
            acknowledgment = CaptureAcknowledgment(
                source: .audio, save: .saved(itemID: saved.itemID, alreadySaved: saved.alreadyImported),
                progress: nil, earlyEnd: early(summary.end))
        case .keptForRetry(let summary, let reason):
            acknowledgment = CaptureAcknowledgment(
                source: .audio, save: reason == .importUnconfirmed ? .keptOnDevice : .notSaved,
                progress: nil, earlyEnd: early(summary.end))
        case .retainedUnsubmitted:
            acknowledgment = CaptureAcknowledgment(source: .audio, save: .cancelled, progress: nil, earlyEnd: nil)
        case .itemDeleted(let summary):
            acknowledgment = CaptureAcknowledgment(
                source: .audio, save: .itemDeleted, progress: nil, earlyEnd: early(summary.end))
        case .continuationNotJoined(_, let end, _, let segmentRetained):
            acknowledgment = CaptureAcknowledgment(
                source: .audio, save: .unreadable(retained: segmentRetained), progress: nil, earlyEnd: early(end))
        case .unrecoverable(_, let end, let fileRetained):
            acknowledgment = CaptureAcknowledgment(
                source: .audio, save: .unreadable(retained: fileRetained), progress: nil, earlyEnd: early(end))
        }
        setAcknowledgment(acknowledgment)
        recordEndOfInputToSave(.voice, save: acknowledgment.save)
        finishIfDurable(acknowledgment.save)
    }

    // MARK: Progress

    /// Reads the saved item's status, if and only if the device currently allows reading stored data.
    func refreshProgress() {
        refreshAccess()
        guard let reader = progressReader, access.canReadStoredStatus,
              let itemID = acknowledgment?.savedItemID else { return }
        progressGeneration += 1
        let generation = progressGeneration
        reader.readStatus(itemID: itemID) { [weak self] result in
            guard let self = self, generation == self.progressGeneration,
                  self.currentAccess().canReadStoredStatus,
                  var current = self.acknowledgment, current.savedItemID == itemID else { return }
            if case .status(let status) = result {
                current.progress = status.progress
                self.acknowledgment = current
            }
        }
    }

    // MARK: Helpers

    /// Ends the end-of-input span of `capture`. Only a durable acknowledgment is a sample; a failed or cancelled end
    /// is not a save and is dropped.
    private func recordEndOfInputToSave(_ capture: CoreMetricsCaptureKind, save: CaptureSaveState) {
        let endedAt: UInt64?
        switch capture {
        case .text:
            endedAt = textInputEndedAt
            textInputEndedAt = nil
        case .voice:
            endedAt = voiceInputEndedAt
            voiceInputEndedAt = nil
        }
        guard let endedAt = endedAt else { return }
        switch save {
        case .saved, .keptOnDevice:
            metrics?.recordEndOfInputToSave(capture, elapsed: Self.seconds(from: endedAt, to: uptimeNanoseconds()))
        case .saving, .notSaved, .itemDeleted, .cancelled, .unreadable:
            break
        }
    }

    private static func seconds(from start: UInt64, to end: UInt64) -> TimeInterval {
        end >= start ? TimeInterval(end - start) / 1_000_000_000 : 0
    }

    private func finishIfDurable(_ save: CaptureSaveState) {
        switch save {
        case .saved:
            captureDidEnd?()
            refreshProgress()
        case .keptOnDevice:
            captureDidEnd?()
        case .saving, .notSaved, .itemDeleted, .cancelled, .unreadable:
            break
        }
    }

    private func setAcknowledgment(_ newAcknowledgment: CaptureAcknowledgment) {
        progressGeneration += 1
        acknowledgment = newAcknowledgment
    }

    private func clearAcknowledgment() {
        progressGeneration += 1
        acknowledgment = nil
    }

    static func startFailureMessage(_ failure: VoiceStartFailure) -> String {
        switch failure {
        case .alreadyActive:
            return "A recording is already in progress."
        case .notInForeground:
            return "Recording needs Oh And on screen. Nothing was recorded."
        case .microphoneDenied:
            return "Microphone access is off. Nothing was recorded. You can type instead."
        case .storageUnavailable:
            return "Recordings cannot be stored right now. Nothing was recorded. You can type instead."
        case .storageSpaceUnknown, .insufficientStorage:
            return "There is not enough free storage to record. Nothing was recorded."
        case .recorderUnavailable:
            return "The recorder could not start. Nothing was recorded. You can type instead."
        case .recordingNotContinuable, .nothingLeftToRecord, .continuationLeftoverPresent:
            return "That recording cannot be continued. Nothing was recorded."
        }
    }
}
