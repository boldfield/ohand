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
    /// The core's durable listing of the jobs a revocation of `profileVersion` retired, one page at a time.
    func jobsRetired(byRevocationOf profileVersion: String, limit: Int?, after: String?) async throws
        -> JobConfigurationRetiredJobs
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

    func jobsRetired(byRevocationOf profileVersion: String, limit: Int?, after: String?) async throws
        -> JobConfigurationRetiredJobs
    {
        try await jobs.jobsRetired(byRevocationOf: profileVersion, limit: limit, after: after)
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

/// One job the core retired when a stored profile version was revoked, read from the core's durable listing of what
/// that revocation retired. The core keeps that list, so it is the same after a relaunch, and a job on it can be moved
/// to another version on purpose until it has been. `preview` adds only what this screen itself listed for the job
/// just before the revocation; it never decides whether the job is on the list.
struct PrivacyRetiredJob: Equatable {
    enum PriorListing: Equatable {
        /// Listed `running` just before the revocation: its request had already been sent.
        case running
        /// Listed `queued` just before the revocation. That says nothing about whether an earlier attempt was sent.
        case queued
        /// Absent from the screen's complete listing just before the revocation. The core retires only queued and
        /// running work, so the job was pinned to the version in the moment between that listing and the revocation.
        case notListed
        /// The screen has no complete listing from just before the revocation: it failed, was capped, or the
        /// revocation happened before this launch.
        case unknown
    }

    let jobID: String
    let itemID: String
    /// RFC 3339: when the version was revoked, as the core records it.
    let retiredAt: String
    /// The replacement job the core queued when this job was moved, once it was.
    let requeuedTo: String?
    let preview: PriorListing

    /// Whether the job can still be offered a move: the core has not recorded one for it yet.
    var isMovable: Bool { requeuedTo == nil }
}

/// The work a revocation of one stored profile version retired, as the core lists it: the pages read so far, the full
/// count, and the cursor of the next page while there is one.
struct PrivacyRetiredVersion: Equatable {
    let profileVersion: String
    let versionLabel: String
    let jobs: [PrivacyRetiredJob]
    let total: Int
    let nextCursor: String?

    /// How many retired jobs the core holds beyond the pages read so far.
    var remaining: Int { max(total - jobs.count, 0) }
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
    /// The work each revoked stored version's revocation retired, as the core's durable listing reports it. Every
    /// refresh reads it again, so it is the same after a relaunch; each job not yet moved is open to an explicit move.
    @Published private(set) var retiredWork: [PrivacyRetiredVersion] = []
    /// Whether some retired work could not be read; a refresh reads it again. The core keeps the list either way.
    @Published private(set) var retiredWorkUnreadable = false
    /// The revoked versions whose next page of retired work is being read now. One page is read at a time per
    /// version, so a job is listed once however many times "Show more" is asked for while a read is under way.
    @Published private(set) var retiredPageLoads: Set<String> = []
    /// The work that was pending when a route permission was last taken away, with what the core reports for each job
    /// now. Every refresh reads it again. It spans all routes, as the core's listing does, and claims nothing per route.
    @Published private(set) var followedWork: [PrivacyFollowedJob] = []
    /// Whether some work pending at that change could not be listed (a truncated or unreadable listing), so is not
    /// followed here. The core applies the same check to it.
    @Published private(set) var followedWorkIncomplete = false
    private var lastReplacementJobID: String?
    /// What this screen listed for each job just before a profile revocation in this session, by job ID.
    private var retiredPreviews: [String: PrivacyRetiredJob.PriorListing] = [:]
    /// The versions whose listing just before their revocation in this session was read in full, so a retired job
    /// absent from it is known to have been pinned in the moment before the revocation.
    private var completePreviews: Set<String> = []

    /// The capabilities the core lets a user authorize for sending content.
    static let grantableCapabilities = ["text_interpretation", "review"]

    /// How many queued or retired jobs are listed per version and per page; the core reports the full count either way.
    static let queuedJobLimit = 20

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
        retiredPageLoads = []
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
            let freshRetired = try await readRetiredWork(freshListing)
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            routes = freshRoutes
            listing = freshListing
            queuedWork = freshQueued.versions
            queuedWorkUnreadable = freshQueued.unreadable
            followedWork = freshFollowed
            retiredWork = freshRetired.versions
            retiredWorkUnreadable = freshRetired.unreadable
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

    /// Reads, for every revoked stored version, the core's durable listing of what its revocation retired: at least as
    /// many pages as were on screen before, so a refresh never hides a job the user had reached.
    private func readRetiredWork(_ freshListing: ProviderProfileListAnswer) async throws
        -> (versions: [PrivacyRetiredVersion], unreadable: Bool)
    {
        var versions: [PrivacyRetiredVersion] = []
        var unreadable = false
        for profile in freshListing.profiles {
            for version in profile.versions where version.revoked {
                let shown = retiredWork.first { $0.profileVersion == version.profileVersion }?.jobs.count ?? 0
                do {
                    let read = try await readRetiredPages(
                        version.profileVersion,
                        versionLabel: PrivacySettingsText.versionLabel(profileID: profile.profileID, version: version),
                        atLeast: shown)
                    if read.total > 0 { versions.append(read) }
                } catch {
                    if isNotAuthenticated(error) { throw ProviderConfigurationError.notAuthenticated }
                    unreadable = true
                }
            }
        }
        return (versions, unreadable)
    }

    /// Reads the first page of the jobs a revocation retired, then further pages until at least `count` are read or
    /// the list ends.
    private func readRetiredPages(_ profileVersion: String, versionLabel: String, atLeast count: Int) async throws
        -> PrivacyRetiredVersion
    {
        var page = try await controls.jobsRetired(byRevocationOf: profileVersion, limit: Self.queuedJobLimit, after: nil)
        var jobs = page.jobs.map { retiredJob($0, profileVersion: profileVersion) }
        while let cursor = page.nextCursor, jobs.count < count {
            page = try await controls.jobsRetired(byRevocationOf: profileVersion, limit: Self.queuedJobLimit, after: cursor)
            jobs += page.jobs.map { retiredJob($0, profileVersion: profileVersion) }
        }
        return PrivacyRetiredVersion(
            profileVersion: profileVersion, versionLabel: versionLabel, jobs: jobs, total: page.total,
            nextCursor: page.nextCursor)
    }

    /// One retired job as the core lists it, with what this screen listed for it just before the revocation, if that
    /// listing was read.
    private func retiredJob(_ job: JobConfigurationRetiredJob, profileVersion: String) -> PrivacyRetiredJob {
        let preview = retiredPreviews[job.jobID] ?? (completePreviews.contains(profileVersion) ? .notListed : .unknown)
        return PrivacyRetiredJob(
            jobID: job.jobID, itemID: job.itemID, retiredAt: job.retiredAt, requeuedTo: job.requeuedTo, preview: preview)
    }

    /// Whether the next page of the version's retired work is being read; the "Show more" row waits for it.
    func isLoadingRetiredWork(profileVersion: String) -> Bool { retiredPageLoads.contains(profileVersion) }

    /// Reads the next page of the jobs a revocation retired and adds it to what is shown. The core reports the full
    /// count either way; a failed read leaves the pages already shown and says the rest could not be read. While
    /// one page is being read for a version, another ask for the same version reads nothing: the page is added once.
    func loadMoreRetiredWork(profileVersion: String) async {
        guard readScope() == .authenticated else { lock(); return }
        guard phase == .loaded, !isWorking, !retiredPageLoads.contains(profileVersion),
            let cursor = retiredWork.first(where: { $0.profileVersion == profileVersion })?.nextCursor
        else { return }
        let thisGeneration = generation
        retiredPageLoads.insert(profileVersion)
        // Whatever bumped the generation meanwhile cleared the set itself, and a later read may own the entry now.
        defer { if thisGeneration == generation { retiredPageLoads.remove(profileVersion) } }
        do {
            let page = try await controls.jobsRetired(
                byRevocationOf: profileVersion, limit: Self.queuedJobLimit, after: cursor)
            guard thisGeneration == generation else { return }
            guard readScope() == .authenticated else { lock(); return }
            // The page follows the cursor it was read from. If the shown list no longer ends at that cursor, the page
            // belongs elsewhere and is dropped, so no job is listed twice.
            guard let index = retiredWork.firstIndex(where: { $0.profileVersion == profileVersion }),
                retiredWork[index].nextCursor == cursor
            else { return }
            let shown = retiredWork[index]
            retiredWork[index] = PrivacyRetiredVersion(
                profileVersion: shown.profileVersion, versionLabel: shown.versionLabel,
                jobs: shown.jobs + page.jobs.map { retiredJob($0, profileVersion: profileVersion) },
                total: page.total, nextCursor: page.nextCursor)
        } catch {
            guard thisGeneration == generation else { return }
            if isNotAuthenticated(error) { lock(); return }
            retiredWorkUnreadable = true
        }
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
                // A preview of the work pinned to it just before, so the result can say which retired job's request
                // had already been sent. It is only a preview: the result itself is the core's durable listing of
                // what the revocation retired, which includes work pinned in the moment after the preview that no
                // preview can see. A failed or capped preview holds nothing back; it only leaves that detail unknown.
                var preview: JobConfigurationPinnedJobs?
                do {
                    preview = try await controls.jobsPinned(toProfile: profileVersion, limit: nil)
                } catch {
                    if isNotAuthenticated(error) { lock(); return }
                }
                try await controls.revokeProfile(profileVersion)
                recordPreview(preview, profileVersion: profileVersion)
                actionState = .changed(await readRetiredResult(profileVersion, versionLabel: versionLabel))
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

    /// Keeps what the screen listed just before a profile revocation, per job, for the retired rows to show.
    private func recordPreview(_ preview: JobConfigurationPinnedJobs?, profileVersion: String) {
        completePreviews.remove(profileVersion)
        guard let preview else { return }
        for job in preview.jobs { retiredPreviews[job.jobID] = job.status == "running" ? .running : .queued }
        if !preview.truncated { completePreviews.insert(profileVersion) }
    }

    /// Reads the core's durable listing of what the revocation retired and describes it. The listing, not any
    /// expectation, is the result: it has every job the revocation retired, and only those. A failed read says so;
    /// the core keeps the list, and the next refresh reads it again.
    private func readRetiredResult(_ profileVersion: String, versionLabel: String) async -> String {
        do {
            let retired = try await readRetiredPages(profileVersion, versionLabel: versionLabel, atLeast: 0)
            retiredWork.removeAll { $0.profileVersion == profileVersion }
            if retired.total > 0 { retiredWork.append(retired) }
            return PrivacySettingsText.profileRevoked(
                versionLabel, retired: retired.total, alreadySent: retired.jobs.filter { $0.preview == .running }.count,
                listed: retired.jobs.count)
        } catch {
            retiredWorkUnreadable = true
            return PrivacySettingsText.profileRevokedUnread(versionLabel)
        }
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

    /// The stored version a queued or running job, or one a revocation retired and the core has not yet seen moved,
    /// was pinned to, with its label.
    private func pinnedOrigin(of jobID: String) -> (profileVersion: String, label: String)? {
        if let queued = queuedWork.first(where: { $0.jobs.jobs.contains { $0.jobID == jobID } }) {
            return (
                queued.version.profileVersion,
                PrivacySettingsText.versionLabel(profileID: queued.profileID, version: queued.version)
            )
        }
        if let retired = retiredWork.first(where: { $0.jobs.contains { $0.jobID == jobID && $0.isMovable } }) {
            return (retired.profileVersion, retired.versionLabel)
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
            // The core accepted the move; the refresh below reads its link back, so a retired job shows where it went.
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
        retiredWork = []
        retiredWorkUnreadable = false
        retiredPageLoads = []
        retiredPreviews = [:]
        completePreviews = []
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
        retiredWork = []
        retiredWorkUnreadable = false
        retiredPageLoads = []
        retiredPreviews = [:]
        completePreviews = []
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
