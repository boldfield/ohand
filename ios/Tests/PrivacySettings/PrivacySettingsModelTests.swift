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

    func testRevokingAProfileVersionRetiresItsQueuedWorkAndShowsTheListingAfterwards() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        XCTAssertEqual(model.queuedWork.map(\.version.profileVersion), ["hosted-v1"])
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.revokeProfileResult = .success(())
        controls.duringRevokeProfile = { controls.pinned["hosted-v1"] = nil }
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.profileRevocations, ["hosted-v1"])
        XCTAssertTrue(model.queuedWork.isEmpty, "the queue shown is the one read back after the revocation")
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("1 job was pending just before and is no longer listed"), message)
        XCTAssertEqual(model.unlistedWork.map(\.jobs.first?.jobID), ["job-1"], "the unlisted job stays open to a deliberate move")
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
    }

    func testARevokedVersionsUnlistedWorkCanBeMovedOnPurposeAfterwards() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
            ], total: 2, truncated: false)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        controls.duringRevokeProfile = { controls.pinned["hosted-v1"] = nil }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("2 jobs were pending just before and are no longer listed"), message)
        XCTAssertTrue(message.contains("1 had already been sent; a reply that arrives after the revocation is not applied"), message)
        XCTAssertEqual(model.unlistedWork.first?.jobs.map(\.jobID), ["job-1", "job-2"])
        XCTAssertEqual(model.unlistedWork.first?.jobs.map(\.requestSent), [false, true])

        model.proposeRequeue(jobID: "job-2", toProfileVersion: "private-v1")
        let shown = try XCTUnwrap(model.requeueProposal)
        XCTAssertEqual(shown.toProfileVersion, "private-v1")
        XCTAssertTrue(controls.requeues.isEmpty, "opening the question moves nothing")
        model.proposeRequeue(jobID: "job-2", toProfileVersion: "old-v1")
        XCTAssertEqual(model.requeueProposal, shown, "a revoked target is not proposed")
        await model.confirmRequeue(shown)
        XCTAssertEqual(controls.requeues.last, ["job-2", shown.newJobID, "private-v1"])
        XCTAssertEqual(model.unlistedWork.first?.jobs.map(\.jobID), ["job-1"], "a moved job leaves the unlisted work")
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

    func testAFailedPreReadOfAffectedWorkStopsAProfileRevocationWithNothingChanged() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = pinnedJobs(["job-1"])
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        let shown = try XCTUnwrap(model.change)
        controls.pinnedFailure = JobConfigurationError.unreadable
        await model.confirmChange(shown)
        XCTAssertEqual(controls.profileRevocations, [], "no revocation without the list of jobs it would retire")
        guard case .failed(let message) = model.actionState else { return XCTFail("expected failed") }
        XCTAssertTrue(message.contains("not revoked"), message)
        XCTAssertTrue(model.unlistedWork.isEmpty)
    }

    func testACappedPreReadStopsAProfileRevocationWithNothingChanged() async throws {
        let (model, controls) = makeModel()
        // The core lists at most 100 jobs; this version has one more than that.
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued")],
            total: PrivacySettingsModel.listedJobLimit + 1, truncated: true)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        await model.confirmChange(try XCTUnwrap(model.change))
        XCTAssertEqual(controls.profileRevocations, [], "no revocation while some of the work it would retire cannot be listed")
        guard case .failed(let message) = model.actionState else { return XCTFail("expected failed, got \(String(describing: model.actionState))") }
        XCTAssertTrue(message.contains("101 jobs pending"), message)
        XCTAssertTrue(message.contains("at most 100"), message)
        XCTAssertTrue(message.contains("was not revoked"), message)
        XCTAssertTrue(model.unlistedWork.isEmpty)
        XCTAssertNil(model.change)
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertEqual(model.queuedWork.map(\.version.profileVersion), ["hosted-v1"], "the version and its work stay on screen")
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        XCTAssertNotNil(model.change, "the version is still unrevoked and can be asked about again")
    }

    func testAJobThatEndedBetweenTheTwoReadsIsNotCalledRetiredAndTheCoresRefusalOfItsMoveIsShown() async throws {
        let (model, controls) = makeModel()
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
            ], total: 2, truncated: false)
        await model.refresh()
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        // job-2's reply is applied in the moment before the revocation runs, so the core retires only job-1. Its
        // listing afterwards shows neither and does not say which was which.
        controls.duringRevokeProfile = { controls.pinned["hosted-v1"] = nil }
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("2 jobs were pending just before and are no longer listed"), message)
        XCTAssertTrue(message.contains("keeps its own outcome"), message)
        XCTAssertFalse(message.contains("jobs retired"), "the screen does not call a job retired that may have ended: \(message)")
        XCTAssertFalse(message.contains("does not change the note"), "a reply applied before the revocation changed it: \(message)")
        XCTAssertEqual(
            model.unlistedWork.first?.jobs,
            [
                PrivacyUnlistedJob(jobID: "job-1", itemID: "item-1", requestSent: false),
                PrivacyUnlistedJob(jobID: "job-2", itemID: "item-2", requestSent: true),
            ])
        let rows = (model.unlistedWork.first?.jobs ?? []).map(PrivacySettingsText.unlistedJob)
        for row in rows {
            XCTAssertTrue(row.contains("no longer listed"), row)
            XCTAssertTrue(row.contains("unless it ended in that moment"), row)
            XCTAssertFalse(row.contains("retired (provider revoked)"), row)
        }
        XCTAssertTrue(rows[1].contains("was running (request already sent) just before"), rows[1])

        // Only the core knows which it was: it refuses to move a job that had ended, and the screen shows that refusal.
        model.proposeRequeue(jobID: "job-2", toProfileVersion: "private-v1")
        controls.requeueResult = .failure(JobConfigurationError.refused)
        await model.confirmRequeue(try XCTUnwrap(model.requeueProposal))
        guard case .failed(let refusal) = model.actionState else { return XCTFail("expected failed") }
        XCTAssertTrue(refusal.contains("had already ended"), refusal)
        XCTAssertNil(model.requeueProposal, "a refusal ends the question")
        XCTAssertEqual(
            model.unlistedWork.first?.jobs.map(\.jobID), ["job-1", "job-2"],
            "the core did not say why it refused, so the job is not dropped as if it had been moved")
    }

    func testHeldWorkNoticeDoesNotNameRoutesAsHoldingJobsItCannotAttributeToThem() {
        let notice = PrivacySettingsText.heldWork(["Work notes · Work"])
        XCTAssertTrue(notice.contains("by provider version, not by route"), notice)
        XCTAssertFalse(notice.contains("Queued work for"), notice)
    }
}
