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
        XCTAssertEqual(rendered[PrivacySettingsView.stickyIdentifier], PrivacySettingsText.stickyNotice)
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

    func testChoosingAnOfferShowsTheQuestionWithAllowAndNotNowAndGrantsNothing() {
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: "hosted-v1")
        settle()
        let proposal = try! XCTUnwrap(model.proposal)
        XCTAssertEqual(rendered[PrivacySettingsView.questionIdentifier], PrivacySettingsText.proposalQuestion(proposal))
        XCTAssertEqual(rendered[PrivacySettingsView.confirmationNoteIdentifier], PrivacySettingsText.confirmationNote)
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
}
