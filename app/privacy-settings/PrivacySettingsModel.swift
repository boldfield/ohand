import Combine
import Foundation
import OhAndCoreBridge
import OhAndServices

/// The route-policy, profile and queued-work operations this screen uses. `PrivacySettingsServices` is the production
/// implementation; it forwards to `ProviderConfigurationService` and `JobConfigurationService` and adds none.
protocol PrivacySettingsControlling: AnyObject {
    func listRoutePolicy() async throws -> [RoutePolicyRoute]
    func listProfiles() async throws -> ProviderProfileListAnswer
    func grant(routeID: String, capability: String, profileVersion: String) async throws
        -> RoutePolicyChangeOutcome.Disposition
    func revoke(routeID: String, capability: String) async throws -> RoutePolicyChangeOutcome.Disposition
    func revokeProfile(_ profileVersion: String) async throws
    func jobsPinned(toProfile profileVersion: String, limit: Int?) async throws -> JobConfigurationPinnedJobs
    func requeue(jobID: String, newJobID: String, toProfileVersion: String) async throws
        -> JobConfigurationRequeuedJob
    func requeueRecord(forJob jobID: String) async throws -> JobConfigurationRequeueRecord?
    func repinWaitingWork(routeID: String) async throws -> Int
    /// The core's processing facts for one note: whether a reply was applied, or why its processing stopped.
    func itemProcessing(itemID: String) async throws -> PrivacyItemProcessing
}

/// The production controls: the root's provider configuration for routes and profiles, its job configuration for
/// work queued behind them, and its gated item status read for what became of that work.
final class PrivacySettingsServices: PrivacySettingsControlling {
    typealias ItemStatusRead = (String, @escaping (Result<ItemStatusReport, CoreFailure>) -> Void) -> Void

    private let providers: ProviderConfigurationService
    private let jobs: JobConfigurationService
    private let itemStatus: ItemStatusRead

    init(providers: ProviderConfigurationService, jobs: JobConfigurationService, itemStatus: @escaping ItemStatusRead) {
        self.providers = providers
        self.jobs = jobs
        self.itemStatus = itemStatus
    }

    func listRoutePolicy() async throws -> [RoutePolicyRoute] { try await providers.listRoutePolicy() }
    func listProfiles() async throws -> ProviderProfileListAnswer { try await providers.listProfiles() }

    func grant(routeID: String, capability: String, profileVersion: String) async throws
        -> RoutePolicyChangeOutcome.Disposition
    {
        try await providers.grant(routeID: routeID, capability: capability, profileVersion: profileVersion)
    }

    func revoke(routeID: String, capability: String) async throws -> RoutePolicyChangeOutcome.Disposition {
        try await providers.revoke(routeID: routeID, capability: capability)
    }

    func revokeProfile(_ profileVersion: String) async throws { try await jobs.revokeProfile(profileVersion) }

    func jobsPinned(toProfile profileVersion: String, limit: Int?) async throws -> JobConfigurationPinnedJobs {
        try await jobs.jobsPinned(toProfile: profileVersion, limit: limit)
    }

    func requeue(jobID: String, newJobID: String, toProfileVersion: String) async throws
        -> JobConfigurationRequeuedJob
    {
        try await jobs.requeue(jobID: jobID, newJobID: newJobID, toProfileVersion: toProfileVersion)
    }

    func requeueRecord(forJob jobID: String) async throws -> JobConfigurationRequeueRecord? {
        try await jobs.requeueRecord(forJob: jobID)
    }

    func repinWaitingWork(routeID: String) async throws -> Int { try await jobs.repinWaitingWork(routeID: routeID) }

