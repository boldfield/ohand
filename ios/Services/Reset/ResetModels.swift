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

/// A provider profile as a `remove` choice binds it: the version to revoke and the reference its credential lives under.
struct ResetBoundProfile: Hashable, Sendable {
    let profileVersion: String
    let credentialReference: String?
}

/// Which secret is stored behind a credential reference at one moment. A reference is stable and mutable (an update
/// overwrites the secret behind the same reference, a missing item is re-bound to a new one), so the reference alone
/// cannot say whether the secret a `remove` choice applied to is still the one stored. The identity is Keychain metadata
/// only: the instants the Keychain itself records for the item's creation and its last modification, which every write
/// moves. It is never secret bytes nor anything derived from them, so a marker may hold it.
struct ResetSecretIdentity: Hashable, Sendable {
    /// `Date.timeIntervalSinceReferenceDate` as its exact bit pattern, so a stored identity compares exactly.
    let createdAtBits: UInt64
    let modifiedAtBits: UInt64

    init(createdAtBits: UInt64, modifiedAtBits: UInt64) {
        self.createdAtBits = createdAtBits
        self.modifiedAtBits = modifiedAtBits
    }

    init(createdAt: Date, modifiedAt: Date) {
        self.init(
            createdAtBits: createdAt.timeIntervalSinceReferenceDate.bitPattern,
            modifiedAtBits: modifiedAt.timeIntervalSinceReferenceDate.bitPattern)
    }

    var createdAt: Date { Date(timeIntervalSinceReferenceDate: Double(bitPattern: createdAtBits)) }
    var modifiedAt: Date { Date(timeIntervalSinceReferenceDate: Double(bitPattern: modifiedAtBits)) }
}

/// A secret as a `remove` choice binds it: the reference and the identity of the secret stored behind it at the tap.
struct ResetBoundSecret: Hashable, Sendable {
    let credentialReference: String
    let identity: ResetSecretIdentity
}

/// What a `remove` choice applies to: exactly the profiles present at the tap that made it, revoked ones included, and
/// exactly the secrets stored at the tap behind their references. The reset revokes these profiles and deletes these
/// secrets, on the first attempt or on a retry after a failure or a crash, and never touches a profile or a secret
/// configured after the tap: that is fresh-generation state, like a capture made after the tap. A secret updated after
/// the tap behind a bound reference is one of those, and so is a reference that had no secret at the tap and gained one
/// since.
struct ResetCredentialRemoval: Equatable, Sendable {
    let profiles: [ResetBoundProfile]
    let secrets: [ResetBoundSecret]

    init(profiles: [ResetBoundProfile], secrets: [ResetBoundSecret] = []) {
        self.profiles = profiles
        self.secrets = secrets
    }

    /// Binds the profiles the core lists at the tap and, through `identityOf`, the secret stored at the tap behind each
    /// of their references (none when `identityOf` answers nil: nothing is stored there to remove).
    init(binding profiles: [ResetProfileCredential], identityOf: (String) throws -> ResetSecretIdentity?) rethrows {
        self.profiles = profiles.map {
            ResetBoundProfile(profileVersion: $0.profileVersion, credentialReference: $0.credentialReference)
        }
        var secrets: [ResetBoundSecret] = []
        for reference in Set(profiles.compactMap(\.credentialReference)).sorted() {
            if let identity = try identityOf(reference) {
                secrets.append(ResetBoundSecret(credentialReference: reference, identity: identity))
            }
        }
        self.secrets = secrets
    }

