import Foundation

/// Coordinates the native effects around the durable Rust reset. The core owns the generation, the boundary, the
/// per-item tombstones and the rule for completion; this service owns only what the core cannot reach: staged native
/// inputs, export temporaries, notifications, audio files and the optional credential removal.
///
/// A reset runs in this order:
/// 1. The tap. In one step, native staging is listed and the ingress boundary is held: a capture made from this moment
///    on is fresh-generation content, so its import waits (it stays staged and durable) until step 4 and it is neither
///    listed, imported nor purged. Everything listed is by definition from before the tap.
/// 2. Record the tap and its credential choice as one durable request. Nothing else is written yet, so an interruption
///    here leaves a request a restart can only honor in full, never a half-started reset or a choice acted on early.
/// 3. Unless a reset is already pending: import the listed records through the production ingress path (an imported
///    capture becomes an item the core tombstones), then purge whatever of the list is still unimported. A capture
///    staged before the tap has no item for a tombstone to fence, so the purge must finish before the core starts; if it
///    cannot, the core is not started and the reset reports why.
/// 4. Start the core reset (or resume the pending one). The core tombstones every pre-reset item, cancels its jobs and
///    reminders and queues one durable cleanup row per effect. Then, on every path, the ingress boundary is released and
///    the captures that waited import into the fresh generation.
/// 5. Only now carry the choice into its markers, mark native cleanup outstanding and drop the request. The outstanding
///    marker is cleared only when a report says `completed`.
/// 6. Run the per-item cleanup executors (`DeletionCleanupService`), which perform each effect and ask the core to
///    record only the ones that actually finished.
/// 7. Remove export and share temporaries.
/// 8. When the user chose it, revoke every provider profile and remove its credential. Without that choice neither is
///    touched, and nothing is touched before step 4 completed.
/// 9. Ask the core to re-evaluate. The report says `completed` only when the core reports the generation completed and
///    every native step converged; this service never decides success on its own.
///
/// Every step is idempotent. A step that cannot finish is reported with a fixed blocker and repeated by the next
/// `reset` or `resume` call, so a crash at any point converges: a request that survives a crash before step 5 makes
/// `resume` run the tap again, as of that launch, and steps 3 to 9; the markers of step 5 make it finish steps 6 to 9.
/// A report never says `notStarted` while the request is still recorded: it says `pending`, because a restart will run
/// it. Calls are serialized.
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

        let snapshot: ResetStagingSnapshot
        do {
            snapshot = try await staging.snapshot()
        } catch {
            return ResetReport(state: .notStarted, progress: nil, blockers: [.stagingUnreadable], itemCleanup: nil)
        }
        do {
            try markers.recordRequested(credentialChoice)
        } catch {
            staging.endBoundary()
            return ResetReport(state: .notStarted, progress: nil, blockers: [.markerNotRecorded], itemCleanup: nil)
        }
        let report = await performRequestedReset(credentialChoice, snapshot: snapshot)
        return settleUnstartedRequest(report, withdraw: true)
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
            let snapshot: ResetStagingSnapshot
            do {
                snapshot = try await staging.snapshot()
            } catch {
                return ResetReport(state: .pending, progress: nil, blockers: [.stagingUnreadable], itemCleanup: nil)
            }
            let report = await performRequestedReset(requested, snapshot: snapshot)
            return settleUnstartedRequest(report, withdraw: false)
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

    /// Steps 3 to 9 for a request that is recorded. The request stays recorded until the core has started.
    private func performRequestedReset(
        _ credentialChoice: ResetCredentialChoice, snapshot: ResetStagingSnapshot
    ) async -> ResetReport {
        let started: ResetProgress
        switch await startCoreBehindBoundary(snapshot) {
        case .stopped(let report): return report
        case .started(let progress): started = progress
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

    private enum CoreStart {
        case started(ResetProgress)
        case stopped(ResetReport)
    }

    /// Drains native staging and starts the core while the ingress boundary is held, then releases the boundary on every
    /// path, so a capture that waited is never stranded by a failed or cancelled reset.
    private func startCoreBehindBoundary(_ snapshot: ResetStagingSnapshot) async -> CoreStart {
        defer { staging.endBoundary() }

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

    /// A request that did not start must not be reported as `notStarted` while it is still recorded: a restart runs it.
    /// `reset` withdraws a request it is reporting as not started, and says so when it cannot; `resume` and a cancelled
    /// call leave the request recorded and report it as pending.
    private func settleUnstartedRequest(_ report: ResetReport, withdraw: Bool) -> ResetReport {
        guard report.state == .notStarted else { return report }
        var blockers = report.blockers
        if withdraw, !blockers.contains(.cancelled) {
            do {
                try markers.clearRequested()
                return report
            } catch {
                blockers.append(.markerNotCleared)
            }
        }
        return ResetReport(state: .pending, progress: report.progress, blockers: blockers, itemCleanup: report.itemCleanup)
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
