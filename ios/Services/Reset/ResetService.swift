import Foundation

/// Coordinates the native effects around the durable Rust reset. The core owns the generation, the boundary, the
/// per-item tombstones and the rule for completion; this service owns only what the core cannot reach: staged native
/// inputs, export temporaries, notifications, audio files and the optional credential removal.
///
/// A reset runs in this order:
/// 1. Bind the choice. A `remove` choice applies to the profiles the core lists now and, for each of their references,
///    to the secret the Keychain holds now (its identity is read). This happens before the tap boundary is set, so a
///    profile or secret written between this binding and the boundary is later state: the claim step leaves it alone
///    (its reference is held by a profile the choice did not bind, or its identity no longer matches), and a bound
///    profile or secret that disappears in that window needs nothing (revoke and the identity-conditioned deletion
///    are no-ops for it). Erring this way keeps data. A `keep` choice binds nothing.
/// 2. The tap. In one step, native staging is listed and the ingress boundary is held: a capture made from this moment
///    on is fresh-generation content, so its import waits (it stays staged and durable) and it is neither listed,
///    imported nor purged. Everything listed is by definition from before the tap.
/// 3. Record the tap as one durable request: the credential choice with what it binds, the list and whether an earlier
///    generation is still pending at the tap. Nothing else is written yet, so an interruption here leaves a request a
///    restart can only honor in full, never a half-started reset or a choice acted on early. While a request is
///    recorded the boundary stays held, in this process and, because the ingress gate holds it again at launch from the
///    recorded list, in the next.
/// 4. When the tap found an earlier reset unfinished (its generation pending, or native cleanup still outstanding for
///    it), finish that reset first (its cleanup, temporaries and recorded choice, steps 8 to 11), then, once it
///    completed, record the tap again as no longer awaiting it. Resuming the earlier generation only erases what existed
///    at the earlier tap, and its credential choice must be carried out before this tap's replaces it, so this tap never
///    counts it as its own: until it completes, the tap stays recorded, the boundary stays held and the report says
///    `pending`.
/// 5. Unless a generation this tap started is already pending: import the listed records through the production
///    ingress path (an imported capture becomes an item the core tombstones), then purge whatever of the list is still
///    unimported. A capture staged before the tap has no item for a tombstone to fence, so the purge must finish before
///    the core starts; if it cannot, the core is not started and the reset reports why.
/// 6. Start the core reset (or resume the pending one this tap started). The core tombstones every pre-reset item,
///    cancels its jobs and reminders and queues one durable cleanup row per effect.
/// 7. Only now carry the choice, with the profiles it binds, into its marker, mark native cleanup outstanding and clear
///    the request. Then release
///    the ingress boundary: the captures that waited import into the fresh generation. No item can be created while a
///    request is recorded, so a restart that starts the core again for that request finds no fresh capture to tombstone.
///    The outstanding marker is cleared only when a report says `completed`.
/// 8. Run the per-item cleanup executors (`DeletionCleanupService`), which perform each effect and ask the core to
///    record only the ones that actually finished.
/// 9. Remove export and share temporaries.
/// 10. When the user chose it, revoke every provider profile the choice bound and remove the secret that was stored
///    behind its reference when the choice was bound, behind the core's credential-claim fence
///    (`removeCredentialsIfChosen`): claim, revoke, delete only what was claimed, release. Without that choice neither
///    is touched, nothing is touched before step 6 completed, and a profile or secret configured after the binding is
///    never touched, not even by a retry of a removal that failed or was interrupted: the removal is conditioned on the
///    secret's identity at the binding, a bound reference that a profile configured since holds is never claimed and
///    keeps its secret, and while a reference is claimed storage refuses any profile that would start holding it.
/// 11. Ask the core to re-evaluate. The report says `completed` only when the core reports the generation completed
///    and every native step converged; this service never decides success on its own.
///
/// Every step is idempotent. A step that cannot finish is reported with a fixed blocker and repeated by the next
/// `reset` or `resume` call, so a crash at any point converges: a request that survives a crash before step 7 makes
/// `resume` hold the boundary at the recorded list and run steps 4 to 11; the markers of step 7 make it finish steps 8
/// to 11. A `reset` that ends before the core started withdraws the request (every recorded tap, so no earlier choice is
/// left armed) and releases the boundary; when it cannot, or when the call was cancelled or was a `resume`, the request
/// stays recorded, the boundary stays held and the report says `pending`, never `notStarted`. Calls are serialized.
///
/// A tap that finds an earlier tap still recorded asks the core first whether that tap's reset started (the recorded
/// tap notes the core's generation at its moment; a newer one is its own). A recorded tap the core never started is
/// superseded by the new explicit choice. One the core did start (step 6 ran, but the interruption came before step 7
/// carried its choice over) is carried over now, before anything else: its choice, with the profiles it binds, goes
/// into the markers and native cleanup is marked outstanding, so the new tap awaits it like any unfinished earlier reset
/// and its choice is carried out before the new one applies, whether the new tap is recorded, withdrawn or fails.
actor ResetService {
    private let core: ResetCore
    private let staging: ResetIngressStaging
    private let itemCleanup: ResetItemCleanup
    private let exportTemporaries: ResetExportTemporaries
    private let credentials: ResetCredentialRemoving
    private let markers: ResetMarkerStore
    private let changes: ItemChangePublishing?
    private var isRunning = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(
        core: ResetCore,
        staging: ResetIngressStaging,
        itemCleanup: ResetItemCleanup,
        exportTemporaries: ResetExportTemporaries,
        credentials: ResetCredentialRemoving,
        markers: ResetMarkerStore,
        changes: ItemChangePublishing? = nil
    ) {
        self.core = core
        self.staging = staging
        self.itemCleanup = itemCleanup
        self.exportTemporaries = exportTemporaries
        self.credentials = credentials
        self.markers = markers
        self.changes = changes
    }

    /// The user's reset tap with their explicit credential choice.
    func reset(credentialChoice: ResetCredentialChoice) async -> ResetReport {
        await acquire()
        defer { release() }

        // An earlier tap may still be recorded. Whether the core started its reset decides its fate, so until that is
        // known nothing is withdrawn: the recorded tap and whatever boundary it holds stay as they are.
        let recorded: ResetRequest?
        do {
            recorded = try markers.request()
        } catch {
            return ResetReport(state: .pending, progress: nil, blockers: [.markerUnreadable], itemCleanup: nil)
        }
        let current: ResetProgress?
        do {
            current = try await core.status()
        } catch {
            let failed = Self.coreFailed(error, progress: nil)
            return recorded == nil ? withdrawRequest(failed) : keepRequest(failed)
        }
        // A recorded tap the core started is an unfinished earlier reset whose choice is only in the request. Carry it
        // over now, so the new tap awaits it and nothing below can withdraw it.
        var earlierStarted = false
        if let recorded, recorded.coreHasStarted(latest: current) {
            do {
                try carryOver(recorded)
            } catch {
                return ResetReport(state: .pending, progress: current, blockers: [.markerNotRecorded], itemCleanup: nil)
            }
            earlierStarted = true
        }

        // A remove choice is bound before the tap boundary is set: it applies to the profiles that exist now and to the
        // secrets stored now behind their references, and both are recorded with it. Neither a retry nor a restart can
        // widen it to a profile configured or a secret entered later, and nothing written between this binding and the
        // boundary below is ever treated as tap-time state: the claim step leaves such a profile's reference unclaimed
        // and such a secret's changed identity unmatched.
        var removal: ResetCredentialRemoval?
        if credentialChoice == .remove {
            let profiles: [ResetProfileCredential]
            do {
                profiles = try await core.profileCredentials()
            } catch {
                return withdrawRequest(Self.coreFailed(error, progress: current), earlierOwed: earlierStarted)
            }
            do {
                removal = try ResetCredentialRemoval(binding: profiles) { try credentials.secretIdentity(reference: $0) }
            } catch {
                return withdrawRequest(
                    ResetReport(state: .notStarted, progress: current, blockers: [.credentialsUnreadable], itemCleanup: nil),
                    earlierOwed: earlierStarted)
            }
        }
        // The tap: the list and the boundary, in one step.
        let snapshot: ResetStagingSnapshot
        do {
            snapshot = try await staging.snapshot()
        } catch {
            return withdrawRequest(
                ResetReport(state: .notStarted, progress: current, blockers: [.stagingUnreadable], itemCleanup: nil),
                earlierOwed: earlierStarted)
        }
        // An earlier reset is unfinished while its generation is pending or, once the core completed it, while native
        // steps it owes (cleanup, temporaries, its credential choice) are still outstanding. A marker that cannot be read
        // counts as unfinished; `finishEarlierReset` decides again.
        let earlierOutstanding = (try? markers.isCleanupOutstanding()) ?? true
        let request = ResetRequest(
            credentialChoice: credentialChoice, credentialRemoval: removal, stagedBeforeTheTap: snapshot,
            awaitsEarlierReset: current?.phase == .pending || (current != nil && earlierOutstanding),
            latestGenerationAtTheTap: current?.generation)
        do {
            try markers.recordRequest(request)
        } catch {
            // An earlier tap the core never started may still be recorded. The user is told this one did not start, so
            // none may stay armed.
            return withdrawRequest(
                ResetReport(state: .notStarted, progress: current, blockers: [.markerNotRecorded], itemCleanup: nil),
                earlierOwed: earlierStarted)
        }
        let report = await performRequestedReset(request)
        guard report.state == .notStarted else { return report }
        return report.blockers.contains(.cancelled) ? keepRequest(report) : withdrawRequest(report)
    }

    /// Finishes an interrupted reset. Safe to call on every launch and foreground, repeatedly. A tap that was recorded but
    /// never confirmed by the core is run again in full, at the boundary recorded with it; otherwise it starts nothing and
    /// only completes what a started reset left (cleanup, temporaries, a recorded credential choice).
    func resume() async -> ResetReport {
        await acquire()
        defer { release() }

        let request: ResetRequest?
        do {
            request = try markers.request()
        } catch {
            // A recorded tap cannot be ruled out, so the ingress gate keeps holding and nothing is reported as not started.
            return ResetReport(state: .pending, progress: nil, blockers: [.markerUnreadable], itemCleanup: nil)
        }
        if let request {
            await staging.holdBoundary(for: request.stagedBeforeTheTap)
            return keepRequest(await performRequestedReset(request))
        }

        let progress: ResetProgress?
        do {
            progress = try await core.status()
        } catch {
            return Self.coreFailed(error, progress: nil)
        }
        let outstanding = (try? markers.isCleanupOutstanding()) ?? false
        guard let progress, progress.phase == .pending || outstanding else {
            return ResetReport(
                state: progress == nil ? .notStarted : .completed,
                progress: progress, blockers: [], itemCleanup: nil)
        }
        return await finish(from: progress)
    }

    /// Whether the native side still owes reset work: a tap whose request is recorded, or cleanup, temporaries or a
    /// credential choice marked outstanding. A marker that cannot be read counts as owed. Starts and changes nothing.
    func hasUnfinishedNativeWork() -> Bool {
        do {
            if try markers.request() != nil { return true }
        } catch {
            return true
        }
        return (try? markers.isCleanupOutstanding()) ?? true
    }

    /// Steps 4 to 11 for a recorded request, with the boundary held. The request stays recorded, and the boundary held,
    /// until the core has started a generation for it and the choice is carried over.
    private func performRequestedReset(_ recorded: ResetRequest) async -> ResetReport {
        var request = recorded
        if request.awaitsEarlierReset {
            if let stopped = await finishEarlierReset() { return stopped }
            let ownTurn = ResetRequest(
                credentialChoice: request.credentialChoice, credentialRemoval: request.credentialRemoval,
                stagedBeforeTheTap: request.stagedBeforeTheTap,
                latestGenerationAtTheTap: request.latestGenerationAtTheTap)
            do {
                try markers.recordRequest(ownTurn)
            } catch {
                // The tap still awaits the (now completed) earlier generation, which the next attempt finds finished.
                return ResetReport(state: .pending, progress: nil, blockers: [.markerNotRecorded], itemCleanup: nil)
            }
            request = ownTurn
        }
        let started: ResetProgress
        switch await startCore(request.stagedBeforeTheTap) {
        case .stopped(let report): return report
        case .started(let progress): started = progress
        }
        do {
            try carryOver(request)
            try markers.clearRequest()
        } catch {
            // The request is still recorded, so the next resume (or tap) carries the choice over before acting on it.
            return ResetReport(state: .pending, progress: started, blockers: [.markerNotRecorded], itemCleanup: nil)
        }
        staging.endBoundary()
        return await finish(from: started)
    }

    /// Step 7 for a tap whose reset the core has started: its choice, with the profiles it binds, becomes the removal
    /// marker (or clears one that a `keep` choice replaces), and native cleanup is marked outstanding. Idempotent: an
    /// owed removal already recorded for the same started reset stays as it is.
    private func carryOver(_ request: ResetRequest) throws {
        if let removal = request.credentialRemoval {
            try markers.recordRemovalRequested(removal)
        } else {
            try markers.clearRemovalRequest()
        }
        try markers.recordCleanupOutstanding()
    }

    private enum CoreStart {
        case started(ResetProgress)
        case stopped(ResetReport)
    }

    /// Finishes the reset that was unfinished when the tap was made: its generation and every native step it owes,
    /// including its credential choice, so a later tap's choice never replaces one that was not yet carried out. Returns
    /// nil once that reset completed, or the `pending` report of what keeps it from completing; the tap stays recorded
    /// either way.
    private func finishEarlierReset() async -> ResetReport? {
        let progress: ResetProgress?
        do {
            progress = try await core.status()
        } catch {
            let blocker: ResetBlocker = Task.isCancelled || error is CancellationError ? .cancelled : .coreUnavailable
            return ResetReport(state: .pending, progress: nil, blockers: [blocker], itemCleanup: nil)
        }
        let outstanding: Bool
        do {
            outstanding = try markers.isCleanupOutstanding()
        } catch {
            return ResetReport(state: .pending, progress: progress, blockers: [.markerUnreadable], itemCleanup: nil)
        }
        guard let progress, progress.phase == .pending || outstanding else { return nil }
        let earlier = await finish(from: progress)
        return earlier.state == .completed ? nil : earlier
    }

    /// Drains the pre-tap staging in `snapshot` and starts the core. A generation that is pending here was started by
    /// this request before an interruption (a tap made while an earlier one was pending has finished that one first), so
    /// it is resumed without draining staging again.
    private func startCore(_ snapshot: ResetStagingSnapshot) async -> CoreStart {
        let existing: ResetProgress?
        do {
            existing = try await core.status()
        } catch {
            return .stopped(Self.coreFailed(error, progress: nil))
        }
        if existing?.phase != .pending {
            if let blocker = await prepareStaging(snapshot) {
                return .stopped(ResetReport(
                    state: .notStarted, progress: existing, blockers: [blocker], itemCleanup: nil))
            }
        }
        if Task.isCancelled { return .stopped(Self.cancelled(progress: existing)) }

        do {
            return .started(try await core.start())
        } catch {
            return .stopped(Self.coreFailed(error, progress: existing))
        }
    }

    /// Withdraws every recorded tap for a report that says the reset did not start, then releases the boundary. When the
    /// withdrawal fails, a restart would run the request that remains, so the boundary stays held and the report says
    /// `pending` with the reason. When an earlier started reset is owed (`earlierOwed`: its choice was carried over at
    /// this tap, so withdrawing the request loses nothing), the report says `pending` too: the next `resume` finishes it.
    private func withdrawRequest(_ report: ResetReport, earlierOwed: Bool = false) -> ResetReport {
        do {
            try markers.clearRequest()
        } catch {
            return ResetReport(
                state: .pending, progress: report.progress, blockers: report.blockers + [.markerNotCleared],
                itemCleanup: report.itemCleanup)
        }
        staging.endBoundary()
        return earlierOwed ? keepRequest(report) : report
    }

    /// The request stays recorded and the boundary held, so a report that the reset did not start says `pending`: the
    /// next `resume` runs it.
    private func keepRequest(_ report: ResetReport) -> ResetReport {
        guard report.state == .notStarted else { return report }
        return ResetReport(
            state: .pending, progress: report.progress, blockers: report.blockers, itemCleanup: report.itemCleanup)
    }

    // MARK: Steps

    /// Imports and purges the pre-tap staging in `snapshot`. Returns the reason the core must not start yet, or nil when
    /// staging holds nothing from before the tap.
    private func prepareStaging(_ snapshot: ResetStagingSnapshot) async -> ResetBlocker? {
        // A record that cannot be imported stays staged, and the purge judges what remains.
        await staging.importSnapshot(snapshot)
        if Task.isCancelled { return .cancelled }
        switch await staging.purgeUnimported(from: snapshot) {
        case .purged: return nil
        case .incomplete: return .stagingPurgeIncomplete
        case .unknown: return .stagingUnreadable
        }
    }

    private func finish(from started: ResetProgress) async -> ResetReport {
        var blockers: [ResetBlocker] = []

        let cleanup = await itemCleanup.reconcile()
        if !cleanup.converged { blockers.append(.itemCleanupIncomplete) }
        if Task.isCancelled { return Self.cancelled(progress: started, itemCleanup: cleanup) }

        if !exportTemporaries.removeAll() { blockers.append(.exportTemporariesRemain) }

        if let credentialBlocker = await removeCredentialsIfChosen() {
            blockers.append(credentialBlocker)
        }

        // The core is asked last so a completed generation is never reported ahead of the native steps.
        let latest: ResetProgress
        do {
            latest = try await core.advance() ?? started
        } catch {
            blockers.append(.coreUnavailable)
            return ResetReport(state: .pending, progress: started, blockers: blockers, itemCleanup: cleanup)
        }
        if latest.phase == .pending { blockers.append(.coreResetPending) }
        if blockers.isEmpty, latest.phase == .completed {
            do {
                try markers.clearCleanupOutstanding()
            } catch {
                blockers.append(.markerNotCleared)
            }
        }
        let state: ResetState = blockers.isEmpty && latest.phase == .completed ? .completed : .pending
        if state == .completed { changes?.publish(.allItems) }
        return ResetReport(state: state, progress: latest, blockers: blockers, itemCleanup: cleanup)
    }

    /// Carries out an owed `remove` choice on exactly the profiles and secrets it binds, behind the core's
    /// credential-claim fence (schema v10), so that no profile can come to hold a secret this removal deletes:
    /// 1. Claim. One core transaction claims each bound secret's reference that no profile outside the bound ones holds.
    ///    Such a profile was configured after the choice was bound (or the choice never bound it), so its secret stays,
    ///    unchanged or not. From the claim until step 4, storage refuses any profile insert or edit that would take a claimed
    ///    reference. On a retry the core returns the persisted claim set unchanged: it is the authority, never a new
    ///    snapshot.
    /// 2. Revoke the bound profiles the core still lists as not revoked, so their jobs retire and nothing dispatches
    ///    before a secret goes.
    /// 3. Delete the Keychain item behind each claimed reference, conditioned on the identity the secret had when the
    ///    choice was bound: a secret the user entered since behind the same reference stays.
    /// 4. Release the claims, then clear the removal marker, only once every deletion was acknowledged. An interruption
    ///    between the two leaves the marker, so the next pass claims again; every claimed secret is gone by then, so that
    ///    pass deletes nothing and only releases and clears.
    private func removeCredentialsIfChosen() async -> ResetBlocker? {
        let removal: ResetCredentialRemoval?
        do {
            removal = try markers.removalRequest()
        } catch {
            return .credentialRemovalIncomplete
        }
        guard let removal else { return nil }

        let bound = removal.profileVersions
        let claimed: Set<String>
        do {
            let claimedReferences = try await core.claimCredentials(
                credentialReferences: removal.secrets.map(\.credentialReference).sorted(),
                boundProfileVersions: bound.sorted())
            claimed = Set(claimedReferences)
        } catch {
            return .credentialRemovalIncomplete
        }

        let profiles: [ResetProfileCredential]
        do {
            profiles = try await core.profileCredentials()
        } catch {
            return .credentialRemovalIncomplete
        }
        // Revoking first retires queued jobs and stops new dispatch before the secret goes.
        var incomplete = false
        for profile in profiles where bound.contains(profile.profileVersion) && !profile.isRevoked {
            do {
                try await core.revokeProfile(profileVersion: profile.profileVersion)
            } catch {
                incomplete = true
            }
        }
        // Only a claimed reference is deleted: an unclaimed one is held by a profile the choice did not bind.
        for secret in removal.secrets.sorted(by: { $0.credentialReference < $1.credentialReference })
        where claimed.contains(secret.credentialReference) {
            do {
                try credentials.deleteCredential(reference: secret.credentialReference, identity: secret.identity)
            } catch CredentialError.invalidReference {
                // A reference that names no Keychain item has no secret to remove.
            } catch {
                incomplete = true
            }
        }
        if incomplete { return .credentialRemovalIncomplete }
        do {
            try await core.releaseCredentialClaims()
            try markers.clearRemovalRequest()
        } catch {
            return .credentialRemovalIncomplete
        }
        return nil
    }

    // MARK: Reports

    private static func coreFailed(_ error: Error, progress: ResetProgress?) -> ResetReport {
        if Task.isCancelled || error is CancellationError { return cancelled(progress: progress) }
        return ResetReport(
            state: progress?.phase == .pending ? .pending : .notStarted,
            progress: progress, blockers: [.coreUnavailable], itemCleanup: nil)
    }

    private static func cancelled(progress: ResetProgress?, itemCleanup: DeletionReconcileReport? = nil) -> ResetReport {
        ResetReport(
            state: progress?.phase == .pending ? .pending : .notStarted,
            progress: progress, blockers: [.cancelled], itemCleanup: itemCleanup)
    }

    // MARK: Serialization

    private func acquire() async {
        if isRunning {
            await withCheckedContinuation { (waiter: CheckedContinuation<Void, Never>) in
                waiters.append(waiter)
            }
        } else {
            isRunning = true
        }
    }

    private func release() {
        if waiters.isEmpty {
            isRunning = false
        } else {
            waiters.removeFirst().resume()
        }
    }
}
