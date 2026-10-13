import Foundation

/// Where a core reset generation stands. Only the core decides this: it is derived from durable rows.
enum ResetPhase: String, Equatable, Sendable {
    case pending
    case completed
}

/// A reset generation and what still keeps the core from completing it.
struct ResetProgress: Equatable, Sendable {
    let generation: Int64
    let phase: ResetPhase
    let boundaryRowID: Int64
    let itemsAwaitingTombstone: Int
    let cleanupPending: Int
    let cleanupFailed: Int
}

/// A stored provider profile version and the Keychain reference its credential lives under.
struct ResetProfileCredential: Equatable, Sendable {
    let profileVersion: String
    let credentialReference: String?
    let isRevoked: Bool
}

/// The user's explicit answer about provider profiles and their stored credentials. Reset never removes them unless the
/// user chose `remove`.
enum ResetCredentialChoice: Equatable, Sendable {
    case keep
    case remove
}

/// The durable reset operation as native code uses it (`core/src/lifecycle/reset`). The core owns the generation, the
/// boundary and the completion rule; native code never keeps a generation of its own and never decides that a reset
/// succeeded.
protocol ResetCore: Sendable {
    /// Records a new generation (or resumes the pending one) and tombstones every pre-reset item.
    func start() async throws -> ResetProgress
    /// Re-evaluates the pending generation after cleanup acknowledgments. `nil` when no reset has ever started.
    func advance() async throws -> ResetProgress?
    /// The latest generation without changing anything. `nil` when no reset has ever started.
    func status() async throws -> ResetProgress?
    func profileCredentials() async throws -> [ResetProfileCredential]
    func revokeProfile(profileVersion: String) async throws
}

/// The per-item cleanup executors (notifications, ingress, audio). `DeletionCleanupService` is the production
/// implementation; it records a completion with the core only after verifying the effect.
protocol ResetItemCleanup {
    func reconcile() async -> DeletionReconcileReport
}

extension DeletionCleanupService: ResetItemCleanup {}

/// What native ingress staging looked like at the moment of the reset tap.
struct ResetStagingSnapshot: Equatable {
    let captureIDs: Set<String>
}

/// The outcome of purging staged inputs that were written before the tap and are still unimported.
enum ResetStagingPurge: Equatable {
    /// Every such input and the audio it owns is gone.
    case purged(count: Int)
    /// These inputs could not be removed now. They stay and the reset does not start.
    case incomplete(remaining: Int)
    /// The staging area could not be listed, so what remains is unknown.
    case unknown
}

/// Native ingress staging as reset needs it. A capture staged before the tap has no item yet, so no tombstone can fence
/// it: it is imported first (and then tombstoned by the core) or purged.
protocol ResetIngressStaging {
    func snapshot() throws -> ResetStagingSnapshot
    /// Imports every staged record through the production ingress path. Returns false when the staging area could not
    /// be listed.
    func importStaged() async -> Bool
    /// Removes the records and owned audio of `snapshot` that are still staged. Inputs staged after the snapshot are
    /// never touched.
    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge
}

/// Export and share temporaries inside the protected temporary store.
protocol ResetExportTemporaries {
    /// Removes every export directory and returns true only when none remains.
    func removeAll() -> Bool
}

protocol ResetCredentialRemoving {
    /// Removes the secret behind a reference. Removing an absent credential succeeds.
    func deleteCredential(reference: String) throws
}

extension CredentialService: ResetCredentialRemoving {}

/// What native code still owes a reset, as two small durable markers, so a restart finishes what the user asked for. They
/// hold a choice and a flag, never a generation or a result: the core alone says whether the reset itself is complete.
protocol ResetMarkerStore {
    /// The user chose to remove provider profiles and their credentials.
    func isRemovalRequested() throws -> Bool
    func recordRemovalRequested() throws
    func clearRemovalRequest() throws

    /// A reset was requested and its native steps (cleanup, temporaries, credentials) have not all been confirmed.
    func isCleanupOutstanding() throws -> Bool
    func recordCleanupOutstanding() throws
    func clearCleanupOutstanding() throws
}

/// Why the reset is not finished. Fixed codes only: never identifiers, paths, content or error text.
enum ResetBlocker: String, Equatable, Sendable {
    case coreUnavailable = "core_unavailable"
    case markerNotRecorded = "reset_marker_not_recorded"
    case markerNotCleared = "reset_marker_not_cleared"
    case stagingUnreadable = "staging_unreadable"
    case stagingPurgeIncomplete = "staging_purge_incomplete"
    case itemCleanupIncomplete = "item_cleanup_incomplete"
    case exportTemporariesRemain = "export_temporaries_remain"
    case credentialRemovalIncomplete = "credential_removal_incomplete"
    case coreResetPending = "core_reset_pending"
    case cancelled = "cancelled"
}

enum ResetState: Equatable, Sendable {
    /// No reset is running or pending.
    case notStarted
    case pending
    /// The core completed the generation and every native step converged.
    case completed
}

struct ResetReport: Equatable {
    let state: ResetState
    /// The core's latest progress, when the core was reached.
    let progress: ResetProgress?
    let blockers: [ResetBlocker]
    /// The per-item cleanup pass of this call, when it ran.
    let itemCleanup: DeletionReconcileReport?

    /// True only when the core reports the generation completed and nothing native is outstanding.
    var isComplete: Bool { state == .completed }
}
