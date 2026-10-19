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
}
