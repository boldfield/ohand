import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real processing-policy screen in a window over the real model (with scripted controls). The screen reports
/// the words each identified part shows; the tests drive the model calls the screen's buttons make and read what is on
/// screen.
@MainActor
final class PrivacySettingsViewTests: XCTestCase {
    private var controls: FakePrivacySettingsControls!
    private var model: PrivacySettingsModel!
    private var rendered: [String: String] = [:]
    private var retainedWindow: UIWindow?

    override func setUp() async throws {
        try await super.setUp()
        rendered = [:]
        controls = FakePrivacySettingsControls(
            routes: [privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")],
            listing: providerListing(
                versions: [(profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: false, supported: true)],
                defaultVersion: nil))
        model = PrivacySettingsModel(controls: controls, readScope: { .authenticated })
        let hosting = UIHostingController(
            rootView: NavigationView {
                PrivacySettingsView(model: model, renderReporter: { [unowned self] in rendered = $0 })
            })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        await model.refresh()
        settle()
    }

    override func tearDown() async throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        model = nil
        try await super.tearDown()
    }

    private func settle() {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

    private static let offer = PrivacySettingsView.offerIdentifier("route-personal", "text_interpretation", "hosted-v1")

    func testAFreshScreenShowsTheLocalOnlyNoticeRouteStatusAndOffersForUngrantedCapabilities() {
        XCTAssertEqual(rendered[PrivacySettingsView.noticeIdentifier], PrivacySettingsText.localFirstNotice)
        XCTAssertEqual(
            PrivacySettingsText.infoParagraphs,
            [PrivacySettingsText.localFirstExplanation, PrivacySettingsText.stickyNotice, PrivacySettingsText.confirmationNote],
            "the full explanation moves to the info sheet; the screen keeps one footer line")
        XCTAssertEqual(
            rendered[PrivacySettingsView.routeIdentifier("route-personal")], "Personal route · Local capture")
        XCTAssertEqual(
            rendered[PrivacySettingsView.routeStatusIdentifier("route-personal")], PrivacySettingsText.routeLocalOnly)
        XCTAssertEqual(
            rendered[Self.offer], "Allow sorting your text notes via hosted · OpenAI · synthetic-model")
        XCTAssertNil(rendered[PrivacySettingsView.offerIdentifier("route-personal", "review", "hosted-v1")], "the profile declares no review support, so none is offered")
        XCTAssertNil(rendered[PrivacySettingsView.questionIdentifier], "no question until an offer is chosen")
        XCTAssertNil(rendered[PrivacySettingsView.allowIdentifier])
        XCTAssertTrue(controls.grants.isEmpty)
    }

    func testChoosingAnOfferShowsTheQuestionWithAllowAndNotNowAndGrantsNothing() throws {
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        settle()
        let proposal = try XCTUnwrap(model.proposal)
        XCTAssertEqual(rendered[PrivacySettingsView.questionIdentifier], PrivacySettingsText.proposalQuestion(proposal))
        XCTAssertEqual(rendered[PrivacySettingsView.confirmationNoteIdentifier], PrivacySettingsText.confirmationFooter)
        XCTAssertEqual(rendered[PrivacySettingsView.allowIdentifier], "Allow")
        XCTAssertEqual(rendered[PrivacySettingsView.cancelIdentifier], "Not now")
        XCTAssertTrue(controls.grants.isEmpty, "showing the question authorizes nothing")
    }

    func testNotNowShowsTheCancelledResultAndTheScreenStaysLocalOnly() {
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        model.cancel()
        settle()
        XCTAssertNil(rendered[PrivacySettingsView.questionIdentifier])
        XCTAssertEqual(rendered[PrivacySettingsView.actionStateIdentifier], PrivacySettingsText.cancelledNotice)
        XCTAssertEqual(
            rendered[PrivacySettingsView.routeStatusIdentifier("route-personal")], PrivacySettingsText.routeLocalOnly)
        XCTAssertTrue(controls.grants.isEmpty)
    }

    func testAllowShowsTheGrantedStatusAndStopsOfferingTheGrantedCapability() async throws {
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        controls.routes = .success([
            privacyRoute(
                id: "route-personal", name: "Local capture", scope: "personal",
                grants: [("text_interpretation", ["cloud"])])
        ])
        await model.confirm(try XCTUnwrap(model.proposal))
        settle()
        guard case .allowed = model.actionState else { return XCTFail("expected allowed") }
        XCTAssertNil(rendered[PrivacySettingsView.questionIdentifier])
        XCTAssertEqual(
            rendered[PrivacySettingsView.routeStatusIdentifier("route-personal")],
            "Allowed: Sorting your text notes via a hosted provider")
        XCTAssertTrue(rendered[PrivacySettingsView.actionStateIdentifier]?.hasPrefix("Allowed ") ?? false)
        XCTAssertNil(rendered[Self.offer], "a granted capability is not offered again")
    }

    // MARK: Revocation, queued work and requeue

    private func grantedRoute(capability: String = "text_interpretation") -> RoutePolicyRoute {
        privacyRoute(id: "route-personal", name: "Local capture", scope: "personal", grants: [(capability, ["cloud"])])
    }

    private func reload(_ routes: [RoutePolicyRoute]) async {
        controls.routes = .success(routes)
        await model.refresh()
        settle()
    }

    func testAGrantedRouteShowsStopAndQueueWaitingNotesRowsAndTheConfirmationCardRevokesNothingUntilConfirmed() async throws {
        await reload([grantedRoute()])
        let stop = PrivacySettingsView.revokeIdentifier("route-personal", "text_interpretation")
        XCTAssertEqual(rendered[stop], "Stop sorting your text notes")
        XCTAssertEqual(
            rendered[PrivacySettingsView.repinIdentifier("route-personal")],
            "Queue waiting notes with the provider selected now")
        XCTAssertEqual(
            rendered[PrivacySettingsView.revokeProfileIdentifier("hosted-v1")]?.hasPrefix("Revoke hosted"), true)

        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        settle()
        let change = try XCTUnwrap(model.change)
        XCTAssertEqual(rendered[PrivacySettingsView.changeQuestionIdentifier], PrivacySettingsText.changeQuestion(change))
        XCTAssertEqual(rendered[PrivacySettingsView.changeConfirmIdentifier], "Confirm")
        XCTAssertEqual(rendered[PrivacySettingsView.cancelIdentifier], "Not now")
        XCTAssertTrue(controls.revocations.isEmpty, "showing the question revokes nothing")

        let fake = controls!
        fake.duringRevoke = {
            fake.routes = .success([privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")])
        }
        await model.confirmChange(change)
        settle()
        XCTAssertNil(rendered[PrivacySettingsView.changeQuestionIdentifier])
        XCTAssertTrue(rendered[PrivacySettingsView.actionStateIdentifier]?.hasPrefix("Stopped sorting") ?? false)
        XCTAssertEqual(
            rendered[PrivacySettingsView.routeStatusIdentifier("route-personal")], PrivacySettingsText.routeLocalOnly)
    }

    func testARouteRevocationWithARequestInFlightShowsItAsSentThenTheRefusedStateTheCoreReports() async throws {
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running")], total: 1, truncated: false)
        await reload([grantedRoute()])
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        let fake = controls!
        fake.duringRevoke = {
            fake.routes = .success([privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")])
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        settle()

        let result = try XCTUnwrap(rendered[PrivacySettingsView.actionStateIdentifier])
        XCTAssertTrue(result.contains("A request already sent cannot be recalled"), result)
        XCTAssertFalse(result.contains("is not sent"), result)
        let row = PrivacySettingsView.followedJobIdentifier("job-2")
        XCTAssertEqual(
            rendered[row],
            "Job job-2 · request already sent, waiting for its reply. A reply changes the note only if the permissions "
                + "in force when it arrives allow it.")
        XCTAssertEqual(rendered[PrivacySettingsView.followedWorkCheckAgainIdentifier], PrivacySettingsText.followedWorkCheckAgain)

        // The late reply arrives and the core refuses it; the settled job leaves the core's listing.
        fake.pinned["hosted-v1"] = nil
        fake.processing["item-2"] = .success(
            PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: "awaiting_configuration"))
        await model.refresh()
        settle()
        XCTAssertNil(rendered[PrivacySettingsView.queuedJobIdentifier("job-2")], "the core no longer lists it as pending")
        XCTAssertEqual(
            rendered[row],
            "Job job-2 · no longer queued or running; its request had been sent. What this job itself returned is not "
                + "shown here; a reply changes the note only if the permissions in force when it arrived allowed it. "
                + "The note's text sorting (which may be other work on it): refused under the current permissions; the "
                + "note waits on this device until a permission allows it.")
        XCTAssertNil(rendered[PrivacySettingsView.followedWorkIncompleteIdentifier])
    }

    func testAFollowedJobThatLeavesTheListingIsNeverShownAsTheNotesSortingResult() async throws {
        // The listing carries no job type: this job may be a review of a note that was sorted long before.
        controls.pinned["review-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-review", itemID: "item-2", status: "running")], total: 1,
            truncated: false)
        controls.listing = .success(
            providerListing(
                versions: [
                    (profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: false, supported: true),
                    (profile: "review", version: "review-v1", providerProtocol: "open_ai", revoked: false, supported: true),
                ], defaultVersion: nil))
        await reload([grantedRoute(capability: "review")])
        model.proposeRevoke(routeID: "route-personal", capability: "review")
        await model.confirmChange(try XCTUnwrap(model.change))
        settle()

        controls.pinned["review-v1"] = nil
        controls.processing["item-2"] = .success(PrivacyItemProcessing(processingState: "processed", processingJobStatus: nil))
        await model.refresh()
        settle()
        let row = try XCTUnwrap(rendered[PrivacySettingsView.followedJobIdentifier("job-review")])
        XCTAssertTrue(row.hasPrefix("Job job-review · no longer queued or running; its request had been sent."), row)
        XCTAssertTrue(row.contains("What this job itself returned is not shown here"), row)
        XCTAssertTrue(row.hasSuffix("The note's text sorting (which may be other work on it): sorted."), row)
        XCTAssertFalse(row.contains("reply was applied"), row)
        XCTAssertFalse(row.contains("was sorted while"), row)
    }

    func testARequestShownAsSentNeverRendersAsNotSentWhenTheCoreRequeuesItAndItsNoteIsUnreadable() async throws {
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running")], total: 1, truncated: false)
        await reload([grantedRoute()])
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        settle()
        let row = PrivacySettingsView.followedJobIdentifier("job-2")
        XCTAssertTrue(rendered[row]?.contains("request already sent") ?? false, rendered[row] ?? "missing")

        // The core backs the refused job off to queued, and its note cannot be read just now.
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "queued")], total: 1, truncated: false)
        controls.processing["item-2"] = .failure(JobConfigurationError.unreadable)
        await model.refresh()
        settle()
        XCTAssertEqual(
            rendered[row],
            "Job job-2 · request already sent; listed queued again. It is sent again, and a reply changes the note, "
                + "only if the permissions in force then allow it. The note's text sorting (which may be other work on "
                + "it): could not be read.")
        XCTAssertFalse(rendered[row]?.contains("not sent") ?? true)
        XCTAssertEqual(rendered[PrivacySettingsView.followedWorkCheckAgainIdentifier], PrivacySettingsText.followedWorkCheckAgain)
    }

