import Foundation

/// Coordinates the native effects around the durable Rust reset. The core owns the generation, the boundary, the
/// per-item tombstones and the rule for completion; this service owns only what the core cannot reach: staged native
/// inputs, export temporaries, notifications, audio files and the optional credential removal.
///
/// A reset runs in this order:
/// 1. Record the tap and its credential choice as one durable request. Nothing else is written yet, so an interruption
///    here leaves a request a restart can only honor in full, never a half-started reset or a choice acted on early.
/// 2. Unless a reset is already pending: snapshot native staging, import the snapshot's records through the production
///    ingress path (an imported capture becomes an item the core tombstones), then purge whatever of the snapshot is
///    still unimported. A capture staged before the tap has no item for a tombstone to fence, so the purge must finish
///    before the core starts; if it cannot, the core is not started and the reset reports why. Inputs staged after the
///    snapshot are fresh-generation content: they are neither imported nor purged. (A capture that the live ingress path
///    imports before the core records the boundary is, by that boundary, earlier content and is tombstoned.)
/// 3. Start the core reset (or resume the pending one). The core tombstones every pre-reset item, cancels its jobs and
///    reminders and queues one durable cleanup row per effect.
/// 4. Only now carry the choice into its markers, mark native cleanup outstanding and drop the request. The outstanding
///    marker is cleared only when a report says `completed`.
/// 5. Run the per-item cleanup executors (`DeletionCleanupService`), which perform each effect and ask the core to
///    record only the ones that actually finished.
/// 6. Remove export and share temporaries.
/// 7. When the user chose it, revoke every provider profile and remove its credential. Without that choice neither is
///    touched, and nothing is touched before step 3 completed.
/// 8. Ask the core to re-evaluate. The report says `completed` only when the core reports the generation completed and
///    every native step converged; this service never decides success on its own.
///
/// Every step is idempotent. A step that cannot finish is reported with a fixed blocker and repeated by the next
/// `reset` or `resume` call, so a crash at any point converges: a request that survives a crash before step 4 makes
/// `resume` run steps 2 to 8 again, and the markers of step 4 make it finish steps 5 to 8. Calls are serialized.
actor ResetService {
    private let core: ResetCore
    private let staging: ResetIngressStaging
    private let itemCleanup: ResetItemCleanup
    private let exportTemporaries: ResetExportTemporaries
    private let credentials: ResetCredentialRemoving
    private let markers: ResetMarkerStore
    private var isRunning = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(
        core: ResetCore,
        staging: ResetIngressStaging,
        itemCleanup: ResetItemCleanup,
        exportTemporaries: ResetExportTemporaries,
        credentials: ResetCredentialRemoving,
        markers: ResetMarkerStore
    ) {
        self.core = core
        self.staging = staging
        self.itemCleanup = itemCleanup
        self.exportTemporaries = exportTemporaries
        self.credentials = credentials
        self.markers = markers
    }

    /// The user's reset tap with their explicit credential choice.
    func reset(credentialChoice: ResetCredentialChoice) async -> ResetReport {
        await acquire()
        defer { release() }

        do {
            try markers.recordRequested(credentialChoice)
        } catch {
            return ResetReport(state: .notStarted, progress: nil, blockers: [.markerNotRecorded], itemCleanup: nil)
        }
        let report = await performRequestedReset(credentialChoice)
        // The user was told this reset did not start, so a restart must not start it behind their back.
        if report.state == .notStarted, !report.blockers.contains(.cancelled) {
            try? markers.clearRequested()
        }
        return report
    }

    /// Finishes an interrupted reset. Safe to call on every launch and foreground, repeatedly. A tap that was recorded but
    /// never confirmed by the core is run again in full; otherwise it starts nothing and only completes what a started
    /// reset left (cleanup, temporaries, a recorded credential choice).
    func resume() async -> ResetReport {
        await acquire()
        defer { release() }

        let requested: ResetCredentialChoice?
        do {
            requested = try markers.requestedChoice()
        } catch {
            return ResetReport(state: .notStarted, progress: nil, blockers: [.markerUnreadable], itemCleanup: nil)
        }
        if let requested {
            return await performRequestedReset(requested)
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

    /// Steps 2 to 8 for a request that is recorded. The request stays recorded until the core has started.
    private func performRequestedReset(_ credentialChoice: ResetCredentialChoice) async -> ResetReport {
        let existing: ResetProgress?
        do {
            existing = try await core.status()
        } catch {
            return Self.coreFailed(error, progress: nil)
        }
        if existing?.phase != .pending {
            if let blocker = await prepareStaging() {
                return ResetReport(
                    state: .notStarted, progress: existing, blockers: [blocker], itemCleanup: nil)
            }
        }
        if Task.isCancelled { return Self.cancelled(progress: existing) }

        let started: ResetProgress
        do {
            started = try await core.start()
        } catch {
            return Self.coreFailed(error, progress: existing)
        }
        do {
            switch credentialChoice {
            case .remove: try markers.recordRemovalRequested()
            case .keep: try markers.clearRemovalRequest()
            }
            try markers.recordCleanupOutstanding()
            try markers.clearRequested()
        } catch {
            // The request is still recorded, so the next resume carries the choice over before acting on it.
            return ResetReport(state: .pending, progress: started, blockers: [.markerNotRecorded], itemCleanup: nil)
        }
        return await finish(from: started)
    }

    // MARK: Steps

    /// Imports and purges native staging. Returns the reason the core must not start yet, or nil when staging holds
    /// nothing from before the tap.
    private func prepareStaging() async -> ResetBlocker? {
        let snapshot: ResetStagingSnapshot
        do {
            snapshot = try staging.snapshot()
        } catch {
            return .stagingUnreadable
        }
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
        return ResetReport(state: state, progress: latest, blockers: blockers, itemCleanup: cleanup)
    }

    private func removeCredentialsIfChosen() async -> ResetBlocker? {
        let requested: Bool
        do {
            requested = try markers.isRemovalRequested()
        } catch {
            return .credentialRemovalIncomplete
        }
        guard requested else { return nil }

        let profiles: [ResetProfileCredential]
        do {
            profiles = try await core.profileCredentials()
        } catch {
            return .credentialRemovalIncomplete
        }
        // Revoking first retires queued jobs and stops new dispatch before the secret goes.
        var incomplete = false
        for profile in profiles where !profile.isRevoked {
            do {
                try await core.revokeProfile(profileVersion: profile.profileVersion)
            } catch {
                incomplete = true
            }
        }
        let references = Set(profiles.compactMap { $0.credentialReference }).sorted()
        for reference in references {
            do {
                try credentials.deleteCredential(reference: reference)
            } catch CredentialError.invalidReference {
                // A reference that names no Keychain item has no secret to remove.
            } catch {
                incomplete = true
            }
        }
        if incomplete { return .credentialRemovalIncomplete }
        do {
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