    func itemProcessing(itemID: String) async throws -> PrivacyItemProcessing {
        let result = await withCheckedContinuation { continuation in
            itemStatus(itemID) { continuation.resume(returning: $0) }
        }
        switch result {
        case .success(let report):
            return PrivacyItemProcessing(
                processingState: report.processingState, processingJobStatus: report.processingJobStatus)
        case .failure(let failure):
            // A read refused for the session is a lock, like every other read on this screen.
            if failure.errorClass == .unauthorized { throw JobConfigurationError.notAuthenticated }
            throw JobConfigurationError.core(failure)
        }
    }
}

/// What the core reports about one note's processing: `processed` once a reply was applied, and for work that stopped,
/// why (`awaiting_configuration` when the core refused it under the current permissions). Core strings, shown as read.
struct PrivacyItemProcessing: Equatable {
    let processingState: String
    let processingJobStatus: String?
}

/// A job that was queued or running when the user last took a route permission away. It is followed by reading the core
/// again, so the screen shows what actually became of it rather than what the change was expected to do.
///
/// The core's pinned-job listing carries no job type, and its item status describes only a note's text sorting. So the
/// job's own state comes only from the listing, and the note's sorting is kept apart as a fact about the note: a job
/// that is not interpretation (a shadow review, say) is never described by its note's sorting.
struct PrivacyFollowedJob: Equatable {
    enum Listing: Equatable {
        /// The core lists it as `running` (a request already sent and not yet answered) or `queued`.
        case listed(String)
        /// The core's full listing for its profile version no longer has it as queued or running: it ended or was
        /// retired. The listing does not say how.
        case notListed
        /// Its profile version's listing could not be read in full just now; reading again may answer.
        case unknown
    }

    enum Note: Equatable {
        /// The note's text sorting as the core reports it now. It may describe other work on the same note.
        case reported(PrivacyItemProcessing)
        /// Not readable just now; reading again may answer.
        case unreadable
    }

    let jobID: String
    let itemID: String
    let profileVersion: String
    let listing: Listing
    /// The note's text sorting, read for every job not listed as running.
    let note: Note?
    /// Whether the screen has seen the core list this job as `running`. Once its request has left the device, a later
    /// `queued` listing (the core backing off a refused reply) never makes it a request that was not sent.
    let requestSent: Bool
}

/// The one permission the user is being asked about: a single capability on a single route for the destinations of a
/// single stored profile version. Confirming it grants exactly this and nothing else.
struct PrivacyGrantProposal: Equatable {
    let routeID: String
    let routeLabel: String
    let capability: String
    let profileVersion: String
    let versionLabel: String
}

/// A stored profile version the user could allow a capability for.
struct PrivacyOfferedVersion: Equatable {
    let profileID: String
    let version: ProviderProfileListAnswer.Version
}

/// One deliberate change that takes permission away or moves waiting work. Confirming it does exactly this and nothing
/// else; none of them grants anything.
struct PrivacyChangeProposal: Equatable {
    enum Target: Equatable {
        /// Stop sending `capability` from the route to any destination.
        case revokeRouteCapability(routeID: String, routeLabel: String, capability: String)
        /// Revoke a stored profile version; work queued for it is retired, not moved.
        case revokeProfileVersion(profileVersion: String, versionLabel: String)
        /// Queue the route's waiting, unpinned notes with the provider the core selects now, if it is authorized.
        case repinWaitingWork(routeID: String, routeLabel: String)
    }

    let target: Target
}

/// One queued or running job, moved to another stored profile version only if the user confirms it. `newJobID` is the
/// identity of this one user action: it is kept across retries so repeating the action answers the same replacement.
struct PrivacyRequeueProposal: Equatable {
    let jobID: String
    let newJobID: String
    let fromVersionLabel: String
    let toProfileVersion: String
    let toVersionLabel: String
}

/// Work queued or running behind one unrevoked stored profile version. Item and status only; no content.
struct PrivacyQueuedVersion: Equatable {
    let profileID: String
    let version: ProviderProfileListAnswer.Version
    let jobs: JobConfigurationPinnedJobs
}

