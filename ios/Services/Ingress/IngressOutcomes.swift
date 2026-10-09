import Foundation

/// A storage failure described only by Foundation error domain and code, never by message, so logging or displaying
/// it cannot disclose captured content or paths.
struct IngressStorageError: Error, Equatable {
    var domain: String
    var code: Int

    init(_ error: Error) {
        let nsError = error as NSError
        domain = nsError.domain
        code = nsError.code
    }

    init(domain: String, code: Int) {
        self.domain = domain
        self.code = code
    }
}

/// Why native code could not make an input durable or usable. In every case the original input is untouched.
enum IngressStagingFailure: Error, Equatable {
    case invalidRecord(IngressRecordProblem)
    /// A staging record with this capture ID holds different content. It is never overwritten.
    case conflictingStagingRecord
    case audioSourceMissing
    case audioSourceEmpty
    case storage(IngressStorageError)
    case unreadableRecord(IngressStorageError)
    case corruptRecord
    case unsupportedRecordVersion(Int)
}

/// Why a staged input is not saved yet. The staging record, and any audio it owns, remains.
enum IngressPendingProblem: Equatable {
    case notCommitted
    case commitUnknown
    case coreUnavailable
    case rejected(code: String)
    case conflictingReuse
    case sourceUnavailable(IngressStagingFailure)
    case recordUnreadable(IngressStagingFailure)
}

/// The Capture Ingestion acknowledgment as native code sees it. It exists only for a confirmed import.
struct IngressSaveAcknowledgment: Equatable {
    var captureID: String
    var itemID: String
    var savedAt: String
    var alreadyImported: Bool
    /// False when the import was confirmed but the staging record could not be removed. The item is saved; the
    /// leftover record is harmless because re-importing it returns the same item.
    var stagingCleanedUp: Bool
    /// Protection or backup attributes that could not be applied to a moved audio file. It keeps at least the class
    /// of the store it came from.
    var audioProtectionFailures: [StorageSetupFailure]
}

/// The result of one submission or recovery of an ingress record.
enum IngressOutcome: Equatable {
    /// The core confirmed the import. This is the only outcome that may be shown as "saved".
    case saved(IngressSaveAcknowledgment)
    /// The input is durable in the staging record but the import is not confirmed. Show it as kept, never as saved.
    case keptForRetry(captureID: String, problem: IngressPendingProblem)
    /// Nothing durable was written; the caller still holds the input and must keep it.
    case notStaged(captureID: String, failure: IngressStagingFailure)
    /// The user deleted the item for this capture; the staging record was discarded and nothing is re-imported.
    case itemDeleted(captureID: String)
}

struct IngressRecoveryEntry: Equatable {
    var captureID: String
    var outcome: IngressOutcome
}

/// What a recovery pass found. Entries cover every staging record; the other lists name files that need no action
/// but are surfaced so recoverable input is never invisible.
struct IngressRecoveryReport: Equatable {
    var entries: [IngressRecoveryEntry] = []
    /// Set when the staging directory itself could not be listed; recoverable input may exist but is not visible, so
    /// callers must treat this as "unknown", never as "nothing to recover".
    var listingFailure: IngressStagingFailure?
    /// Writes that were interrupted before becoming a record. They were never acknowledged and are left in place.
    var incompleteWrites: [String] = []
    /// Audio files in the in-progress store that no staging record owns: a recording that was interrupted before
    /// handoff. They are left in place for the recorder's own recovery.
    var unclaimedInProgressAudio: [String] = []
}