    var profileVersions: Set<String> { Set(profiles.map(\.profileVersion)) }
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

/// The user's reset tap, recorded durably from the tap until the core confirms that it recorded the reset: the explicit
/// credential choice, the profiles a `remove` choice applies to and the staged inputs that existed at the tap. Both lists
/// are the durable form of the tap's moment, so a restart treats exactly what the tap would have as pre-reset input and
/// every later capture, profile or secret as fresh-generation content.
///
/// A tap made while an earlier reset is unfinished (its generation still pending, or native cleanup such as its
/// credential choice still outstanding) `awaitsEarlierReset`: resuming that generation only erases what existed at the
/// earlier tap, and its choice must be carried out before this tap's replaces it, so this tap first lets it finish and
/// then opens a generation of its own for its list. The flag is what tells a restart that a pending generation is the
/// earlier one rather than one this tap already started; it is cleared, by recording the tap again without it, once the
/// earlier reset completed.
///
/// The tap also notes the core's latest generation as it was at the tap. That is a reference to the core's own durable
/// state, not a generation native code keeps: comparing it with what the core reports later says whether the core has
/// since started a generation for this tap (`coreHasStarted`). The answer decides what a later tap may do with a tap
/// that is still recorded: one the core never started is replaced by the latest explicit choice, one the core started
/// has its choice carried out first, exactly as if it had been carried over before the interruption.
struct ResetRequest: Equatable {
    let credentialChoice: ResetCredentialChoice
    /// For `remove`, the profiles present at the tap (possibly none); always nil for `keep`.
    let credentialRemoval: ResetCredentialRemoval?
    let stagedBeforeTheTap: ResetStagingSnapshot
    let awaitsEarlierReset: Bool
    /// The core's latest generation at the tap; nil when no reset had ever started.
    let latestGenerationAtTheTap: Int64?

    init(
        credentialChoice: ResetCredentialChoice,
        credentialRemoval: ResetCredentialRemoval? = nil,
        stagedBeforeTheTap: ResetStagingSnapshot,
        awaitsEarlierReset: Bool = false,
        latestGenerationAtTheTap: Int64? = nil
    ) {
        self.credentialChoice = credentialChoice
        self.credentialRemoval =
            credentialChoice == .remove ? credentialRemoval ?? ResetCredentialRemoval(profiles: []) : nil
        self.stagedBeforeTheTap = stagedBeforeTheTap
        self.awaitsEarlierReset = awaitsEarlierReset
        self.latestGenerationAtTheTap = latestGenerationAtTheTap
    }