/// A job the core listed as queued or running just before a profile revocation and no longer lists afterwards. The
/// revocation retired every job still pending when it ran; a job that ended in the moment between the two reads kept
/// its own outcome. The listing does not say which, so the screen claims neither: it offers the move the core accepts
/// only for a job it retired for the revocation, and shows the core's refusal otherwise.
struct PrivacyUnlistedJob: Equatable {
    let jobID: String
    let itemID: String
    /// Whether the core listed it as `running` (request already sent) just before the revocation.
    let requestSent: Bool
}

/// The work pending for one profile version just before its revocation and not listed afterwards, remembered from
/// that moment so the user can still move it deliberately. The core lists only queued and running jobs, so this is the
/// only place such work stays visible.
struct PrivacyUnlistedVersion: Equatable {
    let profileVersion: String
    let versionLabel: String
    let jobs: [PrivacyUnlistedJob]
}

enum PrivacySettingsPhase: Equatable {
    case idle
    case loading
    case loaded
    /// The read session is not authenticated, so no stored state is shown.
    case locked
    case failed(String)
}

enum PrivacySettingsActionState: Equatable {
    case working
    case allowed(String)
    /// A revocation or a queued-work choice the core accepted, with the state it actually left.
    case changed(String)
    case cancelled
    case failed(String)

    var message: String {
        switch self {
        case .working: return "Working…"
        case .allowed(let message): return message
        case .changed(let message): return message
        case .cancelled: return PrivacySettingsText.cancelledNotice
        case .failed(let message): return message
        }
    }
}

/// The state of the processing-policy screen: each route with what it may send and where, and the deliberate steps that
/// allow one more capability on one route for one destination.
///
/// Rules this type keeps:
/// * Local use needs nothing from here. A fresh install has routes with no permissions; the screen shows them as local
///   only and offers permissions without requiring any. A failed read leaves the screen explanatory, never blocking.
/// * A permission is granted only by `confirm`, and only for the proposal the user was shown. Opening a proposal,
///   cancelling it, a refresh, the listing of profiles or anything a classifier or provider produced never calls the
///   grant. A grant for one capability, route or profile version authorizes no other.
/// * Route labels, scopes and permissions are read from the core and shown as stored. This type never edits them
///   locally; they change only through a confirmed grant that the core accepted, then are read again.
/// * Stored state appears only while the read session is authenticated; a relock empties the screen and drops any open
///   proposal.
@MainActor
final class PrivacySettingsModel: ObservableObject {
    @Published private(set) var phase: PrivacySettingsPhase = .idle
    @Published private(set) var routes: [RoutePolicyRoute] = []
    @Published private(set) var listing: ProviderProfileListAnswer?
    @Published private(set) var proposal: PrivacyGrantProposal?
    @Published private(set) var actionState: PrivacySettingsActionState?
    @Published private(set) var change: PrivacyChangeProposal?
    @Published private(set) var requeueProposal: PrivacyRequeueProposal?
    /// Work waiting behind each unrevoked stored version, as the core reports it. Empty when nothing is queued.
    @Published private(set) var queuedWork: [PrivacyQueuedVersion] = []
    /// Whether queued work could not be read; the rest of the screen still works.
    @Published private(set) var queuedWorkUnreadable = false
    /// The link the core recorded for the most recent requeue, so the move can be inspected.
    @Published private(set) var lastRequeue: JobConfigurationRequeueRecord?
    /// Whether the record of the most recent requeue could not be read; `loadRequeueRecord` reads it again.
    @Published private(set) var requeueRecordUnreadable = false
    /// Work pending just before a profile revocation in this session and not listed afterwards, each job still open to
    /// an explicit move.
    @Published private(set) var unlistedWork: [PrivacyUnlistedVersion] = []
    /// The work that was pending when a route permission was last taken away, with what the core reports for each job
    /// now. Every refresh reads it again. It spans all routes, as the core's listing does, and claims nothing per route.
    @Published private(set) var followedWork: [PrivacyFollowedJob] = []
    /// Whether some work pending at that change could not be listed (a truncated or unreadable listing), so is not
    /// followed here. The core applies the same check to it.
    @Published private(set) var followedWorkIncomplete = false
    private var lastReplacementJobID: String?

