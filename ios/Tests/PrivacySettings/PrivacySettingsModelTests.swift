import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

func privacyRoute(id: String, name: String, scope: String, grants: [(String, [String])] = []) -> RoutePolicyRoute {
    let granted = grants.map { capability, classes in
        let list = classes.map { "\"\($0)\"" }.joined(separator: ",")
        return #"{"capability":"\#(capability)","destination_classes":[\#(list)]}"#
    }.joined(separator: ",")
    let json = #"{"route_id":"\#(id)","route_name":"\#(name)","scope":"\#(scope)","granted_capabilities":[\#(granted)]}"#
    return try! JSONDecoder().decode(RoutePolicyRoute.self, from: Data(json.utf8))
}

final class FakePrivacySettingsControls: PrivacySettingsControlling, @unchecked Sendable {
    var routes: Result<[RoutePolicyRoute], Error>
    var listing: Result<ProviderProfileListAnswer, Error>
    var grantResult: Result<RoutePolicyChangeOutcome.Disposition, Error> = .success(.granted)
    private(set) var grants: [[String]] = []
    /// Invoked in the middle of every grant, so a test can change the session while it is in flight.
    var duringGrant: (() -> Void)?

    init(routes: [RoutePolicyRoute], listing: ProviderProfileListAnswer) {
        self.routes = .success(routes)
        self.listing = .success(listing)
    }

    var revokeResult: Result<RoutePolicyChangeOutcome.Disposition, Error> = .success(.revoked)
    var revokeProfileResult: Result<Void, Error> = .success(())
    var pinned: [String: JobConfigurationPinnedJobs] = [:]
    var pinnedFailure: Error?
    var requeueResult: Result<JobConfigurationRequeuedJob, Error> = .success(
        JobConfigurationRequeuedJob(jobID: "job-new", itemID: "item-1", status: "queued", profileVersion: "private-v1"))
    var recordResult: Result<JobConfigurationRequeueRecord?, Error> = .success(nil)
    var repinResult: Result<Int, Error> = .success(0)
    private(set) var revocations: [[String]] = []
    private(set) var profileRevocations: [String] = []
    private(set) var requeues: [[String]] = []
    private(set) var repins: [String] = []
    /// Invoked in the middle of every revocation, so a test can change stored state while it is in flight.
    var duringRevoke: (() -> Void)?
    /// Invoked in the middle of every profile revocation, so a test can change the pinned work as the core does.
    var duringRevokeProfile: (() -> Void)?

    func revoke(routeID: String, capability: String) async throws -> RoutePolicyChangeOutcome.Disposition {
        revocations.append([routeID, capability])
        duringRevoke?()
        return try revokeResult.get()
    }

    func revokeProfile(_ profileVersion: String) async throws {
        profileRevocations.append(profileVersion)
        duringRevokeProfile?()
        try revokeProfileResult.get()
    }

    func jobsPinned(toProfile profileVersion: String, limit: Int?) async throws -> JobConfigurationPinnedJobs {
        if let pinnedFailure { throw pinnedFailure }
        return pinned[profileVersion] ?? JobConfigurationPinnedJobs(jobs: [], total: 0, truncated: false)
    }

    /// The core's durable listing of what each version's revocation retired. Pages are cut from it the way the core
    /// cuts them: in job ID order, `limit` at a time (at most 100), from after the cursor.
    var retired: [String: [JobConfigurationRetiredJob]] = [:]
    var retiredFailure: Error?
    private(set) var retiredReads: [[String?]] = []
    /// Awaited in the middle of every retired-work read, with the cursor it reads from, so a test can hold a page
    /// read open while the screen is asked for more.
    var duringRetiredRead: ((String?) async -> Void)?

    func jobsRetired(byRevocationOf profileVersion: String, limit: Int?, after: String?) async throws
        -> JobConfigurationRetiredJobs
    {
        retiredReads.append([profileVersion, limit.map { String($0) }, after])
        await duringRetiredRead?(after)
        if let retiredFailure { throw retiredFailure }
        let all = (retired[profileVersion] ?? []).sorted { $0.jobID < $1.jobID }
        let pageSize = min(limit ?? 100, 100)
        let rest = all.filter { job in after.map { job.jobID > $0 } ?? true }
        let page = Array(rest.prefix(pageSize))
        let next = rest.count > pageSize ? page.last?.jobID : nil
        return JobConfigurationRetiredJobs(jobs: page, total: all.count, truncated: next != nil, nextCursor: next)
    }

    func requeue(jobID: String, newJobID: String, toProfileVersion: String) async throws
        -> JobConfigurationRequeuedJob
    {
        requeues.append([jobID, newJobID, toProfileVersion])
        return try requeueResult.get()
    }

    func requeueRecord(forJob jobID: String) async throws -> JobConfigurationRequeueRecord? { try recordResult.get() }

    func repinWaitingWork(routeID: String) async throws -> Int {
        repins.append(routeID)
        return try repinResult.get()
    }

    /// The core's processing facts per item; an item not listed reads as unprocessed with no stopped job.
    var processing: [String: Result<PrivacyItemProcessing, Error>] = [:]
    private(set) var processingReads: [String] = []

    func itemProcessing(itemID: String) async throws -> PrivacyItemProcessing {
        processingReads.append(itemID)
        return try (processing[itemID] ?? .success(PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: nil))).get()
    }

    func listRoutePolicy() async throws -> [RoutePolicyRoute] { try routes.get() }
    func listProfiles() async throws -> ProviderProfileListAnswer { try listing.get() }

    func grant(routeID: String, capability: String, profileVersion: String) async throws
        -> RoutePolicyChangeOutcome.Disposition
    {
        grants.append([routeID, capability, profileVersion])
        duringGrant?()
        return try grantResult.get()
    }
}

