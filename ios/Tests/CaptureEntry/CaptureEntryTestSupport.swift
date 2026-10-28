import Foundation
@testable import OhAnd
@testable import OhAndCoreBridge

/// A recorder the test drives: starting moves it to recording, and `finish(with:)` ends it with a chosen outcome.
final class FakeVoiceController: VoiceRecordingControlling {
    var state: VoiceRecorderState = .idle {
        didSet { if state != oldValue { onStateChange?(state) } }
    }
    var onStateChange: ((VoiceRecorderState) -> Void)?
    var onOutcome: ((VoiceCaptureOutcome) -> Void)?
    var startResult: Result<VoiceRecordingStarted, VoiceStartFailure> = .success(
        VoiceRecordingStarted(captureID: "voice-1", startedAt: Date(timeIntervalSince1970: 1_790_000_000)))
    var microphonePermission: MicrophonePermissionStatus = .granted
    private(set) var startCount = 0
    private(set) var stopCount = 0
    private(set) var cancelCount = 0

    func start(completion: @escaping (Result<VoiceRecordingStarted, VoiceStartFailure>) -> Void) {
        startCount += 1
        if case .success(let started) = startResult {
            state = .recording(captureID: started.captureID, startedAt: started.startedAt)
        }
        completion(startResult)
    }

    func stop() { stopCount += 1 }

    func cancel() { cancelCount += 1 }

    func finish(with outcome: VoiceCaptureOutcome) {
        state = .idle
        onOutcome?(outcome)
    }

    static func summary(captureID: String = "voice-1", end: VoiceRecordingEnd = .stoppedByUser) -> VoiceRecordingSummary {
        VoiceRecordingSummary(
            captureID: captureID, inProgressFileName: "\(captureID).wav",
            startedAt: Date(timeIntervalSince1970: 1_790_000_000), durationSeconds: 4, fileSizeBytes: 128_000,
            end: end, closedCleanly: end == .stoppedByUser, protectionApplied: true)
    }

    static func saved(itemID: String = "item-1", end: VoiceRecordingEnd = .stoppedByUser) -> VoiceCaptureOutcome {
        .saved(summary(end: end), VoiceSaveAcknowledgment(
            itemID: itemID, savedAt: "2026-10-08T09:30:02Z", alreadyImported: false))
    }
}

/// Answers status reads from a script, or holds the answers until `release()` so a test can change the device state
/// while a read is in flight.
final class ScriptedProgressReader: CaptureProgressReading {
    private(set) var requestedItemIDs: [String] = []
    var result: CaptureProgressReadResult = .unavailable
    var holdsCompletions = false
    private var held: [() -> Void] = []

    func readStatus(itemID: String, completion: @escaping (CaptureProgressReadResult) -> Void) {
        requestedItemIDs.append(itemID)
        let answer = result
        if holdsCompletions {
            held.append { completion(answer) }
        } else {
            completion(answer)
        }
    }

    func release() {
        let pending = held
        held = []
        pending.forEach { $0() }
    }
}

enum CaptureEntryFixtures {
    static let dueInstant = Date(timeIntervalSince1970: 1_790_000_000)

    static let wording = CaptureAcknowledgmentWording(
        locale: Locale(identifier: "en_US"), timeZone: TimeZone(identifier: "UTC")!)

    static let openAccess = CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true)

    static func installedStatus() -> CaptureItemStatus {
        CaptureItemStatus(
            processingState: "processed", transcriptionState: "not_applicable",
            reminderRequestState: "resolved", reminderScheduleState: "scheduled",
            unschedulableReason: nil, installedDueInstant: dueInstant)
    }

    static func presentation(
        mode: CaptureEntryMode = .silent,
        launchKind: CaptureLaunchKind = .cold,
        access: CaptureEntryAccess = openAccess
    ) -> CaptureEntryPresentation {
        CaptureEntryPresentation(
            entryID: "entry-1", source: .controlIntent, mode: mode, launchKind: launchKind, access: access)
    }
}

extension CaptureItemStatus {
    /// What a composition root does: copy the core's status report across, adding the due time of the installed
    /// notification request (the report itself carries none).
    init(report: ItemStatusReport, installedDueInstant: Date?) {
        self.init(
            processingState: report.processingState, transcriptionState: report.transcriptionState,
            reminderRequestState: report.reminderRequestState, reminderScheduleState: report.reminderScheduleState,
            unschedulableReason: report.unschedulableReason, installedDueInstant: installedDueInstant)
    }
}