    /// The capabilities the core lets a user authorize for sending content.
    static let grantableCapabilities = ["text_interpretation", "review"]

    /// How many queued jobs are listed per version; the core reports the full count either way.
    static let queuedJobLimit = 20

    /// The most jobs the core lists for one version in any read. A version with more pending work than this cannot be
    /// revoked from this screen, because the work beyond the listing could not be shown or moved afterwards.
    static let listedJobLimit = 100

    /// The most waiting notes one repin moves; the core stops at this many, so a full batch may leave more waiting.
    static let repinBatchLimit = 100

    private let controls: PrivacySettingsControlling
    private let readScope: () -> CompositionReadScope
    private var generation = 0

    nonisolated init(controls: PrivacySettingsControlling, readScope: @escaping () -> CompositionReadScope) {
        self.controls = controls
        self.readScope = readScope
    }

    var isWorking: Bool { actionState == .working }

    /// Whether any route may send anything off the device.
    var isLocalOnly: Bool { routes.allSatisfy { $0.grantedCapabilities.isEmpty } }

    /// Whether the route already holds a grant for `capability`.
    func isGranted(routeID: String, capability: String) -> Bool {
        routes.first { $0.routeID == routeID }?.grantedCapabilities.contains { $0.capability == capability } ?? false
    }

    /// Unrevoked stored versions that declare `capability` as supported or unverified, with their profile names, newest first.
    func offeredVersions(for capability: String) -> [PrivacyOfferedVersion] {
        guard let listing else { return [] }
        return listing.profiles.flatMap { profile in
            profile.versions.reversed().compactMap { version in
                guard !version.revoked,
                    version.capabilities.contains(where: { $0.capability == capability && $0.support != "unsupported" })
                else {
                    return nil
                }
                return PrivacyOfferedVersion(profileID: profile.profileID, version: version)
            }
        }
    }

    /// Labels of the routes that do not allow sorting text notes. Queued work for such a route is held, not sent.
    var routesHoldingWork: [String] {
        routes.filter { route in !route.grantedCapabilities.contains { $0.capability == "text_interpretation" } }
            .map(PrivacySettingsText.routeLabel)
    }

    /// Reads routes and profiles again.
    func refresh() async {
        guard readScope() == .authenticated else { lock(); return }
        generation += 1
        let thisGeneration = generation
        if phase != .loaded { phase = .loading }
        do {
            let freshRoutes = try await controls.listRoutePolicy()
            let freshListing = try await controls.listProfiles()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            let freshQueued = try await readQueuedWork(freshListing)
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            let freshFollowed = try await readFollowedWork()
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            routes = freshRoutes
            listing = freshListing
            queuedWork = freshQueued.versions
            queuedWorkUnreadable = freshQueued.unreadable
            followedWork = freshFollowed
            phase = .loaded
        } catch {
            guard thisGeneration == generation else { return }
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            guard readScope() == .authenticated else { lock(); return }
            phase = .failed(PrivacySettingsText.loadFailure(error))
        }
    }