    func testQueuedWorkRowsShowTheStoredStatusTheHeldNoticeAndMoveOffers() async throws {
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [
                JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued"),
                JobConfigurationPinnedJob(jobID: "job-2", itemID: "item-2", status: "running"),
            ], total: 2, truncated: false)
        controls.listing = .success(
            providerListing(
                versions: [
                    (profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: false, supported: true),
                    (profile: "private", version: "private-v1", providerProtocol: "self_hosted", revoked: false, supported: true),
                ], defaultVersion: nil))
        await reload([privacyRoute(id: "route-personal", name: "Local capture", scope: "personal")])

        XCTAssertEqual(rendered[PrivacySettingsView.queuedJobIdentifier("job-1")], "Job job-1 · queued")
        XCTAssertEqual(
            rendered[PrivacySettingsView.queuedJobIdentifier("job-2")], "Job job-2 · running (request already sent)")
        XCTAssertEqual(
            rendered[PrivacySettingsView.heldWorkIdentifier],
            PrivacySettingsText.heldWork(["Personal route · Local capture"]))
        let offer = PrivacySettingsView.requeueOfferIdentifier("job-1", "private-v1")
        XCTAssertTrue(rendered[offer]?.hasPrefix("Move to private") ?? false)

        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        settle()
        let proposal = try XCTUnwrap(model.requeueProposal)
        XCTAssertEqual(rendered[PrivacySettingsView.requeueQuestionIdentifier], PrivacySettingsText.requeueQuestion(proposal))
        XCTAssertEqual(rendered[PrivacySettingsView.requeueConfirmIdentifier], "Move")
        XCTAssertTrue(controls.requeues.isEmpty, "showing the question moves nothing")
    }

    func testARevokedVersionsUnlistedJobsAndTheRequeueRecordAreShown() async throws {
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued")], total: 1, truncated: false)
        controls.listing = .success(
            providerListing(
                versions: [
                    (profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: false, supported: true),
                    (profile: "private", version: "private-v1", providerProtocol: "self_hosted", revoked: false, supported: true),
                ], defaultVersion: nil))
        await reload([grantedRoute()])
        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        let fake = controls!
        fake.duringRevokeProfile = {
            fake.pinned["hosted-v1"] = nil
            fake.listing = .success(
                providerListing(
                    versions: [
                        (profile: "hosted", version: "hosted-v1", providerProtocol: "open_ai", revoked: true, supported: true),
                        (profile: "private", version: "private-v1", providerProtocol: "self_hosted", revoked: false, supported: true),
                    ], defaultVersion: nil))
        }
        await model.confirmChange(try XCTUnwrap(model.change))
        settle()
        let row = try XCTUnwrap(rendered[PrivacySettingsView.unlistedJobIdentifier("job-1")])
        XCTAssertEqual(
            row, PrivacySettingsText.unlistedJob(PrivacyUnlistedJob(jobID: "job-1", itemID: "item-1", requestSent: false)))
        XCTAssertTrue(row.hasPrefix("Job job-1 · was queued just before the revocation and is no longer listed."), row)
        XCTAssertTrue(row.contains("retired it unless it ended in that moment"), row)
        let result = try XCTUnwrap(rendered[PrivacySettingsView.actionStateIdentifier])
        XCTAssertTrue(result.contains("1 job was pending just before and is no longer listed"), result)
        XCTAssertFalse(result.contains("job retired"), result)
        let offer = PrivacySettingsView.requeueOfferIdentifier("job-1", "private-v1")
        XCTAssertNotNil(rendered[offer], "the unlisted job can be moved on purpose")

        controls.recordResult = .failure(JobConfigurationError.unreadable)
        model.proposeRequeue(jobID: "job-1", toProfileVersion: "private-v1")
        await model.confirmRequeue(try XCTUnwrap(model.requeueProposal))
        settle()
        XCTAssertEqual(
            rendered[PrivacySettingsView.requeueRecordUnreadableIdentifier], PrivacySettingsText.requeueRecordUnreadableNotice)
        XCTAssertEqual(rendered[PrivacySettingsView.requeueRecordRetryIdentifier], PrivacySettingsText.requeueRecordRetry)

        controls.recordResult = .success(
            JobConfigurationRequeueRecord(
                oldJobID: "job-1", newJobID: "job-new", fromProfileVersion: "hosted-v1", toProfileVersion: "private-v1",
                requeuedAt: "2026-10-01T00:00:00Z"))
        await model.loadRequeueRecord()
        settle()
        XCTAssertNil(rendered[PrivacySettingsView.requeueRecordUnreadableIdentifier])
        XCTAssertEqual(
            rendered[PrivacySettingsView.requeueRecordIdentifier], PrivacySettingsText.requeueRecord(try XCTUnwrap(model.lastRequeue)))
    }

    func testACappedPendingListingShowsTheVersionWasNotRevokedAndKeepsItsRow() async throws {
        controls.pinned["hosted-v1"] = JobConfigurationPinnedJobs(
            jobs: [JobConfigurationPinnedJob(jobID: "job-1", itemID: "item-1", status: "queued")],
            total: PrivacySettingsModel.listedJobLimit + 1, truncated: true)
        await reload([grantedRoute()])
        XCTAssertEqual(
            rendered[PrivacySettingsView.queuedVersionIdentifier("hosted-v1")],
            "hosted · OpenAI · synthetic-model · 101 jobs (showing 1)")

        model.proposeRevokeProfile(profileVersion: "hosted-v1")
        await model.confirmChange(try XCTUnwrap(model.change))
        settle()
        XCTAssertTrue(controls.profileRevocations.isEmpty, "nothing was revoked")
        let result = try XCTUnwrap(rendered[PrivacySettingsView.actionStateIdentifier])
        XCTAssertTrue(result.contains("101 jobs pending"), result)
        XCTAssertTrue(result.contains("was not revoked"), result)
        XCTAssertNil(rendered[PrivacySettingsView.changeQuestionIdentifier])
        XCTAssertNil(rendered[PrivacySettingsView.unlistedJobIdentifier("job-1")], "nothing is shown as unlisted")
        XCTAssertEqual(rendered[PrivacySettingsView.queuedJobIdentifier("job-1")], "Job job-1 · queued")
        XCTAssertEqual(
            rendered[PrivacySettingsView.revokeProfileIdentifier("hosted-v1")]?.hasPrefix("Revoke hosted"), true,
            "the version is still unrevoked and its row stays")
    }
}