    /// Whether the core has started a generation since this tap, given its latest progress: a generation newer than
    /// the one at the tap is one this tap started (while the tap awaits an earlier reset, that reset's generation is the
    /// one at the tap, so it never counts). A core that reports no generation has started nothing.
    func coreHasStarted(latest: ResetProgress?) -> Bool {
        guard let latest else { return false }
        guard let atTheTap = latestGenerationAtTheTap else { return true }
        return latest.generation > atTheTap
    }
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
/// it: it is imported first (and then tombstoned by the core) or purged. A capture made after the tap is fresh-generation
/// content, so while the tap is recorded and not yet consumed it must neither be listed, imported nor purged.
protocol ResetIngressStaging {
    /// The tap. Lists the staged inputs and, in the same step, holds the ingress boundary: an import of any input not in
    /// the list waits (its input stays staged) until `endBoundary`. When this throws, this call holds nothing new.
    func snapshot() async throws -> ResetStagingSnapshot
    /// Holds the boundary of a tap recorded by an earlier process, with the list taken at that tap. Inputs staged since
    /// the tap are not in it, so they wait exactly as they would have in the process that recorded the tap.
    func holdBoundary(for snapshot: ResetStagingSnapshot) async
    /// Imports the staged records named by `snapshot` through the production ingress path. A record staged after the
    /// snapshot is never imported here, so it cannot land below the reset boundary. A record that cannot be imported
    /// stays staged and is left to `purgeUnimported`.
    func importSnapshot(_ snapshot: ResetStagingSnapshot) async
    /// Removes the records and owned audio of `snapshot` that are still staged. Inputs staged after the snapshot are
    /// never touched.
    func purgeUnimported(from snapshot: ResetStagingSnapshot) async -> ResetStagingPurge
    /// Releases the boundary, so inputs that waited import into whatever generation is then current. Called only once no
    /// tap is recorded any more. Idempotent, and safe to call when nothing is held.
    func endBoundary()
}

/// Export and share temporaries inside the protected temporary store.
protocol ResetExportTemporaries {
    /// Removes every export directory and returns true only when none remains.
    func removeAll() -> Bool
}

/// The Keychain as reset needs it: which secret is behind a reference now, and removal of exactly that secret.
/// `KeychainResetCredentialRemover` is the production implementation.
protocol ResetCredentialRemoving {
    /// The identity of the secret stored behind `reference` now; nil when none is stored. Throws when the Keychain
    /// cannot answer (a storage failure, or the device is locked), never for an absent item.
    func secretIdentity(reference: String) throws -> ResetSecretIdentity?
    /// Removes the secret behind `reference` only while it is still the one with `identity`. Succeeds, removing nothing,
    /// when no secret is stored there or a different one is: nothing of the bound secret remains either way.
    func deleteCredential(reference: String, identity: ResetSecretIdentity) throws
}

/// What native code still owes a reset, as small durable markers, so a restart finishes what the user asked for. They
/// hold a choice, what it binds, flags and, with a tap, a reference to the core's generation at that tap; never a
/// generation of their own or a result: the core alone says whether the reset itself is complete.
protocol ResetMarkerStore {
    /// The latest recorded tap the core has not confirmed. A restart that finds it holds the ingress boundary at its list
    /// and runs the whole reset again (finishing an earlier pending generation first when the tap awaits it, then
    /// staging, core start, cleanup) with its choice. It is cleared once the core has
    /// started and the two markers below carry the choice, or withdrawn when the user is told the reset did not start.
    func request() throws -> ResetRequest?
    /// Records a tap. It replaces any earlier one, and an interruption or failure at any point leaves either the earlier
    /// request or this one as the latest, never a mix of the two and never no request where one was recorded before.
    func recordRequest(_ request: ResetRequest) throws
    /// Clears every recorded tap. When this throws, the latest request that remains is still a tap the user made.
    func clearRequest() throws

    /// The user chose to remove provider profiles and their credentials, and what that choice binds: nil when no removal
    /// is owed. Set only after the core started the reset that carries the choice, and cleared once every bound profile
    /// is revoked and every bound secret is gone, so a retry acts on exactly the profiles and secrets the choice applied
    /// to.
    func removalRequest() throws -> ResetCredentialRemoval?
    func recordRemovalRequested(_ removal: ResetCredentialRemoval) throws
    func clearRemovalRequest() throws

    /// The core started a reset and its native steps (cleanup, temporaries, credentials) have not all been confirmed.
    /// Set only after the core started, so it never describes a reset that did not begin.
    func isCleanupOutstanding() throws -> Bool
    func recordCleanupOutstanding() throws
    func clearCleanupOutstanding() throws
}

extension ResetMarkerStore {
    /// Whether a removal is owed, regardless of what it binds.
    func isRemovalRequested() throws -> Bool { try removalRequest() != nil }
}

/// Why the reset is not finished. Fixed codes only: never identifiers, paths, content or error text.
enum ResetBlocker: String, Equatable, Sendable {
    case coreUnavailable = "core_unavailable"
    case markerNotRecorded = "reset_marker_not_recorded"
    case markerUnreadable = "reset_marker_unreadable"
    case markerNotCleared = "reset_marker_not_cleared"
    case stagingUnreadable = "staging_unreadable"
    case stagingPurgeIncomplete = "staging_purge_incomplete"
    case itemCleanupIncomplete = "item_cleanup_incomplete"
    case exportTemporariesRemain = "export_temporaries_remain"
    case credentialRemovalIncomplete = "credential_removal_incomplete"
    /// A `remove` tap could not learn which secrets are stored behind the profiles it binds, so it started nothing.
    case credentialsUnreadable = "credentials_unreadable"
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