    /// Reads again what became of each followed job: its own state from the core's full listing for its profile
    /// version, and, unless a request is in flight, its note's text sorting as a separate fact about the note.
    private func readFollowedWork() async throws -> [PrivacyFollowedJob] {
        // A version whose listing could not be read is left out, so its jobs read as unknown.
        var listings: [String: JobConfigurationPinnedJobs] = [:]
        for profileVersion in Set(followedWork.map(\.profileVersion)) {
            do {
                listings[profileVersion] = try await controls.jobsPinned(toProfile: profileVersion, limit: nil)
            } catch {
                if isNotAuthenticated(error) { throw ProviderConfigurationError.notAuthenticated }
            }
        }
        var followed: [PrivacyFollowedJob] = []
        for job in followedWork {
            let listing: PrivacyFollowedJob.Listing
            if let jobs = listings[job.profileVersion] {
                if let listed = jobs.jobs.first(where: { $0.jobID == job.jobID }) {
                    listing = .listed(listed.status)
                } else {
                    listing = jobs.truncated ? .unknown : .notListed
                }
            } else {
                listing = .unknown
            }
            let requestSent = job.requestSent || listing == .listed("running")
            var note: PrivacyFollowedJob.Note?
            if listing != .listed("running") {
                do {
                    let processing = try await controls.itemProcessing(itemID: job.itemID)
                    note = .reported(processing)
                } catch {
                    if isNotAuthenticated(error) { throw ProviderConfigurationError.notAuthenticated }
                    note = .unreadable
                }
            }
            followed.append(
                PrivacyFollowedJob(
                    jobID: job.jobID, itemID: job.itemID, profileVersion: job.profileVersion, listing: listing, note: note,
                    requestSent: requestSent))
        }
        return followed
    }

    private func readQueuedWork(_ freshListing: ProviderProfileListAnswer) async throws
        -> (versions: [PrivacyQueuedVersion], unreadable: Bool)
    {
        var versions: [PrivacyQueuedVersion] = []
        var unreadable = false
        for profile in freshListing.profiles {
            for version in profile.versions where !version.revoked {
                do {
                    let jobs = try await controls.jobsPinned(
                        toProfile: version.profileVersion, limit: Self.queuedJobLimit)
                    if jobs.total > 0 {
                        versions.append(PrivacyQueuedVersion(profileID: profile.profileID, version: version, jobs: jobs))
                    }
                } catch {
                    if case JobConfigurationError.notAuthenticated = error { throw ProviderConfigurationError.notAuthenticated }
                    unreadable = true
                }
            }
        }
        return (versions, unreadable)
    }

    /// Opens the question for one capability, route and profile version. Nothing is granted.
    func propose(routeID: String, capability: String, profileVersion: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking,
            Self.grantableCapabilities.contains(capability),
            let route = routes.first(where: { $0.routeID == routeID }),
            let found = offeredVersions(for: capability).first(where: { $0.version.profileVersion == profileVersion })
        else { return }
        actionState = nil
        change = nil
        requeueProposal = nil
        proposal = PrivacyGrantProposal(
            routeID: route.routeID, routeLabel: PrivacySettingsText.routeLabel(route), capability: capability,
            profileVersion: profileVersion,
            versionLabel: PrivacySettingsText.versionLabel(profileID: found.profileID, version: found.version))
    }

    /// Closes the open question. Nothing is granted and nothing else changes.
    func cancel() {
        guard proposal != nil || change != nil || requeueProposal != nil, !isWorking else { return }
        proposal = nil
        change = nil
        requeueProposal = nil
        actionState = .cancelled
    }

    /// Opens the question for taking `capability` away from the route. Nothing is changed.
    func proposeRevoke(routeID: String, capability: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking, isGranted(routeID: routeID, capability: capability),
            let route = routes.first(where: { $0.routeID == routeID })
        else { return }
        open(.revokeRouteCapability(
            routeID: routeID, routeLabel: PrivacySettingsText.routeLabel(route), capability: capability))
    }

    /// Opens the question for revoking one unrevoked stored profile version. Nothing is changed.
    func proposeRevokeProfile(profileVersion: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking, let listing else { return }
        for profile in listing.profiles {
            if let version = profile.versions.first(where: { $0.profileVersion == profileVersion && !$0.revoked }) {
                open(.revokeProfileVersion(
                    profileVersion: profileVersion,
                    versionLabel: PrivacySettingsText.versionLabel(profileID: profile.profileID, version: version)))
                return
            }
        }
    }

