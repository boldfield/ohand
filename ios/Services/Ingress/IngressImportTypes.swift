import Foundation

/// What the core reports once an import transaction has committed.
struct IngressImportConfirmation: Equatable {
    var captureID: String
    var itemID: String
    var savedAt: String
    /// True when an earlier attempt had already committed (a retry after an interruption or a repeated handoff).
    var alreadyImported: Bool
}

/// Why an import produced no confirmation. Each case says whether the staging record must be kept.
enum IngressImportFailure: Equatable {
    /// Nothing was written; retry later with the same capture ID.
    case notCommitted
    /// The commit outcome cannot be asserted. Keep the record and retry: the import is idempotent either way.
    case commitUnknown
    /// The core could not take the request (closed or cancelled before it ran). Keep the record.
    case coreUnavailable
    /// The record was refused with the core's specific code; it will not succeed until the record or configuration
    /// changes, and it is never discarded automatically.
    case rejected(code: String)
    /// The capture ID already holds different source content. Nothing was overwritten; the record is kept.
    case conflictingReuse
    /// The user deleted the item. This is the only terminal outcome: the staging record may be discarded.
    case itemDeleted
}

enum IngressImportResult: Equatable {
    case confirmed(IngressImportConfirmation)
    case failed(IngressImportFailure)
}

/// The native side of the import boundary. An implementation reports `confirmed` only after the core's transaction
/// committed, and calls `completion` exactly once.
protocol ForegroundIngressImporting: AnyObject {
    func importRecord(_ record: IngressRecord, completion: @escaping (IngressImportResult) -> Void)
}
