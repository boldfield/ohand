import Foundation

/// Coordinates the native effects around the durable Rust reset. The core owns the generation, the boundary, the
/// per-item tombstones and the rule for completion; this service owns only what the core cannot reach: staged native
/// inputs, export temporaries, notifications, audio files and the optional credential removal.
///
/// A reset runs in this order:
/// 1. The tap. In one step, native staging is listed and the ingress boundary is held: a capture made from this moment
///    on is fresh-generation content, so its import waits (it stays staged and durable) and it is neither listed,
///    imported nor purged. Everything listed is by definition from before the tap.
/// 2. Record the tap as one durable request: the credential choice, the list and whether an earlier generation is still
///    pending at the tap. Nothing else is written yet, so an interruption here leaves a request a restart can only honor
///    in full, never a half-started reset or a choice acted on early. While a request is recorded the boundary stays
///    held, in this process and, because the ingress gate holds it again at launch from the recorded list, in the next.
/// 3. When the tap found an earlier reset unfinished (its generation pending, or native cleanup still outstanding for
///    it), finish that reset first (its cleanup, temporaries and recorded choice, steps 7 to 10), then, once it
///    completed, record the tap again as no longer awaiting it. Resuming the earlier generation only erases what existed
///    at the earlier tap, and its credential choice must be carried out before this tap's replaces it, so this tap never
///    counts it as its own: until it completes, the tap stays recorded, the boundary stays held and the report says
///    `pending`.
/// 4. Unless a generation this tap started is already pending: import the listed records through the production
///    ingress path (an imported capture becomes an item the core tombstones), then purge whatever of the list is still
///    unimported. A capture staged before the tap has no item for a tombstone to fence, so the purge must finish before
///    the core starts; if it cannot, the core is not started and the reset reports why.
/// 5. Start the core reset (or resume the pending one this tap started). The core tombstones every pre-reset item,
///    cancels its jobs and reminders and queues one durable cleanup row per effect.
/// 6. Only now carry the choice into its markers, mark native cleanup outstanding and clear the request. Then release
///    the ingress boundary: the captures that waited import into the fresh generation. No item can be created while a
///    request is recorded, so a restart that starts the core again for that request finds no fresh capture to tombstone.
///    The outstanding marker is cleared only when a report says `completed`.
/// 7. Run the per-item cleanup executors (`DeletionCleanupService`), which perform each effect and ask the core to
///    record only the ones that actually finished.
/// 8. Remove export and share temporaries.
/// 9. When the user chose it, revoke every provider profile and remove its credential. Without that choice neither is
///    touched, and nothing is touched before step 5 completed.
/// 10. Ask the core to re-evaluate. The report says `completed` only when the core reports the generation completed
///    and every native step converged; this service never decides success on its own.
///
/// Every step is idempotent. A step that cannot finish is reported with a fixed blocker and repeated by the next
/// `reset` or `resume` call, so a crash at any point converges: a request that survives a crash before step 6 makes
/// `resume` hold the boundary at the recorded list and run steps 3 to 10; the markers of step 6 make it finish steps 7
/// to 10. A `reset` that ends before the core started withdraws the request (every recorded tap, so no earlier choice is
/// left armed) and releases the boundary; when it cannot, or when the call was cancelled or was a `resume`, the request
/// stays recorded, the boundary stays held and the report says `pending`, never `notStarted`. Calls are serialized.
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
            return withdrawRequest(
                ResetReport(state: .notStarted, progress: nil, blockers: [.stagingUnreadable], itemCleanup: nil))
        }
        let current: ResetProgress?
        do {
            current = try await core.status()
        } catch {
            return withdrawRequest(Self.coreFailed(error, progress: nil))
        }
        // An earlier reset is unfinished while its generation is pending or, once the core completed it, while native
        // steps it owes (cleanup, temporaries, its credential choice) are still outstanding. A marker that cannot be read
        // counts as unfinished; `finishEarlierReset` decides again.
        let earlierOutstanding = (try? markers.isCleanupOutstanding()) ?? true
        let request = ResetRequest(
            credentialChoice: credentialChoice, stagedBeforeTheTap: snapshot,
            awaitsEarlierReset: current?.phase == .pending || (current != nil && earlierOutstanding))
        do {
            try markers.recordRequest(request)
        } catch {
            // An earlier tap may still be recorded. The user is told this one did not start, so none may stay armed.
            return withdrawRequest(
                ResetReport(state: .notStarted, progress: nil, blockers: [.markerNotRecorded], itemCleanup: nil))
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

    /// Steps 3 to 10 for a recorded request, with the boundary held. The request stays recorded, and the boundary held,
    /// until the core has started a generation for it and the choice is carried over.
    private func performRequestedReset(_ recorded: ResetRequest) async -> ResetReport {
        var request = recorded
        if request.awaitsEarlierReset {
            if let stopped = await finishEarlierReset() { return stopped }
            let ownTurn = ResetRequest(
                credentialChoice: request.credentialChoice, stagedBeforeTheTap: request.stagedBeforeTheTap)
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
            switch request.credentialChoice {
            case .remove: try markers.recordRemovalRequested()
            case .keep: try markers.clearRemovalRequest()
            }
            try markers.recordCleanupOutstanding()
            try markers.clearRequest()
        } catch {
            // The request is still recorded, so the next resume carries the choice over before acting on it.
            return ResetReport(state: .pending, progress: started, blockers: [.markerNotRecorded], itemCleanup: nil)
        }
        staging.endBoundary()
        return await finish(from: started)
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
    /// `pending` with the reason.
    private func withdrawRequest(_ report: ResetReport) -> ResetReport {
        do {
            try markers.clearRequest()
        } catch {
            return ResetReport(
                state: .pending, progress: report.progress, blockers: report.blockers + [.markerNotCleared],
                itemCleanup: report.itemCleanup)
        }
        staging.endBoundary()
        return report
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