    /// Opens the question for queueing the route's waiting notes with the provider selected now. Only offered where the
    /// route already allows sorting text notes; the core still decides whether the selected provider is authorized.
    func proposeRepinWaitingWork(routeID: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking, isGranted(routeID: routeID, capability: "text_interpretation"),
            let route = routes.first(where: { $0.routeID == routeID })
        else { return }
        open(.repinWaitingWork(routeID: routeID, routeLabel: PrivacySettingsText.routeLabel(route)))
    }

    private func open(_ target: PrivacyChangeProposal.Target) {
        actionState = nil
        proposal = nil
        requeueProposal = nil
        change = PrivacyChangeProposal(target: target)
    }

    /// Applies exactly the shown change. `shown` must be the change on screen, so a stale confirmation does nothing.
    /// The screen then reads the stored state again and shows that, not the expectation.
    func confirmChange(_ shown: PrivacyChangeProposal) async {
        guard readScope() == .authenticated else { lock(); return }
        guard let current = change, current == shown, !isWorking else { return }
        actionState = .working
        var routeRevoke: (outcome: RoutePolicyChangeOutcome.Disposition, routeID: String, routeLabel: String, capability: String)?
        do {
            switch current.target {
            case .revokeRouteCapability(let routeID, let routeLabel, let capability):
                // Read the pending work just before the change, so what became of each job, including a request
                // already sent, can be read back afterwards. A failed read does not hold back taking permission away.
                var pending: (versions: [PrivacyQueuedVersion], unreadable: Bool) = ([], true)
                if let listing { pending = try await readQueuedWork(listing) }
                let outcome = try await controls.revoke(routeID: routeID, capability: capability)
                routeRevoke = (outcome, routeID, routeLabel, capability)
                if outcome == .revoked {
                    followedWork = pending.versions.flatMap { version in
                        version.jobs.jobs.map {
                            PrivacyFollowedJob(
                                jobID: $0.jobID, itemID: $0.itemID, profileVersion: version.version.profileVersion,
                                listing: .listed($0.status), note: nil, requestSent: $0.status == "running")
                        }
                    }
                    followedWorkIncomplete = pending.unreadable || pending.versions.contains { $0.jobs.truncated }
                }
                actionState = .changed(PrivacySettingsText.revokeResult(outcome, routeLabel: routeLabel, capability: capability))
            case .revokeProfileVersion(let profileVersion, let versionLabel):
                // Read the work pinned to it before and after, so the message and the retry list come from the core.
                // The read before is required and must be complete: the core lists only queued and running jobs, so
                // a job it retires here can be offered for a move afterwards only if it was listed now. A failed or
                // capped read stops here with nothing revoked.
                let before: JobConfigurationPinnedJobs
                do {
                    before = try await controls.jobsPinned(toProfile: profileVersion, limit: nil)
                } catch {
                    if isNotAuthenticated(error) { lock(); return }
                    change = nil
                    actionState = .failed(PrivacySettingsText.profileRevokeNotStarted(versionLabel))
                    return
                }
                if before.truncated {
                    change = nil
                    actionState = .failed(PrivacySettingsText.profileRevokeNotListed(versionLabel, pending: before.total))
                    return
                }
                try await controls.revokeProfile(profileVersion)
                let after = try? await controls.jobsPinned(toProfile: profileVersion, limit: nil)
                actionState = .changed(recordUnlisted(profileVersion, versionLabel: versionLabel, before: before, after: after))
            case .repinWaitingWork(let routeID, let routeLabel):
                let moved = try await controls.repinWaitingWork(routeID: routeID)
                actionState = .changed(
                    PrivacySettingsText.repinResult(
                        moved: moved, routeLabel: routeLabel, moreMayWait: moved >= Self.repinBatchLimit))
            }
            change = nil
        } catch {
            if isNotAuthenticated(error) { lock(); return }
            change = nil
            actionState = .failed(PrivacySettingsText.changeFailure(error))
        }
        await refresh()
        if let revoked = routeRevoke, case .changed = actionState {
            actionState = .changed(
                PrivacySettingsText.revokeResult(
                    revoked.outcome, routeLabel: revoked.routeLabel, capability: revoked.capability,
                    stillAllowed: phase == .loaded
                        ? isGranted(routeID: revoked.routeID, capability: revoked.capability) : nil,
                    followed: followedWork.count))
        }
    }

