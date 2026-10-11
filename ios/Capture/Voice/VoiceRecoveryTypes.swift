import Foundation

/// Why a staged capture is not confirmed saved. The capture itself is durable on the device in every case.
enum VoicePendingReason: Equatable {
    case importUnconfirmed
    case coreRejected(code: String)
    case audioUnavailable
    case stagingRecordUnreadable
}

struct VoicePendingCapture: Equatable {
    var captureID: String
    var reason: VoicePendingReason
}

/// What one pass over the foreground ingress staging area found, in the recorder's terms.
struct VoiceIngressRecoveryReport: Equatable {
    /// Staged captures whose import is still unconfirmed. They are retained and retried, never reported as saved.
    var pending: [VoicePendingCapture] = []
    /// Staged captures the core confirmed during this pass.
    var confirmedCaptureIDs: [String] = []
    /// Staged captures whose item the user had deleted; nothing was re-imported.
    var deletedItemCaptureIDs: [String] = []
    /// Files in the in-progress audio store that no staged capture owns. nil means "could not be determined" (the
    /// staging area or the store could not be listed or read) and must never be read as "none".
    var unclaimedInProgressFileNames: [String]?
}

/// The seam to the foreground ingress writer's restart recovery. The implementation calls `completion` exactly once,
/// on the main queue, and reports `unclaimedInProgressFileNames` as nil whenever ownership of the in-progress files is
/// not fully known.
protocol VoiceRecoveryIngress: AnyObject {
    func recoverStagedCaptures(completion: @escaping (VoiceIngressRecoveryReport) -> Void)
}

/// A recording left in the in-progress store that was never handed to ingress: a user cancellation, or a recording
/// that was cut off before its handoff. Offered for the user to finish, continue or delete; nothing queues or expires.
struct RecoverableVoiceRecording: Equatable {
    var captureID: String
    var fileName: String
    var fileSizeBytes: Int64
    /// nil when bytes exist but cannot be read back as audio.
    var durationSeconds: Double?
    var modifiedAt: Date?
    /// True when audio can still be added within the duration and size bounds and no earlier added audio is waiting.
    var canContinue: Bool
    /// True when this file is audio added to another recording that may never have been joined onto it; it is its own
    /// recording until the user finishes or deletes it. Stays true after that other recording is finished or deleted.
    var isUnjoinedAddedAudio = false
    /// True when audio added to this recording earlier is still kept as a separate file, so more cannot be added yet.
    var hasUnjoinedAddedAudio = false

    var canFinish: Bool { durationSeconds != nil }
}

enum VoiceRecoveryAction: Hashable {
    case finish
    case continueRecording
    case delete
}

/// What re-entry found. `recordings` is the bounded list shown; the other fields keep unknowns and overflow visible.
struct VoiceRecoveryListing: Equatable {
    var recordings: [RecoverableVoiceRecording] = []
    /// Recoverable recordings beyond the bound; they stay on the device and appear as the shown ones are handled.
    var notShownCount = 0
    /// True when which recordings exist could not be determined. An empty `recordings` then means "unknown".
    var recordingsUnknown = false
    var pendingCaptures: [VoicePendingCapture] = []
    var confirmedCaptureIDs: [String] = []
}

enum VoiceRecoveryFailure: Error, Equatable {
    case recordingNotFound
    case alreadyInProgress
    case recordingActive
    case ingressStateUnknown
    case ownedByStagedCapture
    case notReadable
    case removalFailed
    case recorderRefused(VoiceStartFailure)
}

/// What an explicit delete removed, for an honest confirmation message.
struct VoiceDeletedRecording: Equatable {
    var captureID: String
    var durationSeconds: Double?
    var fileSizeBytes: Int64
}

struct VoiceRecoveryRow: Equatable {
    var fileName: String
    var title: String
    var detail: String
    var actions: [VoiceRecoveryAction]
}
