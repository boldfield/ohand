import Foundation
import OhAndCoreBridge

/// Why audio has no automatic expiry. Raw values are the core's `UnresolvedReason` codes.
public enum AudioUnresolvedReason: String, CaseIterable, Equatable, Sendable {
    case transcriptionPending = "transcription_pending"
    case transcriptionUnsupported = "transcription_unsupported"
    case transcriptionFailed = "transcription_failed"
    case textWithoutTranscript = "text_without_transcript"
    case transcriptRecordInconsistent = "transcript_record_inconsistent"
    case transcriptEmpty = "transcript_empty"
    case transcriptAudioMismatch = "transcript_audio_mismatch"
    case transcriptionTimeUnreadable = "transcription_time_unreadable"
}

/// Where one item's audio stands against its retention period. Timestamps are the core's RFC 3339 text.
public enum AudioRetentionStatus: Equatable, Sendable {
    case retained(expiresAt: String)
    case expired(expiredAt: String)
    case unresolved(AudioUnresolvedReason)
}

/// Actions the user is offered explicitly. Raw values are the core's.
public enum AudioRetentionOption: String, Equatable, Sendable {
    case retryTranscription = "retry_transcription"
    case deleteAudio = "delete_audio"
}

/// Why storage pressure did not free this audio. Raw values are the core's.
public enum AudioStorageNotice: String, Equatable, Sendable {
    case soleSourceRetained = "sole_source_retained"
    case notYetEligible = "not_yet_eligible"
}

/// What the core says native code may do with the audio.
public enum AudioCleanupIntent: Equatable, Sendable {
    case none
    case removeAudio(audioReference: String, expiredAt: String)
}

/// One live item's audio retention status as the core reported it.
public struct AudioRetentionItem: Equatable, Sendable {
    public let itemID: String
    public let audioReference: String
    public let status: AudioRetentionStatus
    public let isSoleSource: Bool
    public let options: [AudioRetentionOption]
    public let cleanup: AudioCleanupIntent
    public let storageNotice: AudioStorageNotice?

    public init(
        itemID: String,
        audioReference: String,
        status: AudioRetentionStatus,
        isSoleSource: Bool,
        options: [AudioRetentionOption],
        cleanup: AudioCleanupIntent,
        storageNotice: AudioStorageNotice?
    ) {
        self.itemID = itemID
        self.audioReference = audioReference
        self.status = status
        self.isSoleSource = isSoleSource
        self.options = options
        self.cleanup = cleanup
        self.storageNotice = storageNotice
    }
}

/// Why the core refuses a removal at the moment of removal.
public enum AudioRemovalDenial: Equatable, Sendable {
    case itemNotFound
    case audioReferenceMismatch
    case deletionOwnsCleanup
    case notYetExpired
    case unresolved(AudioUnresolvedReason)
    /// The user asked to remove audio that is the item's only source. Only deleting the item may remove it.
    case soleSource
}

public enum AudioRemovalDecision: Equatable, Sendable {
    case approved
    case denied(AudioRemovalDenial)
}

/// What happened to the file when native code carried out an approved removal.
public enum AudioRemovalEffect: Equatable, Sendable {
    /// The file existed, was removed and is verified absent.
    case removed
    /// The file was not there; it is verified absent.
    case alreadyAbsent
    /// The file may still exist.
    case failed
}

/// The outcome of `removeAudio`: refused with the effect never run, or approved with the effect that native code carried
/// out while the core held its fence.
public enum AudioRemovalResult: Equatable, Sendable {
    case denied(AudioRemovalDenial)
    case approved(AudioRemovalEffect)
}

/// The retention policy as native code uses it (`core/src/lifecycle/retention`). The core stays the only authority on
/// what may be removed: it decides eligibility from the successful-transcription time and the item's authoritative
/// source state. Native code never derives eligibility itself, and removes a retention file only inside `removeAudio`,
/// where the core decides and then holds a store-wide fence (the store's write lock) while native code carries out the
/// removal, so audio that became protected after a sweep, or between the approval and the removal, cannot be lost.
public protocol AudioRetentionCore: Sendable {
    /// Every live item that holds audio, with the core's retention state and cleanup intent.
    func sweep(pressure: CoreRetentionPressure, periodSeconds: UInt64) async throws -> [AudioRetentionItem]

    /// Read-only: whether the core would approve removing this audio now. It permits nothing.
    func authorizeRemoval(itemID: String, audioReference: String, periodSeconds: UInt64) async throws
        -> AudioRemovalDecision