/// Holds a fake read open until the test lets it go; every waiter is released at once, and later waiters pass through.
final class PrivacyReadHold: @unchecked Sendable {
    private let lock = NSLock()
    private var released = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { continuation in
            lock.lock()
            if released {
                lock.unlock()
                continuation.resume()
            } else {
                waiters.append(continuation)
                lock.unlock()
            }
        }
    }

    func release() {
        lock.lock()
        released = true
        let waiting = waiters
        waiters = []
        lock.unlock()
        for waiter in waiting { waiter.resume() }
    }
}

/// One job as the core lists it among those a revocation retired.
func retiredJob(_ jobID: String, item: String = "item-1", requeuedTo: String? = nil) -> JobConfigurationRetiredJob {
    JobConfigurationRetiredJob(jobID: jobID, itemID: item, retiredAt: "2026-10-02T00:00:00Z", requeuedTo: requeuedTo)
}

/// The default listing as the core reports it once `hosted-v1` is revoked.
func listingWithHostedRevoked() -> ProviderProfileListAnswer {
    providerListing(
        versions: [
            (profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: true, supported: true),
            (profile: "private", version: "private-v1", providerProtocol: "self_hosted", revoked: false, supported: true),
            (profile: "old", version: "old-v1", providerProtocol: "open_ai", revoked: true, supported: true),
        ], defaultVersion: nil)
}

@MainActor
final class PrivacySettingsModelTests: XCTestCase {
    private var scope: CompositionReadScope = .authenticated

    private func makeModel(
        routes: [RoutePolicyRoute]? = nil, profiles: Bool = true
    ) -> (PrivacySettingsModel, FakePrivacySettingsControls) {
        let versions = profiles
            ? [(profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: false, supported: true),
               (profile: "private", version: "private-v1", providerProtocol: "self_hosted", revoked: false, supported: true),
               (profile: "old", version: "old-v1", providerProtocol: "open_ai", revoked: true, supported: true)]
            : []
        let controls = FakePrivacySettingsControls(
            routes: routes ?? [
                privacyRoute(id: "route-personal", name: "Local capture", scope: "personal"),
                privacyRoute(id: "route-work", name: "Work notes", scope: "work"),
            ],
            listing: providerListing(versions: versions, defaultVersion: nil))
        let model = PrivacySettingsModel(controls: controls, readScope: { [unowned self] in self.scope })
        return (model, controls)
    }

    func testFreshInstallIsLocalOnlyAndUsableWithoutAnyProfile() async {
        let (model, controls) = makeModel(
            routes: [privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")], profiles: false)
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly)
        XCTAssertTrue(model.offeredVersions(for: "text_interpretation").isEmpty)
        XCTAssertEqual(PrivacySettingsText.routeLabel(model.routes[0]), "Personal route · Local capture")
        XCTAssertTrue(controls.grants.isEmpty, "loading the screen authorizes nothing")
    }

    func testAReadFailureLeavesTheScreenExplanatoryAndGrantsNothing() async {
        let (model, controls) = makeModel()
        controls.routes = .failure(ProviderConfigurationError.unreadable)
        await model.refresh()
        guard case .failed(let message) = model.phase else { return XCTFail("expected failed") }
        XCTAssertTrue(message.contains("Capture and search still work"))
        XCTAssertTrue(controls.grants.isEmpty)
    }

    func testOpeningAProposalGrantsNothingAndCancellingLeavesLocalUseIntact() async {
        let (model, controls) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        XCTAssertNotNil(model.proposal)
        XCTAssertTrue(controls.grants.isEmpty)
        model.cancel()
        XCTAssertNil(model.proposal)
        XCTAssertEqual(model.actionState, .cancelled)
        XCTAssertTrue(controls.grants.isEmpty)
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly)
    }

    func testConfirmingGrantsExactlyTheShownCapabilityRouteAndVersion() async throws {
        let (model, controls) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        let shown = try XCTUnwrap(model.proposal)
        await model.confirm(shown)
        XCTAssertEqual(controls.grants, [["route-personal", "text_interpretation", "hosted-v1"]])
        XCTAssertNil(model.proposal)
        guard case .allowed = model.actionState else { return XCTFail("expected allowed") }
    }