    /// Records the jobs listed just before a profile revocation and not afterwards, and describes what the core now
    /// reports for the version. A job missing from the read after was retired by the revocation unless it ended in the
    /// moment between the two reads; the listing does not say which, so neither does the screen.
    private func recordUnlisted(
        _ profileVersion: String, versionLabel: String, before: JobConfigurationPinnedJobs,
        after: JobConfigurationPinnedJobs?
    ) -> String {
        // Without the read after, every job seen before is kept open to a move; the core accepts a move only for work
        // it still holds or retired for the revocation, and refuses the rest.
        let stillListed = Set(after?.jobs.map(\.jobID) ?? [])
        let unlisted = before.jobs.filter { !stillListed.contains($0.jobID) }
            .map { PrivacyUnlistedJob(jobID: $0.jobID, itemID: $0.itemID, requestSent: $0.status == "running") }
        unlistedWork.removeAll { $0.profileVersion == profileVersion }
        if !unlisted.isEmpty {
            unlistedWork.append(
                PrivacyUnlistedVersion(profileVersion: profileVersion, versionLabel: versionLabel, jobs: unlisted))
        }
        guard let after else { return PrivacySettingsText.profileRevokedUnread(versionLabel, pendingBefore: before.total) }
        return PrivacySettingsText.profileRevoked(
            versionLabel, unlisted: unlisted.count, alreadySent: unlisted.filter(\.requestSent).count,
            stillPending: after.total)
    }

    /// Opens the question for moving one queued job to another unrevoked stored version. Nothing is moved.
    func proposeRequeue(jobID: String, toProfileVersion: String) {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking, let listing,
            let from = pinnedOrigin(of: jobID),
            from.profileVersion != toProfileVersion
        else { return }
        for profile in listing.profiles {
            if let target = profile.versions.first(where: { $0.profileVersion == toProfileVersion && !$0.revoked }) {
                actionState = nil
                proposal = nil
                change = nil
                requeueProposal = PrivacyRequeueProposal(
                    jobID: jobID, newJobID: UUID().uuidString.lowercased(),
                    fromVersionLabel: from.label,
                    toProfileVersion: toProfileVersion,
                    toVersionLabel: PrivacySettingsText.versionLabel(profileID: profile.profileID, version: target))
                return
            }
        }
    }

    /// The stored version a queued or running job, or one unlisted since a revocation, was pinned to, with its label.
    private func pinnedOrigin(of jobID: String) -> (profileVersion: String, label: String)? {
        if let queued = queuedWork.first(where: { $0.jobs.jobs.contains { $0.jobID == jobID } }) {
            return (
                queued.version.profileVersion,
                PrivacySettingsText.versionLabel(profileID: queued.profileID, version: queued.version)
            )
        }
        if let unlisted = unlistedWork.first(where: { $0.jobs.contains { $0.jobID == jobID } }) {
            return (unlisted.profileVersion, unlisted.versionLabel)
        }
        return nil
    }