    /// Re-decides the removal of the exact audio reference and, only when approved, runs `effect` while no other write
    /// to the store can commit, then ends the fence. `effect` removes the one file the reference names, verifies it
    /// absent and says what happened; it is never run when the removal is denied, and never run late in the fence's
    /// life. Throws, after the effect may have run, when the fence cannot be shown to have held throughout: the removal
    /// is then not reported as done and the next sweep decides it again.
    func removeAudio(
        itemID: String, audioReference: String, periodSeconds: UInt64, effect: () -> AudioRemovalEffect
    ) async throws -> AudioRemovalResult

    /// Like `removeAudio`, for audio the user asked to remove: the core ignores the retention clock and approves
    /// whenever the item keeps text without the audio. Audio that is the item's only source is denied as
    /// `soleSource`, and `effect` is never run. The same fence, pin and release apply.
    func removeAudioForUser(itemID: String, audioReference: String, effect: () -> AudioRemovalEffect) async throws
        -> AudioRemovalResult
}

/// Why a removal was left for the next sweep. Fixed codes only: a result never reports identifiers, paths or error text.
public enum AudioRetentionRetryReason: String, Equatable, Sendable {
    case coreUnavailable = "core_unavailable"
    case storageUnavailable = "storage_unavailable"
    /// The file is gone but its completion could not be recorded durably, so it is not reported as cleaned up yet.
    case ledgerUnavailable = "ledger_unavailable"
    case cancelled = "cancelled"
    /// The audio is open for playback; it is left in place and the removal is decided again once playback ends.
    case audioInPlayback = "audio_in_playback"
}

/// Why a removal will not be retried automatically. The audio stays in place and the failure stays visible on every
/// sweep until `resetFailure` re-arms it.
public enum AudioRetentionPermanentFailure: String, Equatable, Sendable {
    /// The core's audio reference does not name a file inside the finalized audio store.
    case audioReferenceUnresolvable = "audio_reference_unresolvable"
    /// The file could not be removed on the last allowed attempts.
    case attemptsExhausted = "attempts_exhausted"
}

public enum AudioRemovalOutcome: Equatable, Sendable {
    /// The file is verified absent and the completion is recorded durably.
    case removed
    /// The file was already absent and an earlier sweep had recorded the completion.
    case alreadyRemoved
    /// The core refused at removal time; nothing was removed.
    case denied(AudioRemovalDenial)
    /// Nothing is recorded as done; the next sweep repeats the idempotent effect.
    case retryLater(AudioRetentionRetryReason)
    case failedPermanently(AudioRetentionPermanentFailure)

    var isSettled: Bool {
        switch self {
        case .removed, .alreadyRemoved, .denied: return true
        case .retryLater, .failedPermanently: return false
        }
    }
}

public struct AudioRetentionItemReport: Equatable, Sendable {
    public let item: AudioRetentionItem
    /// Nil when the core offered no cleanup for the item, or the sweep's removal limit was reached before it.
    public let removal: AudioRemovalOutcome?

    public init(item: AudioRetentionItem, removal: AudioRemovalOutcome?) {
        self.item = item
        self.removal = removal
    }
}

public struct AudioRetentionSweepReport: Equatable, Sendable {
    /// Set when the core could not list the items. That is "unknown", never "nothing to do".
    public let listingFailure: AudioRetentionRetryReason?
    public let pressure: CoreRetentionPressure
    public let items: [AudioRetentionItemReport]
    /// Cleanups the core offered that this sweep left for the next one because it reached its removal limit.
    public let deferredRemovals: Int

    public init(
        listingFailure: AudioRetentionRetryReason?,
        pressure: CoreRetentionPressure,
        items: [AudioRetentionItemReport],
        deferredRemovals: Int = 0
    ) {
        self.listingFailure = listingFailure
        self.pressure = pressure
        self.items = items
        self.deferredRemovals = deferredRemovals
    }

    /// Removals that are not done and not refused: retried next sweep, or failed for good.
    public var unfinishedRemovals: [AudioRetentionItemReport] {
        items.filter { report in report.removal.map { !$0.isSettled } ?? false }
    }

    /// True only when the core was read, every offered cleanup was attempted, and each one is removed or refused.
    public var converged: Bool {
        listingFailure == nil && deferredRemovals == 0 && unfinishedRemovals.isEmpty
    }
}
