import SwiftUI
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Holds every request until released, then answers it with one valid reply, so a test controls when a late review
/// result arrives.
private final class PrivacyHeldReviewSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private let reply: Data
    private var released = false
    private var attempts = 0

    init(reply: Data) { self.reply = reply }

    var attemptCount: Int { lock.withLock { attempts } }

    func release() { lock.withLock { released = true } }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.withLock { attempts += 1 }
        while !lock.withLock({ released }) {
            if Task.isCancelled { throw ProviderTransportError.cancelled }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return ProviderHTTPResponse(status: 200, headers: ["content-type": "application/json"], body: reply)
    }
}

private final class PrivacyReviewRenderBox {
    var rendered: [String: String] = [:]
    var finished = false
}

/// Drives the processing-policy screen over the real core while a shadow review, which shares the pinned job queue with
/// interpretation, is in flight. The core's listing carries no job type and its item status describes only a note's
/// text sorting, so the screen must never present the note's sorting as the review's result.
final class PrivacySettingsReviewRealCoreTests: OptionalCapabilitiesTestCase {
    private var retainedWindow: UIWindow?

    override func tearDownWithError() throws {
        retainedWindow?.isHidden = true
        retainedWindow = nil
        try super.tearDownWithError()
    }

    /// Runs one main-actor step of the model to completion while the run loop delivers core events.
    @MainActor
    private func perform(_ work: @escaping @MainActor () async -> Void) -> Bool {
        let box = PrivacyReviewRenderBox()
        Task { @MainActor in
            await work()
            box.finished = true
        }
        return pump(until: { box.finished }, timeout: 30)
    }

    @MainActor
    private func settle() {
        retainedWindow?.rootViewController?.view.setNeedsLayout()
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.3))
        retainedWindow?.rootViewController?.view.layoutIfNeeded()
    }

    @MainActor
    func testARevokedReviewWhoseLateReplyArrivesIsNeverShownAsTheNotesSortingResult() throws {
        try prepareShadowReviewStore()
        // The note was sorted long before the review: its item status says processed whatever the review does.
        try runSQL(["UPDATE items SET processing_state = 'processed'"])
        let held = PrivacyHeldReviewSender(reply: try OptionalCapabilitiesReplies.review(itemType: "action"))
        defer { held.release() }
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: held, modules: [module], captureRouteID: ShadowReviewScenario.routeID)
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.shadowReview)

        let model = PrivacySettingsModel.assembled(root: root)
        let box = PrivacyReviewRenderBox()
        let hosting = UIHostingController(
            rootView: NavigationView { PrivacySettingsView(model: model, renderReporter: { box.rendered = $0 }) })
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        retainedWindow = window
        XCTAssertTrue(perform { await model.refresh() })
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isGranted(routeID: ShadowReviewScenario.routeID, capability: "review"))

        // The review request leaves the device and is held in flight.
        let jobID = try selectCase(service)
        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { held.attemptCount == 1 }, timeout: 30), "the review request was sent")
        XCTAssertEqual(JobStoreInspector.job(at: databasePath, id: jobID)?.status, "running")
        XCTAssertTrue(perform { await model.refresh() })
        XCTAssertTrue(
            model.queuedWork.flatMap(\.jobs.jobs).contains { $0.jobID == jobID && $0.status == "running" },
            "the screen lists the review in flight")

        // Take review away from the route on the screen while the request is in flight.
        model.proposeRevoke(routeID: ShadowReviewScenario.routeID, capability: "review")
        let change = try XCTUnwrap(model.change)
        XCTAssertTrue(perform { await model.confirmChange(change) })
        guard case .changed(let message) = model.actionState else {
            return XCTFail("expected changed, got \(String(describing: model.actionState))")
        }
        XCTAssertTrue(message.contains("A request already sent cannot be recalled"), message)
        XCTAssertFalse(model.isGranted(routeID: ShadowReviewScenario.routeID, capability: "review"))
        XCTAssertEqual(model.followedWork.map(\.jobID), [jobID], "the review in flight is followed")
        XCTAssertEqual(model.followedWork.first?.listing, .listed("running"))
        let authoritative = try ShadowReviewSeed.authoritativeSnapshot(path: databasePath)

        // The valid late reply arrives and the core settles the review.
        held.release()
        XCTAssertTrue(pump(until: { run != nil }, timeout: 30), "the review run ended")
        let settled = JobStoreInspector.job(at: databasePath, id: jobID)?.status
        XCTAssertFalse(["queued", "running"].contains(settled ?? ""), "the review left the queue: \(String(describing: settled))")
        XCTAssertEqual(
            try ShadowReviewSeed.authoritativeSnapshot(path: databasePath), authoritative,
            "the review changed no item, proposal, correction, grant, profile or capture")

        XCTAssertTrue(perform { await model.refresh() })
        settle()
        let followed = try XCTUnwrap(model.followedWork.first)
        XCTAssertEqual(followed.jobID, jobID)
        XCTAssertEqual(followed.listing, .notListed, "the job's own state is the core's listing: no longer pending")
        XCTAssertTrue(followed.requestSent)
        XCTAssertEqual(
            followed.note, .reported(PrivacyItemProcessing(processingState: "processed", processingJobStatus: nil)),
            "the note's sorting is read, and kept apart from the review")

        let row = try XCTUnwrap(box.rendered[PrivacySettingsView.followedJobIdentifier(jobID)], "the row is on screen")
        XCTAssertEqual(row, PrivacySettingsText.followedJob(followed))
        XCTAssertTrue(row.contains("no longer queued or running; its request had been sent"), row)
        XCTAssertTrue(row.contains("What this job itself returned is not shown here"), row)
        XCTAssertTrue(row.hasSuffix("The note's text sorting (which may be other work on it): sorted."), row)
        XCTAssertFalse(row.contains("reply was applied"), "the review applied nothing: \(row)")
        XCTAssertFalse(row.contains("was sorted while"), row)
        XCTAssertFalse(row.contains("refused"), row)
    }
}
