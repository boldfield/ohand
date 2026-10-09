import Foundation

/// Bounds on one explicit recording. The defaults match the 16 kHz mono 16-bit format the engine writes (about 32 kB
/// per second), so the size bound stays a backstop behind the duration bound.
struct VoiceRecordingLimits: Equatable {
    var maxDurationSeconds: TimeInterval = 300
    var maxFileBytes: Int64 = 12_000_000
    var minimumFreeBytesToStart: Int64 = 50_000_000
    var minimumFreeBytesWhileRecording: Int64 = 10_000_000
    var pollIntervalSeconds: TimeInterval = 1
    /// A continuation shorter than this is not offered: there is too little room left to be worth starting.
    var minimumContinuationSeconds: TimeInterval = 1
    /// The recorder's 16 kHz mono 16-bit byte rate, used to check the size bound before a continuation starts.
    var bytesPerSecond: Int64 = 32_000
}

/// Why a recording stopped. Only `stoppedByUser` with a clean close is a complete recording.
enum VoiceRecordingEnd: Equatable {
    case stoppedByUser
    case cancelledByUser
    case durationLimit
    case sizeLimit
    case lowStorage
    case audioInterrupted
    case leftForeground
    case deviceLocking
    case recorderFailed
    /// Not an end the recorder saw: a recording found in the in-progress store after a restart, finished by the user.
    /// How it originally ended is unknown, so it is never presented as complete.
    case recoveredOnReentry

    var reachedLimit: Bool {
        self == .durationLimit || self == .sizeLimit || self == .lowStorage
    }
}

/// What a closed recording is, as verified by reading the file back.
struct VoiceRecordingSummary: Equatable {
    var captureID: String
    var inProgressFileName: String
    var startedAt: Date
    var durationSeconds: Double
    var fileSizeBytes: Int
    var end: VoiceRecordingEnd
    /// False when the engine did not confirm a successful finish although a readable prefix exists.
    var closedCleanly: Bool
    /// False when the in-progress store's protection class could not be applied to the recording file.
    var protectionApplied: Bool

    var isComplete: Bool { end == .stoppedByUser && closedCleanly }
}

enum VoiceStartFailure: Error, Equatable {
    case alreadyActive
    case notInForeground
    case microphoneDenied
    case storageUnavailable
    case storageSpaceUnknown
    case insufficientStorage
    case recorderUnavailable
    /// The recording to continue is missing, unreadable or not a recording this recorder wrote.
    case recordingNotContinuable
    /// The recording already uses the duration or size bound, so nothing more can be added.
    case nothingLeftToRecord
}

struct VoiceRecordingStarted: Equatable {
    var captureID: String
    var startedAt: Date
}

enum VoiceRecorderState: Equatable {
    case idle
    case requestingPermission
    case recording(captureID: String, startedAt: Date)
    case finishing(captureID: String)
}

/// A closed recording offered to the foreground ingress writer. The finalized name is bound to the capture ID.
struct VoiceRecordingHandoff: Equatable {
    var captureID: String
    var inProgressFileName: String
    var finalizedFileName: String
    var startedAt: Date
    var durationSeconds: Double
    var fileSizeBytes: Int
    var end: VoiceRecordingEnd
}

enum VoiceHandoffResult: Equatable {
    /// The core confirmed the import. The only result that may be shown as saved.
    case saved(itemID: String, savedAt: String, alreadyImported: Bool)
    /// The audio is durable in the ingress staging area but the import is not confirmed.
    case keptForRetry
    /// Nothing was staged; the audio remains in the in-progress store.
    case notStaged
    /// The user deleted the item for this capture.
    case itemDeleted
}

/// The seam to the foreground ingress writer. The implementation calls `completion` exactly once, on the main queue.
protocol VoiceRecordingHandoffReceiving: AnyObject {
    func handOff(_ handoff: VoiceRecordingHandoff, completion: @escaping (VoiceHandoffResult) -> Void)
}

struct VoiceSaveAcknowledgment: Equatable {
    var itemID: String
    var savedAt: String
    var alreadyImported: Bool
}

enum VoiceKeptReason: Equatable {
    case importUnconfirmed
    case notStaged
}

/// The honest result of one recording. `saved` is the only outcome that means the core holds the capture.
enum VoiceCaptureOutcome: Equatable {
    case saved(VoiceRecordingSummary, VoiceSaveAcknowledgment)
    /// The audio is kept for a later attempt; it is not saved yet.
    case keptForRetry(VoiceRecordingSummary, VoiceKeptReason)
    /// The user cancelled. The audio stays in the in-progress store, unsubmitted, until the user decides.
    case retainedUnsubmitted(VoiceRecordingSummary)
    case itemDeleted(VoiceRecordingSummary)
    /// The audio added to an existing recording was not joined to it. The existing recording is exactly as it was.
    /// `segmentRetained` says whether the new audio was kept as its own recoverable recording.
    case continuationNotJoined(captureID: String, end: VoiceRecordingEnd, reason: VoiceContinuationFailure, segmentRetained: Bool)
    /// No readable audio exists. `fileRetained` says whether unreadable bytes were left on disk for recovery.
    case unrecoverable(captureID: String, end: VoiceRecordingEnd, fileRetained: Bool)
}

enum VoiceContinuationFailure: Equatable {
    /// The added audio could not be read back.
    case noReadableAudio
    /// The joined recording could not be written or did not verify.
    case joinFailed
}