    /// Moves exactly the shown job. The proposal's `newJobID` is reused when an unclear result is retried, so a repeat
    /// answers the same replacement instead of queueing another.
    func confirmRequeue(_ shown: PrivacyRequeueProposal) async {
        guard readScope() == .authenticated else { lock(); return }
        guard let current = requeueProposal, current == shown, !isWorking else { return }
        actionState = .working
        do {
            let replacement = try await controls.requeue(
                jobID: current.jobID, newJobID: current.newJobID, toProfileVersion: current.toProfileVersion)
            requeueProposal = nil
            // The core accepted the move, so it held or had retired this job: it leaves the unlisted work.
            unlistedWork = unlistedWork.compactMap { unlisted in
                let remaining = unlisted.jobs.filter { $0.jobID != current.jobID }
                return remaining.isEmpty
                    ? nil
                    : PrivacyUnlistedVersion(
                        profileVersion: unlisted.profileVersion, versionLabel: unlisted.versionLabel, jobs: remaining)
            }
            lastReplacementJobID = replacement.jobID
            lastRequeue = nil
            requeueRecordUnreadable = false
            await loadRequeueRecord()
            guard readScope() == .authenticated else { lock(); return }
            actionState = .changed(
                PrivacySettingsText.requeued(to: current.toVersionLabel, recorded: lastRequeue != nil))
        } catch {
            if isNotAuthenticated(error) { lock(); return }
            // Only an answer that never arrived or was unclear can be repeated; a refusal ends the question.
            let retryable: Bool
            switch error {
            case JobConfigurationError.unreadable, JobConfigurationError.core: retryable = true
            default: retryable = false
            }
            if !retryable { requeueProposal = nil }
            actionState = .failed(PrivacySettingsText.changeFailure(error))
        }
        await refresh()
    }

    /// Reads the core's record of the most recent requeue. A failed or empty read is shown as unreadable, never as done.
    func loadRequeueRecord() async {
        guard readScope() == .authenticated else { lock(); return }
        guard let replacementJobID = lastReplacementJobID else { return }
        do {
            let record = try await controls.requeueRecord(forJob: replacementJobID)
            guard readScope() == .authenticated else { lock(); return }
            lastRequeue = record
            requeueRecordUnreadable = record == nil
        } catch {
            if isNotAuthenticated(error) { lock(); return }
            lastRequeue = nil
            requeueRecordUnreadable = true
        }
    }

    private func isNotAuthenticated(_ error: Error) -> Bool {
        if case ProviderConfigurationError.notAuthenticated = error { return true }
        if case JobConfigurationError.notAuthenticated = error { return true }
        return false
    }

    /// Grants exactly the shown proposal. `shown` must be the proposal on screen, so a stale confirmation of an earlier
    /// question grants nothing.
    func confirm(_ shown: PrivacyGrantProposal) async {
        guard readScope() == .authenticated else { lock(); return }
        guard let current = proposal, current == shown, !isWorking else { return }
        actionState = .working
        do {
            let outcome = try await controls.grant(
                routeID: current.routeID, capability: current.capability, profileVersion: current.profileVersion)
            proposal = nil
            actionState = .allowed(PrivacySettingsText.result(outcome, current))
        } catch {
            if case ProviderConfigurationError.notAuthenticated = error { lock(); return }
            proposal = nil
            actionState = .failed(PrivacySettingsText.failure(error))
        }
        await refresh()
    }

    /// Drops everything shown, as when the screen goes away.
    func clear() {
        generation += 1
        proposal = nil
        change = nil
        requeueProposal = nil
        lastRequeue = nil
        requeueRecordUnreadable = false
        lastReplacementJobID = nil
        unlistedWork = []
        followedWork = []
        followedWorkIncomplete = false
        queuedWork = []
        queuedWorkUnreadable = false
        actionState = nil
        routes = []
        listing = nil
        if phase == .loaded || phase == .loading { phase = .idle }
    }

    private func lock() {
        generation += 1
        proposal = nil
        change = nil
        requeueProposal = nil
        lastRequeue = nil
        requeueRecordUnreadable = false
        lastReplacementJobID = nil
        unlistedWork = []
        followedWork = []
        followedWorkIncomplete = false
        queuedWork = []
        queuedWorkUnreadable = false
        actionState = nil
        routes = []
        listing = nil
        phase = .locked
    }
}