    func testAcceptingOneCapabilityOrDestinationAuthorizesNoOther() async throws {
        let (model, controls) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        let first = try XCTUnwrap(model.proposal)
        await model.confirm(first)
        // Only the confirmed triple reached the grant; the other route, version and capability were never touched.
        XCTAssertEqual(controls.grants.count, 1)
        XCTAssertEqual(controls.grants[0], ["route-personal", "text_interpretation", "hosted-v1"])
        // A second question needs its own confirmation.
        model.propose(routeID: "route-work", capability: "text_interpretation", profileVersion: "hosted-v1")
        XCTAssertNotNil(model.proposal)
        XCTAssertEqual(controls.grants.count, 1)
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "private-v1")
        XCTAssertEqual(model.proposal?.profileVersion, "private-v1")
        XCTAssertEqual(controls.grants.count, 1)
    }

    func testAStaleConfirmationOfAnEarlierQuestionGrantsNothing() async throws {
        let (model, controls) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        let earlier = try XCTUnwrap(model.proposal)
        model.propose(routeID: "route-work", capability: "text_interpretation", profileVersion: "private-v1")
        await model.confirm(earlier)
        XCTAssertTrue(controls.grants.isEmpty)
        model.cancel()
        await model.confirm(earlier)
        XCTAssertTrue(controls.grants.isEmpty, "a cancelled question cannot be confirmed afterwards")
    }

    func testRevokedOrUnknownChoicesAreNotProposed() async {
        let (model, _) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "old-v1")
        XCTAssertNil(model.proposal, "a revoked version is not offered")
        model.propose(routeID: "route-missing", capability: "text_interpretation", profileVersion: "hosted-v1")
        XCTAssertNil(model.proposal)
        model.propose(routeID: "route-personal", capability: "transcription", profileVersion: "hosted-v1")
        XCTAssertNil(model.proposal, "an on-device capability is never granted")
        model.propose(routeID: "route-personal", capability: "review", profileVersion: "hosted-v1")
        XCTAssertNil(model.proposal, "a version that does not declare the capability is not offered")
    }

    func testAFailedGrantIsReportedAndKeepsLocalStateUnchanged() async throws {
        let (model, controls) = makeModel()
        controls.grantResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "profile_revoked", message: "synthetic")))
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        await model.confirm(try XCTUnwrap(model.proposal))
        XCTAssertEqual(model.actionState, .failed("That profile version was revoked. Nothing was changed."))
        XCTAssertTrue(model.isLocalOnly)
        XCTAssertNil(model.proposal)
    }

    func testRelockingDropsTheOpenQuestionAndTheStoredState() async throws {
        let (model, controls) = makeModel()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        let shown = try XCTUnwrap(model.proposal)
        scope = .captureOnly
        await model.confirm(shown)
        XCTAssertTrue(controls.grants.isEmpty, "a capture-only session grants nothing")
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.proposal)
        XCTAssertTrue(model.routes.isEmpty)
    }

    func testRoutePermissionsAreShownAsStoredAndNeverInferred() async {
        let granted = privacyRoute(
            id: "route-personal", name: "Local capture", scope: "personal",
            grants: [("text_interpretation", ["private_server"])])
        let (model, controls) = makeModel(routes: [granted, privacyRoute(id: "route-work", name: "Work notes", scope: "work")])
        await model.refresh()
        XCTAssertFalse(model.isLocalOnly)
        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-work", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "review"))
        XCTAssertEqual(
            PrivacySettingsText.grantSummary(granted.grantedCapabilities[0]),
            "Allowed: Sorting your text notes via your private server")
        await model.refresh()
        XCTAssertEqual(model.routes, [granted, privacyRoute(id: "route-work", name: "Work notes", scope: "work")])
        XCTAssertTrue(controls.grants.isEmpty, "reading and re-reading never writes a permission")
    }

    // MARK: Revocation and queued work

    private func grantedRoute() -> RoutePolicyRoute {
        privacyRoute(
            id: "route-personal", name: "Local capture", scope: "personal",
            grants: [("text_interpretation", ["cloud"])])
    }

    private func pinnedJobs(_ ids: [String]) -> JobConfigurationPinnedJobs {
        JobConfigurationPinnedJobs(
            jobs: ids.map { JobConfigurationPinnedJob(jobID: $0, itemID: "item-\($0)", status: "queued") },
            total: ids.count, truncated: false)
    }

    func testOpeningOrCancellingARevocationChangesNothing() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        XCTAssertNotNil(model.change)
        model.cancel()
        XCTAssertNil(model.change)
        XCTAssertTrue(controls.revocations.isEmpty)
        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
    }

    func testConfirmingARevocationRevokesExactlyThatAndShowsTheReadBackState() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        // The core's stored state after the revocation is what the screen shows, not the expectation.
        controls.duringRevoke = { controls.routes = .success([privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")]) }
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.revocations, [["route-personal", "text_interpretation"]])
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("Stopped sorting your text notes"), message)
        XCTAssertTrue(message.contains("A request already sent cannot be recalled"), message)
        XCTAssertTrue(model.followedWork.isEmpty, "nothing was pending, so nothing is followed")
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
        XCTAssertTrue(model.isLocalOnly, "with nothing allowed the app is still usable, local only")
        XCTAssertNil(model.change)
    }

    func testRevokingWhatIsNotGrantedIsNotProposedAndAStaleConfirmationDoesNothing() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "review")
        XCTAssertNil(model.change)
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        let earlier = try XCTUnwrap(model.change)
        model.cancel()
        await model.confirmChange(earlier)
        XCTAssertTrue(controls.revocations.isEmpty)
    }

    func testARefusedRevocationIsReportedAndStoredStateIsReadAgain() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.revokeResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "route_unknown", message: "synthetic")))
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(model.actionState, .failed("That route no longer exists. Nothing was changed."))
        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"), "shown as stored")
    }

    func testRevokingAProfileVersionRetiresItsQueuedWorkAndShowsTheCoresRetiredListAfterwards() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        XCTAssertEqual(model.queuedWork.map(\.version.profileVersion), ["hosted-v1"])
        XCTAssertTrue(model.retiredWork.isEmpty, "a revoked version whose revocation retired nothing is not listed")
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.revokeProfileResult = .success(())
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = [retiredJob("job-1")]
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.profileRevocations, ["hosted-v1"])
        XCTAssertTrue(model.queuedWork.isEmpty, "the queue shown is the one read back after the revocation")
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("The revocation retired 1 job"), message)
        XCTAssertEqual(model.retiredWork.map(\.profileVersion), ["hosted-v1"])
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1"], "the retired job is the core's listing")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.queued])
        XCTAssertEqual(model.retiredWork.first?.total, 1)
        model.proposeRevokeProfile(profileVersion: "old-v1")
        XCTAssertNil(model.change, "an already revoked version is not offered")
    }

    func testRequeueIsExplicitInspectableAndKeepsItsIdentityAcrossAnUnclearRetry() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        controls.recordResult = .success(
            JobConfigurationRequeueRecord(
                oldJobID: "job-1", newJobID: "job-new", fromProfileVersion: "hosted-v1", toProfileVersion: "private-v1",
                requeuedAt: "2026-10-01T00:00:00Z"))
        await model.refresh()
        XCTAssertTrue(controls.requeues.isEmpty, "a refresh moves nothing")
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        let shown = try XCTUnwrap(model.requeueProposal)
        XCTAssertTrue(controls.requeues.isEmpty, "opening the question moves nothing")

        controls.requeueResult = .failure(JobConfigurationError.unreadable)
        await model.confirmRequeue(shown)
        XCTAssertEqual(model.requeueProposal, shown, "an unclear result keeps the same question and identity")
        controls.requeueResult = .success(
            JobConfigurationRequeuedJob(jobID: "job-new", itemID: "item-job-1", status: "queued", profileVersion: "private-v1"))
        await model.confirmRequeue(shown)

        XCTAssertEqual(controls.requeues.map { $0[1] }, [shown.newJobID, shown.newJobID])
        XCTAssertEqual(controls.requeues.last, ["job-1", shown.newJobID, "private-v1"])
        XCTAssertEqual(model.lastRequeue?.newJobID, "job-new")
        XCTAssertNil(model.requeueProposal)
    }

    func testRequeueToTheSameOrARevokedVersionIsNotProposedAndARefusalEndsTheQuestion() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "hosted-v1")
        XCTAssertNil(model.requeueProposal)
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "old-v1")
        XCTAssertNil(model.requeueProposal)
        model.proposeRequeue(jobID: "job-missing", toProfileVersion: "private-v1")
        XCTAssertNil(model.requeueProposal)
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        controls.requeueResult = .failure(JobConfigurationError.refused)
        await model.confirmRequeue(try XCTUnwrap(model.requeueProposal))
        XCTAssertNil(model.requeueProposal)
        guard case .failed = model.actionState else { return XCTFail("expected failed") }
    }

    func testRepinNeedsConfirmationAndReportsWhenNothingWasMoved() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        await model.refresh()
        model.proposeRepinWaitingWork(routeID: "route-personal")
        XCTAssertTrue(controls.repins.isEmpty)
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.repins, ["route-personal"])
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.hasPrefix("Nothing was moved"))
    }

    func testRepinIsNotOfferedOnARouteThatAllowsNothing() async {
        let (model, _) = makeModel()
        await model.refresh()
        model.proposeRepinWaitingWork(routeID: "route-personal")
        XCTAssertNil(model.change)
    }

    func testUnreadableQueuedWorkLeavesPermissionsAndLocalUseWorking() async {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinnedFailure = JobConfigurationError.unreadable
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.queuedWorkUnreadable)
        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
    }

    func testRelockingDropsAnOpenRevocationAndQueuedWork() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        let shown = try XCTUnwrap(model.change)
        scope = .captureOnly
        await model.confirmChange(shown)
        XCTAssertTrue(controls.revocations.isEmpty, "a capture-only session revokes nothing")
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.change)
        XCTAssertTrue(model.queuedWork.isEmpty)
        XCTAssertTrue(model.retiredWork.isEmpty)
    }

    func testARevokedVersionsRetiredWorkCanBeMovedOnPurposeAfterwardsAndShowsWhereItWent() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
            ], total: 2, truncated: false)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = [retiredJob("job-1"), retiredJob("job-2", item: "item-2")]
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("The revocation retired 2 jobs"), message)
        XCTAssertFalse(message.contains("had already been sent"), "the result counts no sent requests: \(message)")
        XCTAssertTrue(message.hasSuffix(PrivacySettingsText.lateReplyDiscarded), message)
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1", "job-2"])
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.queued, .running])

        model.proposeRequeue(jobID: "job-2", toProfileVersion: "private-v1")
        let shown = try XCTUnwrap(model.requeueProposal)
        XCTAssertEqual(shown.toProfileVersion, "private-v1")
        XCTAssertEqual(shown.fromVersionLabel, "hosted · OpenAI · synthetic-model")
        XCTAssertTrue(controls.requeues.isEmpty, "opening the question moves nothing")
        model.proposeRequeue(jobID: "job-2", toProfileVersion: "old-v1")
        XCTAssertEqual(model.requeueProposal, shown, "a revoked target is not proposed")
        // The core records the move; the screen reads that back rather than dropping the job.
        controls.retired["hosted-v1"] = [retiredJob("job-1"), retiredJob("job-2", item: "item-2", requeuedTo: "job-new")]
        await model.confirmRequeue(shown)
        XCTAssertEqual(controls.requeues.last, ["job-2", shown.newJobID, "private-v1"])
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1", "job-2"], "a moved job stays on the core's list")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.requeuedTo), [nil, "job-new"])
        let moved = PrivacySettingsText.retiredJob(try XCTUnwrap(model.retiredWork.first?.jobs.last))
        XCTAssertTrue(moved.hasSuffix(" Moved to job job-new."), moved)
        model.proposeRequeue(jobID: "job-2", toProfileVersion: "private-v1")
        XCTAssertNil(model.requeueProposal, "a job the core has seen moved is not offered again")
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        XCTAssertNotNil(model.requeueProposal, "the job not yet moved still is")
    }

    func testAnUnreadableRequeueRecordIsShownAsUnreadableAndCanBeReadAgain() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        controls.recordResult = .failure(JobConfigurationError.unreadable)
        await model.confirmRequeue(try XCTUnwrap(model.requeueProposal))
        XCTAssertTrue(model.requeueRecordUnreadable)
        XCTAssertNil(model.lastRequeue)
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertFalse(message.contains("recorded below"), message)

        controls.recordResult = .success(
            JobConfigurationRequeueRecord(
                oldJobID: "job-1", newJobID: "job-new", fromProfileVersion: "hosted-v1", toProfileVersion: "private-v1",
                requeuedAt: "2026-10-01T00:00:00Z"))
        await model.loadRequeueRecord()
        XCTAssertFalse(model.requeueRecordUnreadable)
        XCTAssertEqual(model.lastRequeue?.oldJobID, "job-1")
    }

    func testAFullRepinBatchSaysMoreMayStillBeWaiting() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.repinResult = .success(PrivacySettingsModel.repinBatchLimit)
        await model.refresh()
        model.proposeRepinWaitingWork(routeID: "route-personal")
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("More may still be waiting"), message)
    }

    func testARouteRevocationClaimsNoOutcomeAndFollowsEachPendingJobToTheStateTheCoreReports() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
                JobConfigurationPinnedJob(jobID: "job-3", itemID: "item-3", status: "running"),
            ], total: 3, truncated: false)
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        controls.duringRevoke = { controls.routes = .success([privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")]) }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("Stopped"), message)
        XCTAssertTrue(message.contains("A request already sent cannot be recalled"), message)
        XCTAssertFalse(message.contains("is not sent"), "a request already in flight was sent: \(message)")
        XCTAssertFalse(message.contains("not applied"), "the change cannot say which reply it drops: \(message)")
        XCTAssertTrue(message.contains("3 jobs pending"), message)
        XCTAssertEqual(
            model.followedWork.map(\.listing), [.listed("queued"), .listed("running"), .listed("running")],
            "while the core still lists them, each job shows its listed status")
        XCTAssertEqual(model.followedWork.map(\.profileVersion), ["hosted-v1", "hosted-v1", "hosted-v1"])
        XCTAssertEqual(controls.processingReads, ["item-1"], "a request in flight needs no item read; a queued job does")
        XCTAssertFalse(model.followedWorkIncomplete)

        // The core settles them and they leave its listing. Their notes report refused, sorted and unreadable; none of
        // that is the job's own result, because the listing does not say what kind of job each was.
        controls.pinned["hosted-v1"] = nil
        controls.processing["item-1"] = .failure(JobConfigurationError.unreadable)
        let refused = PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: "awaiting_configuration")
        controls.processing["item-2"] = .success(refused)
        let sorted = PrivacyItemProcessing(processingState: "processed", processingJobStatus: nil)
        controls.processing["item-3"] = .success(sorted)
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.jobID), ["job-1", "job-2", "job-3"])
        XCTAssertEqual(model.followedWork.map(\.listing), [.notListed, .notListed, .notListed])
        XCTAssertEqual(model.followedWork.map(\.note), [.unreadable, .reported(refused), .reported(sorted)])
        XCTAssertEqual(model.followedWork.map(\.requestSent), [false, true, true])
        let shown = model.followedWork.map(PrivacySettingsText.followedJob)
        XCTAssertTrue(shown[0].contains("no longer queued or running. What this job itself returned is not shown"), shown[0])
        XCTAssertTrue(shown[0].contains("text sorting (which may be other work on it): could not be read"), shown[0])
        XCTAssertTrue(shown[1].contains("its request had been sent"), shown[1])
        XCTAssertTrue(shown[1].contains("text sorting (which may be other work on it): refused"), shown[1])
        XCTAssertTrue(shown[2].hasSuffix("text sorting (which may be other work on it): sorted."), shown[2])
        for text in shown {
            XCTAssertFalse(text.contains("reply was applied"), text)
            XCTAssertFalse(text.contains("Nothing was applied"), text)
        }

        // Reading again answers the unreadable note.
        controls.processing["item-1"] = .success(refused)
        await model.refresh()
        XCTAssertEqual(model.followedWork[0].note, .reported(refused))
    }

    func testAQueuedJobKeepsItsListedStatusAndItsNotesRefusalIsShownAsTheNotes() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(model.followedWork.map(\.listing), [.listed("queued")])

        // The note's sorting is held for configuration while the core still lists the job as queued. The listing
        // does not say the two are the same work, so the job keeps its listed status and the note's state follows.
        let refused = PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: "awaiting_configuration")
        controls.processing["item-job-1"] = .success(refused)
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.listing), [.listed("queued")])
        XCTAssertEqual(model.followedWork.map(\.note), [.reported(refused)])
        let shown = PrivacySettingsText.followedJob(model.followedWork[0])
        XCTAssertTrue(shown.hasPrefix("Job job-1 · listed queued."), shown)
        XCTAssertTrue(shown.contains("text sorting (which may be other work on it): refused"), shown)

        controls.processing["item-job-1"] = .failure(JobConfigurationError.unreadable)
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.listing), [.listed("queued")])
        XCTAssertEqual(model.followedWork.map(\.note), [.unreadable])
    }

    func testAFollowedJobWhoseListingCannotBeReadIsUnknownNotSettled() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "running")], total: 1, truncated: false)
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))

        controls.pinnedFailure = JobConfigurationError.unreadable
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.listing), [.unknown])
        XCTAssertEqual(model.followedWork.map(\.requestSent), [true])
        let shown = PrivacySettingsText.followedJob(model.followedWork[0])
        XCTAssertTrue(shown.contains("request already sent; its current listing could not be read"), shown)

        // A truncated listing that does not show the job does not prove it ended either.
        controls.pinnedFailure = nil
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-other", itemID: "item-9", status: "queued")], total: 101,
            truncated: true)
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.listing), [.unknown])
    }

    func testARequestSeenSentNeverReadsAsNotSentWhenTheCoreRequeuesItAndItsNoteIsUnreadable() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "running")], total: 1, truncated: false)
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(model.followedWork.map(\.listing), [.listed("running")])
        XCTAssertEqual(model.followedWork.map(\.requestSent), [true])

        // The core refuses the late reply and backs the job off to queued; its note cannot be read just now.
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued")], total: 1, truncated: false)
        controls.processing["item-1"] = .failure(JobConfigurationError.unreadable)
        await model.refresh()
        XCTAssertEqual(model.queuedWork.flatMap(\.jobs.jobs).map(\.status), ["queued"])
        XCTAssertEqual(model.followedWork.map(\.listing), [.listed("queued")])
        XCTAssertEqual(model.followedWork.map(\.note), [.unreadable])
        XCTAssertEqual(model.followedWork.map(\.requestSent), [true])
        var shown = PrivacySettingsText.followedJob(model.followedWork[0])
        XCTAssertFalse(shown.contains("not sent"), shown)
        XCTAssertTrue(shown.contains("request already sent; listed queued again"), shown)
        XCTAssertTrue(shown.contains("could not be read"), shown)

        // Readable again: still listed queued, and still known to have been sent.
        controls.processing["item-1"] = .success(PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: nil))
        await model.refresh()
        XCTAssertEqual(model.followedWork.map(\.requestSent), [true])
        shown = PrivacySettingsText.followedJob(model.followedWork[0])
        XCTAssertFalse(shown.contains("not sent"), shown)
        XCTAssertTrue(shown.contains("request already sent"), shown)
    }

    func testFollowedWorkIsIncompleteWhenThePendingListingIsTruncatedAndIsDroppedOnRelock() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "running")], total: 40, truncated: true)
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(model.followedWork.map(\.jobID), ["job-1"])
        XCTAssertTrue(model.followedWorkIncomplete, "jobs beyond the listing are not followed, and the screen says so")

        scope = .captureOnly
        await model.refresh()
        XCTAssertTrue(model.followedWork.isEmpty)
        XCTAssertFalse(model.followedWorkIncomplete)
    }

    func testAnUnchangedRouteRevocationFollowsNothing() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        controls.revokeResult = .success(.unchanged)
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertTrue(model.followedWork.isEmpty)
    }

    func testARelockedItemReadLocksTheScreen() async throws {
        let (model, controls) = makeModel(routes: [grantedRoute()])
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        controls.pinned["hosted-v1"] = nil
        controls.processing["item-job-1"] = .failure(JobConfigurationError.notAuthenticated)
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.followedWork.isEmpty)
    }

    func testAFailedPreviewHoldsNothingBackAndTheResultIsTheCoresRetiredListWithThatDetailUnknown() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        let shown = try XCTUnwrap(model.change)
        controls.pinnedFailure = JobConfigurationError.unreadable
        controls.duringRevokeProfile = {
            controls.retired["hosted-v1"] = [retiredJob("job-1")]
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(shown)
        XCTAssertEqual(controls.profileRevocations, ["hosted-v1"], "the revocation does not wait on a preview")
        guard case .changed(let message) = model.actionState else {
            return XCTFail("expected changed, got \(String(describing: model.actionState))")
        }
        XCTAssertTrue(message.contains("The revocation retired 1 job"), message)
        XCTAssertFalse(message.contains("already been sent"), "nothing is claimed about a request the preview did not see: \(message)")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1"], "the retired job comes from the core's list")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.unknown])
        let row = PrivacySettingsText.retiredJob(try XCTUnwrap(model.retiredWork.first?.jobs.first))
        XCTAssertEqual(row, "Job job-1 · retired by the revocation.")
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        XCTAssertNotNil(model.requeueProposal, "the retired job is still open to an explicit move")
    }

    func testRetiredWorkBeyondOnePageIsListedPageByPageAndEveryJobExactlyOnce() async throws {
        let (model, controls) = makeModel()
        let ids = (1...45).map { String(format: "job-%02d", $0) }
        // The core lists at most 100 pending jobs, and this screen's preview is capped the same way.
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-01", itemID: "item-1", status: "queued")], total: 45, truncated: true)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = ids.map { retiredJob($0) }
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.profileRevocations, ["hosted-v1"], "a capped preview no longer holds the revocation back")
        guard case .changed(let message) = model.actionState else {
            return XCTFail("expected changed, got \(String(describing: model.actionState))")
        }
        XCTAssertTrue(message.contains("The revocation retired 45 jobs"), message)
        XCTAssertTrue(message.contains("20 of them are shown; the rest load from the list"), message)
        let first = try XCTUnwrap(model.retiredWork.first)
        XCTAssertEqual(first.total, 45)
        XCTAssertEqual(first.jobs.count, PrivacySettingsModel.queuedJobLimit)
        XCTAssertNotNil(first.nextCursor)
        XCTAssertEqual(first.remaining, 25)
        // A capped preview says only what it listed: the job it saw was queued; of the others it claims nothing.
        XCTAssertEqual(first.jobs.first?.preview, .queued)
        XCTAssertEqual(first.jobs.last?.preview, .unknown)

        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        XCTAssertEqual(model.retiredWork.first?.jobs.count, 40)
        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        let all = try XCTUnwrap(model.retiredWork.first)
        XCTAssertEqual(all.jobs.map(\.jobID), ids, "every retired job is listed exactly once, in the core's order")
        XCTAssertNil(all.nextCursor)
        XCTAssertEqual(all.remaining, 0)
        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        XCTAssertEqual(model.retiredWork.first?.jobs.count, 45, "nothing more to read")

        // A refresh keeps every page the user had reached.
        await model.refresh()
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ids)
        model.proposeRequeue(jobID: "job-45", toProfileVersion: "private-v1")
        XCTAssertNotNil(model.requeueProposal, "a job on a later page can be moved on purpose")
    }

    func testAskingForMoreRetiredWorkTwiceWhileOnePageIsBeingReadListsThatPageOnce() async throws {
        let (model, controls) = makeModel()
        let ids = (1...45).map { String(format: "job-%02d", $0) }
        controls.retired["hosted-v1"] = ids.map { retiredJob($0) }
        controls.listing = .success(listingWithHostedRevoked())
        await model.refresh()
        let first = try XCTUnwrap(model.retiredWork.first { $0.profileVersion == "hosted-v1" })
        XCTAssertEqual(first.jobs.count, PrivacySettingsModel.queuedJobLimit)
        let cursor = try XCTUnwrap(first.nextCursor)
        XCTAssertFalse(model.isLoadingRetiredWork(profileVersion: "hosted-v1"))
        let readsBefore = controls.retiredReads.count

        // The core holds the second page's read open while the screen is asked for more twice over.
        let hold = PrivacyReadHold()
        controls.duringRetiredRead = { after in if after != nil { await hold.wait() } }
        let firstAsk = Task { await model.loadMoreRetiredWork(profileVersion: "hosted-v1") }
        let deadline = Date(timeIntervalSinceNow: 5)
        while controls.retiredReads.count == readsBefore, Date() < deadline { await Task.yield() }
        XCTAssertEqual(controls.retiredReads.count, readsBefore + 1, "the first ask is reading the next page")
        XCTAssertEqual(controls.retiredReads.last?[2], cursor)
        XCTAssertTrue(model.isLoadingRetiredWork(profileVersion: "hosted-v1"), "the row waits for the read")

        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        XCTAssertEqual(controls.retiredReads.count, readsBefore + 1, "a second ask during the read reads nothing")
        XCTAssertEqual(model.retiredWork.first { $0.profileVersion == "hosted-v1" }?.jobs.count, 20, "nothing was added yet")
        XCTAssertFalse(model.retiredWorkUnreadable)

        hold.release()
        await firstAsk.value
        XCTAssertFalse(model.isLoadingRetiredWork(profileVersion: "hosted-v1"))
        let shown = try XCTUnwrap(model.retiredWork.first { $0.profileVersion == "hosted-v1" })
        XCTAssertEqual(shown.jobs.map(\.jobID), Array(ids.prefix(40)), "the page was added once, each job listed once")
        XCTAssertEqual(Set(shown.jobs.map(\.jobID)).count, shown.jobs.count)
        XCTAssertEqual(shown.total, 45)
        XCTAssertEqual(shown.remaining, 5)
        XCTAssertEqual(PrivacySettingsText.retiredShowMore(shown), "Show 5 more")

        // Once the read is over, asking again reads the page after it, once more.
        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        XCTAssertEqual(controls.retiredReads.count, readsBefore + 2)
        let all = try XCTUnwrap(model.retiredWork.first { $0.profileVersion == "hosted-v1" })
        XCTAssertEqual(all.jobs.map(\.jobID), ids, "every retired job is listed exactly once")
        XCTAssertNil(all.nextCursor)
    }

    /// Revokes `hosted-v1` with 25 jobs pinned to it, of which `running` had been listed as running by the preview, and
    /// returns the result the screen shows. The core's retired list is five jobs longer than this screen's first page.
    private func revokeTwentyFive(
        _ model: PrivacySettingsModel, _ controls: FakePrivacySettingsControls, running: Set<String>,
        previewTruncated: Bool = false
    ) async throws -> String {
        let ids = (1...25).map { String(format: "job-%02d", $0) }
        let previewed = previewTruncated ? ids.filter { running.contains($0) || $0 == "job-01" } : ids
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: previewed.map {
                JobConfigurationPinnedJob(jobID: $0, itemID: "item-1", status: running.contains($0) ? "running" : "queued")
            },
            total: 25, truncated: previewTruncated)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = ids.map { retiredJob($0) }
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else {
            XCTFail("expected changed, got \(String(describing: model.actionState))")
            return ""
        }
        XCTAssertTrue(message.contains("The revocation retired 25 jobs"), message)
        XCTAssertTrue(message.contains("20 of them are shown; the rest load from the list"), message)
        XCTAssertFalse(message.contains("of them had already been sent"), "no count is phrased as covering every retired job: \(message)")
        XCTAssertEqual(model.retiredWork.first { $0.profileVersion == "hosted-v1" }?.jobs.count, 20)
        return message
    }

    /// The preview is read before the revocation, and the runner keeps dispatching in between, so no count of sent
    /// requests taken from it can be trusted as exact or as an upper bound.
    private func assertCountsNoSentRequests(_ message: String) {
        XCTAssertFalse(message.contains("had already been sent"), "no exact count of sent requests: \(message)")
        XCTAssertFalse(message.contains("Up to"), "no upper bound on sent requests: \(message)")
        XCTAssertFalse(message.contains("None of"), "no claim that none was sent: \(message)")
        XCTAssertFalse(message.contains("not sent"), message)
        XCTAssertTrue(message.hasSuffix(PrivacySettingsText.lateReplyDiscarded), message)
    }

    func testTheResultCountsNoSentRequestsWhateverThePreviewListedAndEachRowSaysWhatItSawLabelledAsBefore() async throws {
        let (model, controls) = makeModel()
        let message = try await revokeTwentyFive(model, controls, running: ["job-05", "job-25"])
        assertCountsNoSentRequests(message)

        // The rows show what the preview listed, labelled as seen just before, on this page and the next.
        await model.loadMoreRetiredWork(profileVersion: "hosted-v1")
        let all = try XCTUnwrap(model.retiredWork.first { $0.profileVersion == "hosted-v1" })
        XCTAssertEqual(all.jobs.filter { $0.preview == .running }.map(\.jobID), ["job-05", "job-25"])
        XCTAssertTrue(
            PrivacySettingsText.retiredJob(all.jobs[24]).contains(
                "its request had already been sent, and a reply that arrives after the revocation is not applied"),
            PrivacySettingsText.retiredJob(all.jobs[24]))
        let queued = PrivacySettingsText.retiredJob(all.jobs[0])
        XCTAssertEqual(
            queued,
            "Job job-01 · retired by the revocation; it was listed queued just before the revocation and may have been "
                + "sent after that; a reply is not applied.")
    }

    func testWithACappedPreviewTheResultIsTheSameAndRowsThePreviewDidNotSeeClaimNothing() async throws {
        let (model, controls) = makeModel()
        let message = try await revokeTwentyFive(model, controls, running: ["job-01"], previewTruncated: true)
        assertCountsNoSentRequests(message)
        let page = try XCTUnwrap(model.retiredWork.first { $0.profileVersion == "hosted-v1" })
        XCTAssertEqual(page.jobs[0].preview, .running)
        XCTAssertEqual(page.jobs[1].preview, .unknown, "a capped preview labels nothing it did not list")
        XCTAssertEqual(PrivacySettingsText.retiredJob(page.jobs[1]), "Job job-02 · retired by the revocation.")
    }

    func testTheResultIsExactlyWhatTheCoreRetiredNotWhatWasListedBefore() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
            ], total: 2, truncated: false)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        // In the moment between the preview and the revocation, job-2's reply is applied and a new capture, job-3, is
        // pinned to the version. The core retires job-1 and job-3, and its list says exactly that.
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = [retiredJob("job-1"), retiredJob("job-3", item: "item-3")]
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("The revocation retired 2 jobs"), message)
        XCTAssertFalse(
            message.contains("already been sent"), "the sent job was not retired, so no reply is said to be dropped: \(message)")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1", "job-3"])
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.queued, .notListed])
        let rows = (model.retiredWork.first?.jobs ?? []).map(PrivacySettingsText.retiredJob)
        XCTAssertEqual(
            rows[0],
            "Job job-1 · retired by the revocation; it was listed queued just before the revocation and may have been "
                + "sent after that; a reply is not applied.")
        XCTAssertEqual(
            rows[1],
            "Job job-3 · retired by the revocation; it was pinned to the version in the moment before the revocation "
                + "and may have been sent; a reply is not applied.")

        model.proposeRequeue(jobID: "job-2", toProfileVersion: "private-v1")
        XCTAssertNil(model.requeueProposal, "a job the core did not retire is not offered a move")
        model.proposeRequeue(jobID: "job-3", toProfileVersion: "private-v1")
        XCTAssertNotNil(model.requeueProposal, "the job pinned in that moment is offered its move")
    }

    func testRetiredWorkOfAVersionRevokedBeforeThisLaunchIsReadFromTheCoreAndCanBeMoved() async throws {
        let (model, controls) = makeModel()
        controls.retired["old-v1"] = [retiredJob("job-old", item: "item-old")]
        await model.refresh()
        XCTAssertEqual(model.retiredWork.map(\.profileVersion), ["old-v1"])
        XCTAssertEqual(model.retiredWork.first?.versionLabel, "old · OpenAI · synthetic-model")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.unknown], "nothing is claimed beyond the core's list")
        XCTAssertEqual(controls.retiredReads.map { $0[0] }, ["old-v1"], "only revoked versions are asked")
        model.proposeRequeue(jobID: "job-old", toProfileVersion: "hosted-v1")
        XCTAssertEqual(model.requeueProposal?.fromVersionLabel, "old · OpenAI · synthetic-model")
    }

    func testUnreadableRetiredWorkIsSaidSoAndReadAgainWithTheScreen() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.duringRevokeProfile = {
            controls.pinned["hosted-v1"] = nil
            controls.retired["hosted-v1"] = [retiredJob("job-1")]
            controls.retiredFailure = JobConfigurationError.unreadable
            controls.listing = .success(listingWithHostedRevoked())
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertEqual(message, PrivacySettingsText.profileRevokedUnread("hosted · OpenAI · synthetic-model"))
        XCTAssertTrue(model.retiredWorkUnreadable)
        XCTAssertTrue(model.retiredWork.isEmpty, "nothing is listed that was not read")
        XCTAssertEqual(model.phase, .loaded)

        controls.retiredFailure = nil
        await model.refresh()
        XCTAssertFalse(model.retiredWorkUnreadable)
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), ["job-1"], "the core kept the list")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.preview), [.queued], "what the screen listed just before is still shown")
    }

    func testHeldWorkNoticeDoesNotNameRoutesAsHoldingJobsItCannotAttributeToThem() {
        let notice = PrivacySettingsText.heldWork(["Work notes · Work"])
        XCTAssertTrue(notice.contains("by provider version, not by route"), notice)
        XCTAssertFalse(notice.contains("Queued work for"), notice)
    }
}
