import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

final class FakeProviderSettingsControls: ProviderSettingsControlling, @unchecked Sendable {
    var listing: Result<ProviderProfileListAnswer, Error>
    var createResult: Result<ProviderProfileCreatedAnswer, Error> = .success(
        decodeCreated(profileID: "p", version: "p-v1"))
    var setDefaultResult: Result<Void, Error> = .success(())
    var clearDefaultResult: Result<Void, Error> = .success(())
    private(set) var created: [ProviderProfileDraft] = []
    private(set) var defaultsSet: [String] = []
    private(set) var defaultsCleared = 0
    /// Awaited in the middle of a profile creation, so a test can act while the save is still pending.
    var suspendCreate: (() async -> Void)?
    var checkResult: Result<ProviderConnectionOutcome, Error> = .success(.ok)
    private(set) var checked: [String] = []
    /// Invoked in the middle of every call, so a test can change the session while it is in flight.
    var during: (() -> Void)?

    /// The route policy the privacy read reports; a failure leaves the answer unknown.
    var routePolicy: Result<[RoutePolicyRoute], Error> = .success([])

    init(listing: ProviderProfileListAnswer) { self.listing = .success(listing) }

    func listRoutePolicy() async throws -> [RoutePolicyRoute] { try routePolicy.get() }

    func createProfile(_ draft: ProviderProfileDraft) async throws -> ProviderProfileCreatedAnswer {
        created.append(draft)
        during?()
        await suspendCreate?()
        return try createResult.get()
    }

    func setDefault(capability: String, profileVersion: String) async throws {
        defaultsSet.append(profileVersion)
        during?()
        try setDefaultResult.get()
    }

    func clearDefault(capability: String) async throws {
        defaultsCleared += 1
        during?()
        try clearDefaultResult.get()
    }

    private(set) var listed = 0

    func listProfiles() async throws -> ProviderProfileListAnswer {
        listed += 1
        during?()
        return try listing.get()
    }

    func checkConnection(profileVersion: String) async throws -> ProviderConnectionOutcome {
        checked.append(profileVersion)
        during?()
        return try checkResult.get()
    }
}

/// Records what reached the credential service and answers from canned results. It keeps secrets only so a test can
/// prove where they went.
final class FakeProviderSettingsCredentials: ProviderSettingsCredentialControlling {
    var addResult: Result<String, Error> = .success("11111111-1111-1111-1111-111111111111")
    var updateResult: Result<Void, Error> = .success(())
    var deleteResult: Result<Void, Error> = .success(())
    var statusResult: Result<CredentialStatus, Error> = .success(.present)
    private(set) var added: [Data] = []
    private(set) var updated: [(Data, String)] = []
    private(set) var deleted: [String] = []

    /// The leases ended, in order.
    private(set) var released: [String] = []

    func addCredential(_ secret: Data) throws -> String {
        added.append(secret)
        return try addResult.get()
    }

    func releaseLease(reference: String) {
        released.append(reference)
    }

    func updateCredential(_ secret: Data, reference: String) throws {
        updated.append((secret, reference))
        try updateResult.get()
    }

    func deleteCredential(reference: String) throws {
        deleted.append(reference)
        try deleteResult.get()
    }

    func credentialStatus(reference: String) throws -> CredentialStatus { try statusResult.get() }

    var replaceByVersionResult: Result<Void, Error> = .success(())
    var removeByVersionResult: Result<Void, Error> = .success(())
    var statusByVersionResult: Result<CredentialStatus, Error> = .success(.present)
    private(set) var replacedByVersion: [(Data, String)] = []
    private(set) var removedByVersion: [String] = []
    private(set) var checkedByVersion: [String] = []
    /// Invoked in the middle of every by-version call, so a test can change the session while it is in flight.
    var duringByVersion: (() -> Void)?
    /// Awaited in the middle of a by-version replacement, so a test can hold the operation open.
    var suspendReplaceByVersion: (() async -> Void)?

    func replaceCredential(_ secret: Data, profileVersion: String) async throws {
        replacedByVersion.append((secret, profileVersion))
        duringByVersion?()
        await suspendReplaceByVersion?()
        try replaceByVersionResult.get()
    }

    func credentialStatus(profileVersion: String) async throws -> CredentialStatus {
        checkedByVersion.append(profileVersion)
        duringByVersion?()
        return try statusByVersionResult.get()
    }

    func removeCredential(profileVersion: String) async throws {
        removedByVersion.append(profileVersion)
        duringByVersion?()
        try removeByVersionResult.get()
    }
}

/// Records each sweep and what the screen had read by then.
final class FakeProviderSettingsSweep: ProviderSettingsCredentialSweeping {
    var outcome: ProviderCredentialSweepOutcome = .swept(deleted: 0, failed: 0)
    /// Invoked during every sweep, so a test can see what had happened before it.
    var during: (() -> Void)?
    private(set) var runs = 0

    func run() async -> ProviderCredentialSweepOutcome {
        runs += 1
        during?()
        return outcome
    }
}

private func decodeCreated(profileID: String, version: String) -> ProviderProfileCreatedAnswer {
    let json = #"{"operation_id":1,"profile_id":"\#(profileID)","profile_version":"\#(version)"}"#
    return try! JSONDecoder().decode(ProviderProfileCreatedAnswer.self, from: Data(json.utf8))
}

func providerListing(
    versions: [(profile: String, version: String, providerProtocol: String, revoked: Bool, supported: Bool)],
    defaultVersion: String?
) -> ProviderProfileListAnswer {
    providerListing(
        supportLevels: versions.map { ($0.profile, $0.version, $0.providerProtocol, $0.revoked, $0.supported ? "supported" : "unsupported") },
        defaultVersion: defaultVersion)
}

/// Like `providerListing`, with the support level the core declared: "supported", "unverified" or "unsupported".
func providerListing(
    supportLevels versions: [(profile: String, version: String, providerProtocol: String, revoked: Bool, support: String)],
    defaultVersion: String?
) -> ProviderProfileListAnswer {
    var profiles: [String: [String]] = [:]
    for entry in versions {
        let support = entry.support
        let json = #"{"profile_version":"\#(entry.version)","protocol":"\#(entry.providerProtocol)","#
            + #""endpoint":null,"model":"synthetic-model","capabilities":[{"capability":"text_interpretation","support":"\#(support)"}],"#
            + #""credential_bound":true,"revoked":\#(entry.revoked),"default_for":[],"created_at":"2026-10-01T00:00:00Z"}"#
        profiles[entry.profile, default: []].append(json)
    }
    let profileJSON = profiles.sorted { $0.key < $1.key }.map {
        #"{"profile_id":"\#($0.key)","versions":[\#($0.value.joined(separator: ","))]}"#
    }.joined(separator: ",")
    let defaults = defaultVersion.map {
        #"[{"capability":"text_interpretation","profile_version":"\#($0)"}]"#
    } ?? "[]"
    let json = #"{"operation_id":1,"profiles":[\#(profileJSON)],"defaults":\#(defaults)}"#
    return try! JSONDecoder().decode(ProviderProfileListAnswer.self, from: Data(json.utf8))
}

@MainActor
final class ProviderSettingsModelTests: XCTestCase {
    private var scope: CompositionReadScope = .authenticated

    private let credentials = FakeProviderSettingsCredentials()
    private let sweep = FakeProviderSettingsSweep()

    private func makeModel(_ controls: FakeProviderSettingsControls, offersOwnServer: Bool = true) -> ProviderSettingsModel {
        ProviderSettingsModel(
            controls: controls, credentials: credentials, sweep: sweep, readScope: { [unowned self] in scope },
            offersOwnServer: offersOwnServer, captureRouteID: "route-personal")
    }

    private static func routes(
        granting grants: [(capability: String, covered: [String])], routeID: String = "route-personal"
    ) -> [RoutePolicyRoute] {
        let granted = grants.map { grant in
            let covered = grant.covered.map { "\"\($0)\"" }.joined(separator: ",")
            return #"{"capability":"\#(grant.capability)","destination_classes":["cloud"],"covered_profile_versions":[\#(covered)]}"#
        }.joined(separator: ",")
        let json = #"[{"route_id":"\#(routeID)","route_name":"Local capture","scope":"personal","granted_capabilities":[\#(granted)]}]"#
        return try! JSONDecoder().decode([RoutePolicyRoute].self, from: Data(json.utf8))
    }

    private static let twoVersions = providerListing(
        versions: [
            ("hosted", "hosted-v1", "open_ai", false, true), ("private", "private-v1", "self_hosted", false, true),
            ("old", "old-v1", "anthropic", true, true), ("weak", "weak-v1", "anthropic", false, false),
        ], defaultVersion: nil)

    private func validDraft(_ model: ProviderSettingsModel) {
        model.draft = ProviderSettingsDraft(
            profileName: "work", providerProtocol: .openAI, endpoint: "", model: "synthetic-model",
            credentialReference: "cred-1", timeoutSeconds: 60)
    }

    func testNoDefaultIsShownAsLocalOnly() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        XCTAssertEqual(model.operatingMode, .localOnly)
        XCTAssertTrue(ProviderSettingsText.localOnlyNotice.contains("stay on this device"))
    }

    func testAPrivateServerDefaultIsDescribedAsWaitingOnTheDeviceWhenUnreachable() async {
        let listing = providerListing(
            versions: [("private", "private-v1", "self_hosted", false, true)], defaultVersion: "private-v1")
        let model = makeModel(FakeProviderSettingsControls(listing: listing))
        await model.refresh()
        XCTAssertEqual(model.operatingMode, .privateServer(model: "synthetic-model"))
        XCTAssertTrue(ProviderSettingsText.privateServerNotice.contains("wait on this device"))
        XCTAssertTrue(ProviderSettingsText.privateServerNotice.contains("not sent to another provider"))
    }

    func testIncompleteDraftsAreRefusedWithoutAskingTheCore() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()

        XCTAssertEqual(
            Set(model.issues), [.profileNameRequired, .modelRequired, .credentialRequired])
        await model.save()

        XCTAssertEqual(controls.created, [])
        XCTAssertNotNil(model.saveState?.message)
    }

    func testAFreshFormPreselectsAnthropicAndShowsNoProblemUntilEditedOrSaveIsPressed() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        XCTAssertEqual(model.draft.providerProtocol, .anthropic)
        XCTAssertFalse(model.issues.isEmpty)
        for field in [ProviderSettingsField.profileName, .model, .credential] {
            XCTAssertEqual(model.visibleIssues(for: field), [], "an untouched form shows nothing")
        }

        model.draft.profileName = " "
        XCTAssertEqual(model.visibleIssues(for: .profileName), [.profileNameRequired], "an edited field shows its own problem")
        XCTAssertEqual(model.visibleIssues(for: .model), [], "other fields stay quiet")

        await model.save()
        XCTAssertTrue(model.hasAttemptedSave)
        XCTAssertEqual(model.visibleIssues(for: .model), [.modelRequired])
        XCTAssertEqual(model.visibleIssues(for: .credential), [.credentialRequired])

        model.resetDraft()
        XCTAssertEqual(model.visibleIssues(for: .model), [], "abandoning the form starts the next one untouched")
    }

    func testTheOwnServerTypeIsNotOfferedAndASavedOneCannotBecomeTheDefault() async {
        XCTAssertFalse(ProviderSettingsModel.ownServerOffered, "this build does not offer a private server")
        XCTAssertEqual(ProviderSettingsProtocol.offered(offersOwnServer: false), [.openAI, .anthropic])
        XCTAssertEqual(ProviderSettingsProtocol.offered(offersOwnServer: true), ProviderSettingsProtocol.allCases)

        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls, offersOwnServer: false)
        await model.refresh()
        let privateServer = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "private-v1" }!
        XCTAssertTrue(model.isUnofferedOwnServer(privateServer))
        XCTAssertFalse(model.canMakeDefault(privateServer))
        XCTAssertNil(model.candidateVersion(for: .ownServer))
        await model.makeDefault(privateServer)
        XCTAssertEqual(controls.defaultsSet, [])
        XCTAssertTrue(model.canMakeDefault(model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!))
    }

    func testTheAllowanceLineIsNeededOnlyForACloudDefaultThatIsNotYetAllowed() async {
        let listing = providerListing(
            versions: [("hosted", "hosted-v1", "anthropic", false, true)], defaultVersion: "hosted-v1")
        let controls = FakeProviderSettingsControls(listing: listing)
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "no grant yet")

        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", ["hosted-v1"])]))
        await model.refresh()
        XCTAssertFalse(model.needsPrivacyAllowance, "the line goes once the grant covers the default's version")

        controls.routePolicy = .success(Self.routes(granting: [("review", ["hosted-v1"])]))
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "another capability does not allow interpretation")

        controls.routePolicy = .failure(ProviderConfigurationError.unreadable)
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "an unreadable policy keeps the prompt")

        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", ["hosted-v1"])], routeID: "route-other"))
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "a grant on a different route does not allow this capture route")

        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", [])]))
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "a grant that covers no version does not allow the default")

        let unknownRoute = ProviderSettingsModel(
            controls: controls, credentials: credentials, sweep: sweep, readScope: { [unowned self] in scope })
        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", ["hosted-v1"])]))
        await unknownRoute.refresh()
        XCTAssertTrue(unknownRoute.needsPrivacyAllowance, "without a known capture route nothing is proven")

        let none = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await none.refresh()
        XCTAssertFalse(none.needsPrivacyAllowance, "no default, nothing to allow")
    }

    func testAGrantCoveringAnotherProvidersVersionDoesNotClearTheLine() async {
        let mixed = providerListing(
            versions: [("openai", "openai-v1", "open_ai", false, true), ("hosted", "hosted-v1", "anthropic", false, true)],
            defaultVersion: "hosted-v1")
        let controls = FakeProviderSettingsControls(listing: mixed)
        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", ["openai-v1"])]))
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "an OpenAI-only grant does not cover the Anthropic default")

        controls.routePolicy = .success(Self.routes(granting: [("text_interpretation", ["hosted-v1", "openai-v1"])]))
        await model.refresh()
        XCTAssertFalse(model.needsPrivacyAllowance, "with both providers stored, covering the default clears the line")

        let legacy = #"[{"route_id":"route-personal","route_name":"Local capture","scope":"personal","granted_capabilities":[{"capability":"text_interpretation","destination_classes":["cloud"]}]}]"#
        controls.routePolicy = .success(try! JSONDecoder().decode([RoutePolicyRoute].self, from: Data(legacy.utf8)))
        await model.refresh()
        XCTAssertTrue(model.needsPrivacyAllowance, "a listing without per-version coverage covers nothing")
    }

    func testUnsupportedCombinationsAreRefused() {
        var draft = ProviderSettingsDraft(
            profileName: "a", providerProtocol: .selfHosted, endpoint: "", model: "m", credentialReference: "c")
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [.endpointRequired])
        draft.endpoint = "http://server.example"
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [.endpointNotHTTPS])
        draft.endpoint = "https://server.example/v1?key=1"
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [.endpointHasQueryOrFragment])
        draft.endpoint = "https://server.example/v1"
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [])
        draft.providerProtocol = .anthropic
        XCTAssertEqual(draft.endpoint, "", "switching to a hosted provider drops the hidden address")
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [])
        draft.endpoint = "https://server.example/v1"
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [.endpointNotAllowed])
        draft.endpoint = ""
        draft.timeoutSeconds = 301
        XCTAssertEqual(ProviderSettingsModel.issues(for: draft), [.timeoutOutOfBounds])
    }

    func testAValidDraftIsSentWithTheTextInterpretationCapabilityOnlyAndDoesNotBecomeDefault() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        validDraft(model)

        await model.save()

        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(controls.created.first?.capabilities, ["text_interpretation"])
        XCTAssertEqual(controls.created.first?.providerProtocol, "open_ai")
        XCTAssertNil(controls.created.first?.endpoint)
        XCTAssertEqual(model.saveState, .saved)
        XCTAssertEqual(controls.defaultsSet, [], "saving a version never changes the default")
        XCTAssertEqual(model.draft, ProviderSettingsDraft(), "the form is cleared after a save")
    }

    func testACoreRefusalIsReportedAndNothingIsClaimedSaved() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.createResult = .failure(
            ProviderConfigurationError.core(
                CoreFailure(errorClass: .permanent, code: "endpoint_invalid", message: "synthetic")))
        let model = makeModel(controls)
        await model.refresh()
        validDraft(model)

        await model.save()

        XCTAssertEqual(
            model.saveState, .failed(ProviderSettingsText.failure(controls.createResult.failureValue!)))
        XCTAssertNotEqual(model.saveState, .saved)
        XCTAssertEqual(model.draft.profileName, "work", "the form is kept so it can be corrected")
    }

    func testOnlyUnrevokedSupportedNonDefaultVersionsOfferDefaultChange() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        let versions = model.listing!.profiles.flatMap(\.versions)
        let offered = versions.filter { model.canMakeDefault($0) }.map(\.profileVersion)
        XCTAssertEqual(Set(offered), ["hosted-v1", "private-v1"])
    }

    func testAnUnverifiedPrivateServerIsUnavailableAndCannotBeMadeDefault() async {
        let listing = providerListing(
            supportLevels: [
                ("private", "private-v1", "self_hosted", false, "unverified"),
                ("hosted", "hosted-v1", "open_ai", false, "supported"),
            ], defaultVersion: nil)
        let controls = FakeProviderSettingsControls(listing: listing)
        let model = makeModel(controls)
        await model.refresh()
        let versions = model.listing!.profiles.flatMap(\.versions)
        let unverified = versions.first { $0.profileVersion == "private-v1" }!
        let supported = versions.first { $0.profileVersion == "hosted-v1" }!

        XCTAssertEqual(model.availability(of: unverified), .unverified)
        XCTAssertEqual(model.availability(of: supported), .ready)
        XCTAssertFalse(model.canMakeDefault(unverified))
        XCTAssertTrue(model.canMakeDefault(supported))

        await model.makeDefault(unverified)

        XCTAssertEqual(controls.defaultsSet, [], "an unverified version is never sent to the core as a default")
        XCTAssertNotNil(model.defaultState?.message)
        XCTAssertTrue(ProviderSettingsText.capabilityStatus("text_interpretation", support: "unverified").contains("not verified"))
        XCTAssertTrue(ProviderSettingsText.capabilityStatus("text_interpretation", support: "unverified").contains("will not process notes"))
    }

    func testAnUnverifiedDefaultAlreadyStoredIsReportedAsUnableToProcess() async {
        let listing = providerListing(
            supportLevels: [("private", "private-v1", "self_hosted", false, "unverified")], defaultVersion: "private-v1")
        let model = makeModel(FakeProviderSettingsControls(listing: listing))
        await model.refresh()
        XCTAssertEqual(model.defaultAvailability, .unverified)
        XCTAssertTrue(ProviderSettingsText.unavailableDefaultNotice.contains("cannot process notes"))
    }

    func testChangingTheDefaultCallsOnlyTheProfileServiceAndWarnsThatWaitingWorkIsPinned() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        let version = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!

        await model.makeDefault(version)

        XCTAssertEqual(controls.defaultsSet, ["hosted-v1"])
        XCTAssertEqual(model.defaultState, .defaultChanged)
        XCTAssertTrue(ProviderSettingsText.defaultChangeNote.contains("keeps the provider version"))
        XCTAssertTrue(ProviderSettingsText.defaultChangeNote.contains("saved captures are never changed"))
    }

    func testARevokedVersionCannotBeMadeDefault() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        let revoked = model.listing!.profiles.flatMap(\.versions).first { $0.revoked }!

        await model.makeDefault(revoked)

        XCTAssertEqual(controls.defaultsSet, [])
        XCTAssertNotNil(model.defaultState?.message)
    }

    func testUseLocalOnlyClearsTheDefault() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        await model.useLocalOnly()
        XCTAssertEqual(controls.defaultsCleared, 1)
        XCTAssertEqual(model.defaultState, .defaultChanged)
    }

    func testARelockWhileASaveIsInFlightEmptiesTheScreen() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        validDraft(model)
        controls.suspendCreate = { [unowned self] in scope = .captureOnly }

        await model.save()

        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.listing)
        XCTAssertEqual(model.draft, ProviderSettingsDraft())
    }

    func testACaptureOnlySessionNeverAsksTheCore() async {
        scope = .captureOnly
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        await model.useLocalOnly()
        XCTAssertEqual(controls.defaultsCleared, 0)
    }

    // MARK: credentials and connection checks

    private static let syntheticSecret = Data("synthetic-secret-value".utf8)

    private func allVisibleText(_ model: ProviderSettingsModel) -> String {
        [
            model.credentialState?.message ?? "", model.draft.credentialReference,
            model.connectionStates.values.map(\.message).joined(separator: " "),
        ].joined(separator: " ")
    }

    func testANewSecretGoesToTheCredentialServiceAndOnlyItsReferenceIsKept() {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        model.storeCredential(Self.syntheticSecret)

        XCTAssertEqual(credentials.added, [Self.syntheticSecret])
        XCTAssertEqual(model.draft.credentialReference, "11111111-1111-1111-1111-111111111111")
        XCTAssertEqual(model.credentialState, .stored)
        XCTAssertFalse(allVisibleText(model).contains("synthetic-secret-value"))
    }

    func testANamedCredentialIsReplacedInPlaceAndRemovalForgetsTheReference() {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        model.draft.credentialReference = "22222222-2222-2222-2222-222222222222"
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(credentials.updated.map(\.1), ["22222222-2222-2222-2222-222222222222"])
        XCTAssertEqual(model.credentialState, .updated)

        model.removeCredential()
        XCTAssertEqual(credentials.deleted, ["22222222-2222-2222-2222-222222222222"])
        XCTAssertEqual(model.draft.credentialReference, "")
        XCTAssertEqual(model.credentialState, .removed)
    }

    func testCredentialFailuresAreFixedSentencesThatEchoNothing() {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        model.draft.credentialReference = "secret-looking-reference"
        let failures: [CredentialError] = [
            .invalidReference, .emptySecret, .notFound, .invalidated, .deviceLocked(.update), .storage(.update, -25299),
        ]
        for failure in failures {
            credentials.updateResult = .failure(failure)
            model.storeCredential(Self.syntheticSecret)
            guard case .failed(let message) = model.credentialState else { return XCTFail("\(failure) is reported") }
            XCTAssertFalse(message.contains("secret-looking-reference"))
            XCTAssertFalse(message.contains("25299"))
            XCTAssertFalse(message.contains("synthetic-secret-value"))
        }
    }

    func testCredentialStatusReportsPresentAbsentAndInvalidatedWithoutReadingASecret() {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        model.checkCredential()
        XCTAssertEqual(model.credentialState, .failed(ProviderSettingsText.credentialMissingReference))

        model.draft.credentialReference = "33333333-3333-3333-3333-333333333333"
        for status in [CredentialStatus.present, .absent, .invalidated] {
            credentials.statusResult = .success(status)
            model.checkCredential()
            XCTAssertEqual(model.credentialState, .status(status))
        }
    }

    func testAnEmptySecretIsRefusedBeforeTheCredentialServiceIsAsked() {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        model.noteEmptySecret()
        XCTAssertEqual(model.credentialState, .failed(ProviderSettingsText.credentialEmptySecret))
        XCTAssertEqual(credentials.added.count, 0)
    }

    func testACapturelessSessionCannotTouchCredentials() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        scope = .captureOnly
        model.storeCredential(Self.syntheticSecret)
        model.removeCredential()
        XCTAssertEqual(credentials.added.count, 0)
        XCTAssertEqual(credentials.deleted, [])
        XCTAssertEqual(model.phase, .locked)
    }

    func testAnAuthenticationLapseInsideTheCredentialServiceLocksTheScreen() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        credentials.addResult = .failure(ProviderConfigurationError.notAuthenticated)
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.credentialState)
    }

    func testEveryConnectionOutcomeHasAFixedRedactedSentence() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        let version = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!
        let outcomes: [ProviderConnectionOutcome] = [
            .ok, .credentialMissing, .credentialRejected, .unreachable, .tlsFailure, .timeout, .capabilityMismatch,
            .protocolNotEnabled, .rateLimited, .providerError, .unexpectedResponse, .cancelled,
        ]
        var sentences = Set<String>()
        for outcome in outcomes {
            controls.checkResult = .success(outcome)
            await model.checkConnection(version)
            XCTAssertEqual(model.connectionStates["hosted-v1"], .finished(outcome))
            let message = model.connectionStates["hosted-v1"]!.message
            sentences.insert(message)
            XCTAssertFalse(message.contains("https"), message)
        }
        XCTAssertEqual(sentences.count, outcomes.count, "each outcome reads differently")
        XCTAssertEqual(controls.checked, Array(repeating: "hosted-v1", count: outcomes.count))
    }

    func testAConnectionCheckRunsOnlyOnRequestAndNeverForARevokedVersion() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertEqual(controls.checked, [], "loading the screen sends nothing")
        let revoked = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "old-v1" }!
        XCTAssertFalse(model.canCheckConnection(revoked))
        await model.checkConnection(revoked)
        XCTAssertEqual(controls.checked, [])
        XCTAssertNil(model.connectionStates["old-v1"])
    }

    func testAFailedCheckIsRedactedAndALockDuringItShowsNothing() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        let version = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!
        controls.checkResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "profile_revoked", message: "https://leak.example")))
        await model.checkConnection(version)
        XCTAssertEqual(
            model.connectionStates["hosted-v1"],
            .failed(ProviderSettingsText.failure(controls.checkResult.failureValue!)))
        XCTAssertFalse(allVisibleText(model).contains("leak.example"))

        controls.checkResult = .success(.ok)
        controls.during = { [unowned self] in scope = .captureOnly }
        await model.checkConnection(version)
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.connectionStates.isEmpty)
    }

    func testARelockDuringACheckThatThenFailsEmptiesTheScreenInsteadOfReporting() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        let version = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!
        controls.checkResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .transient, code: "sender_busy", message: "")))
        controls.during = { [unowned self] in scope = .captureOnly }

        await model.checkConnection(version)

        XCTAssertEqual(controls.checked, ["hosted-v1"])
        XCTAssertEqual(model.phase, .locked, "a failure that arrives after a relock locks rather than reports")
        XCTAssertTrue(model.connectionStates.isEmpty)
        XCTAssertNil(model.listing)
        XCTAssertEqual(model.draft, ProviderSettingsDraft())
    }

    // MARK: unsaved secrets are not orphaned

    private func storedModel() async -> ProviderSettingsModel {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        return model
    }

    func testASecretStoredForADraftThatIsAbandonedIsDeleted() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(credentials.deleted, [], "while the draft is open the secret is kept")
        model.clear()
        XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
    }

    func testEditingTheReferenceAwayFromTheStoredSecretDeletesIt() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        model.draft.credentialReference = "another-name"
        XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
        model.clear()
        XCTAssertEqual(credentials.deleted.count, 1, "nothing is deleted twice or for a name this screen did not add")
    }

    func testASavedProfileKeepsItsSecretWhenTheScreenIsLeft() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "work"
        model.draft.model = "synthetic-model"
        await model.save()
        XCTAssertEqual(model.saveState, .saved)
        model.clear()
        XCTAssertEqual(credentials.deleted, [], "a secret a saved version names is not deleted")
    }

    func testARefusedSaveStillLeavesTheSecretToBeDeletedWhenTheDraftIsAbandoned() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "model_invalid", message: "")))
        let model = makeModel(controls)
        await model.refresh()
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "work"
        model.draft.model = "synthetic-model"
        await model.save()
        XCTAssertNotEqual(model.saveState, .saved)
        XCTAssertEqual(credentials.deleted, [])
        model.clear()
        XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
    }

    func testRemovingTheDraftSecretDoesNotDeleteItAgainOnLeaving() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        model.removeCredential()
        model.clear()
        XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
    }

    func testAnAbandonedSecretWhoseDeletionFailsIsReleasedForTheSweep() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        scope = .captureOnly
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertEqual(credentials.deleted, [Self.reference], "deleted at once, best effort")
        XCTAssertEqual(credentials.released, [Self.reference], "and left for the next sweep when that fails")
    }

    private static let reference = "11111111-1111-1111-1111-111111111111"

    private func storeAndNameSecret(_ model: ProviderSettingsModel) {
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "work"
        model.draft.model = "synthetic-model"
    }

    func testACommittedSaveReleasesTheLeaseOfTheSecretItNames() async {
        let model = await storedModel()
        storeAndNameSecret(model)
        XCTAssertEqual(credentials.released, [], "leased while the draft is open")
        await model.save()
        XCTAssertEqual(model.saveState, .saved)
        XCTAssertEqual(credentials.released, [Self.reference])
        XCTAssertEqual(credentials.deleted, [])
    }

    func testARefusedSaveKeepsTheLeaseWhileTheDraftStillNamesTheSecret() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "model_invalid", message: "")))
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        await model.save()
        XCTAssertEqual(credentials.released, [], "nothing committed, and the draft still owns it")
        XCTAssertEqual(credentials.deleted, [])
    }

    func testLeavingTheScreenWhileASaveIsInFlightKeepsTheSecretTheSavedVersionNames() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        controls.suspendCreate = { model.clear() }

        await model.save()

        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(credentials.deleted, [], "the profile the core committed still has its secret")
        XCTAssertEqual(credentials.released, [Self.reference], "released once committed")
    }

    func testARelockDuringASaveThenAReloadNeverDeletesTheSavedVersionsSecret() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        controls.suspendCreate = { [unowned self] in scope = .captureOnly }

        await model.save()
        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(model.phase, .locked)

        scope = .authenticated
        await model.refresh()
        XCTAssertEqual(credentials.deleted, [], "neither the lock nor the reload touches the saved secret")
        XCTAssertEqual(credentials.released, [Self.reference])
    }

    func testARefusedSaveAfterTheScreenWasLeftDeletesTheUnsavedSecret() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "model_invalid", message: "")))
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        controls.suspendCreate = { model.clear() }

        await model.save()

        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(credentials.deleted, [Self.reference])
        XCTAssertEqual(credentials.released, [], "deleted, so no lease is left to end")
    }

    /// Failures the client reports itself after submitting: the core may have committed the version all the same.
    private static let unknownSaveOutcomes: [(String, Error)] = [
        ("unreadable_event", ProviderConfigurationError.core(ProviderConfigurationClientFailure.unreadableAnswer)),
        ("client_shut_down", ProviderConfigurationError.core(ProviderConfigurationClientFailure.shutDown)),
        ("unreadable", ProviderConfigurationError.unreadable),
    ]

    func testASaveWithAnUnknownOutcomeNeverDeletesTheSecretAndLeavesItToTheSweep() async {
        for (name, failure) in Self.unknownSaveOutcomes {
            let credentials = FakeProviderSettingsCredentials()
            let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
            controls.createResult = .failure(failure)
            let model = ProviderSettingsModel(
                controls: controls, credentials: credentials, sweep: sweep, readScope: { [unowned self] in scope })
            await model.refresh()
            model.storeCredential(Self.syntheticSecret)
            model.draft.profileName = "work"
            model.draft.model = "synthetic-model"

            await model.save()

            XCTAssertEqual(controls.created.count, 1, name)
            XCTAssertEqual(credentials.deleted, [], "\(name): a version the core may have committed keeps its secret")
            XCTAssertEqual(credentials.released, [Self.reference], "\(name): the lease ends so the sweep can decide")
            XCTAssertEqual(model.draft.credentialReference, "", "\(name): the draft no longer names it")
            XCTAssertNotEqual(model.saveState, .saved, name)

            model.clear()
            XCTAssertEqual(credentials.deleted, [], "\(name): leaving the screen does not delete it either")
        }
    }

    func testLeavingTheScreenOrRelockingDuringASaveWithAnUnknownOutcomeNeverDeletesTheSecret() async {
        let interruptions: [(String, @MainActor (ProviderSettingsModel) -> Void)] = [
            ("left", { $0.clear() }),
            ("relocked", { [unowned self] _ in scope = .captureOnly }),
        ]
        for (name, failure) in Self.unknownSaveOutcomes {
            for (interruption, interrupt) in interruptions {
                scope = .authenticated
                let label = "\(name) after the screen was \(interruption)"
                let credentials = FakeProviderSettingsCredentials()
                let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
                controls.createResult = .failure(failure)
                let model = ProviderSettingsModel(
                    controls: controls, credentials: credentials, sweep: sweep, readScope: { [unowned self] in scope })
                await model.refresh()
                model.storeCredential(Self.syntheticSecret)
                model.draft.profileName = "work"
                model.draft.model = "synthetic-model"
                controls.suspendCreate = { interrupt(model) }

                await model.save()

                XCTAssertEqual(controls.created.count, 1, label)
                XCTAssertEqual(credentials.deleted, [], "\(label): the secret is not deleted")
                XCTAssertEqual(credentials.released, [Self.reference], "\(label): its lease ends for the sweep")
                XCTAssertNil(model.saveState, "\(label): no failure is shown on a screen that was left or locked")
                XCTAssertNil(model.listing, label)
                XCTAssertEqual(model.draft, ProviderSettingsDraft(), label)
                if interruption == "relocked" {
                    XCTAssertEqual(model.phase, .locked, "\(label): the relock is honored on the failure path too")
                }

                scope = .authenticated
                await model.refresh()
                XCTAssertEqual(credentials.deleted, [], "\(label): a reload deletes nothing itself")
            }
        }
    }

    func testARelockDuringASaveTheCoreThenRefusesEmptiesTheScreenAndDeletesTheUnsavedSecret() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.createResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "model_invalid", message: "")))
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        controls.suspendCreate = { [unowned self] in scope = .captureOnly }

        await model.save()

        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(model.phase, .locked, "a refusal that arrives after a relock locks rather than reports")
        XCTAssertNil(model.saveState)
        XCTAssertNil(model.listing)
        XCTAssertEqual(model.draft, ProviderSettingsDraft())
        XCTAssertEqual(
            credentials.deleted, [Self.reference], "nothing saved it and the draft is gone, so it is deleted")
        XCTAssertEqual(credentials.released, [])
    }

    func testARelockDuringAFailingReloadLocksInsteadOfReportingTheFailure() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        controls.listing = .failure(ProviderConfigurationError.unreadable)
        controls.during = { [unowned self] in scope = .captureOnly }

        await model.refresh()

        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.listing, "the earlier listing is not kept behind a failure notice")
    }

    func testARelockDuringAFailingDefaultChangeEmptiesTheScreen() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        controls.setDefaultResult = .failure(
            ProviderConfigurationError.core(CoreFailure(errorClass: .transient, code: "storage_busy", message: "")))
        let model = makeModel(controls)
        await model.refresh()
        let version = model.listing!.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" }!
        controls.during = { [unowned self] in scope = .captureOnly }

        await model.makeDefault(version)

        XCTAssertEqual(controls.defaultsSet, ["hosted-v1"])
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.defaultState)
        XCTAssertNil(model.listing)
    }

    func testOnlyAFailureTheCoreIssuedOrARefusalBeforeSubmissionIsADefiniteRefusal() {
        XCTAssertTrue(ProviderConfigurationError.isDefiniteRefusal(ProviderConfigurationError.notAuthenticated))
        XCTAssertTrue(ProviderConfigurationError.isDefiniteRefusal(ProviderConfigurationError.invalidRequest))
        XCTAssertTrue(
            ProviderConfigurationError.isDefiniteRefusal(
                ProviderConfigurationError.core(CoreFailure(errorClass: .permanent, code: "model_invalid", message: ""))))
        for (name, failure) in Self.unknownSaveOutcomes {
            XCTAssertFalse(ProviderConfigurationError.isDefiniteRefusal(failure), name)
        }
        XCTAssertFalse(ProviderConfigurationError.isDefiniteRefusal(CoreFailure.handleClosed), "an unknown error type")
    }

    func testTheSweepRunsWhenTheScreenLoadsBeforeTheListAndBeforeAnySaveIsAccepted() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        var listedBeforeSweep: [Int] = []
        sweep.during = { listedBeforeSweep.append(controls.listed) }
        validDraft(model)
        await model.save()
        XCTAssertEqual(controls.created, [], "no save is accepted before the screen has loaded")
        XCTAssertEqual(sweep.runs, 0)

        await model.refresh()
        XCTAssertEqual(sweep.runs, 1)
        XCTAssertEqual(listedBeforeSweep, [0], "the sweep runs before the list is read")
        await model.refresh()
        XCTAssertEqual(sweep.runs, 1, "a refresh of a loaded screen does not sweep again")

        model.clear()
        await model.refresh()
        XCTAssertEqual(sweep.runs, 2, "each load sweeps")
        validDraft(model)
        await model.save()
        XCTAssertEqual(controls.created.count, 1)
    }

    func testARelockDuringTheSweepShowsNothing() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        sweep.during = { [unowned self] in scope = .captureOnly }
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertNil(model.listing)
    }

    func testASweepNeverDeletesTheSecretOfTheDraftBeingEdited() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        model.clear()
        XCTAssertEqual(credentials.deleted, [Self.reference])
        model.storeCredential(Self.syntheticSecret)
        await model.refresh()
        XCTAssertEqual(credentials.deleted, [Self.reference], "the draft's own secret stays leased through a reload")
        XCTAssertEqual(credentials.released, [])
    }

    // MARK: secrets of stored versions

    private func hostedVersion(_ model: ProviderSettingsModel) throws -> ProviderProfileListAnswer.Version {
        try XCTUnwrap(model.listing?.profiles.flatMap(\.versions).first { $0.profileVersion == "hosted-v1" })
    }

    func testAStoredVersionsSecretIsReplacedCheckedAndRemovedByVersionWithoutAReference() async throws {
        let model = await storedModel()
        let hosted = try hostedVersion(model)

        await model.replaceSavedCredential(Self.syntheticSecret, for: hosted)
        XCTAssertEqual(credentials.replacedByVersion.map(\.1), ["hosted-v1"])
        XCTAssertEqual(credentials.replacedByVersion.map(\.0), [Self.syntheticSecret])
        XCTAssertEqual(model.savedCredentialStates["hosted-v1"], .updated)
        XCTAssertEqual(credentials.updated.count + credentials.added.count, 0, "no new credential is created")

        for status in [CredentialStatus.present, .absent, .invalidated] {
            credentials.statusByVersionResult = .success(status)
            await model.checkSavedCredential(hosted)
            XCTAssertEqual(model.savedCredentialStates["hosted-v1"], .status(status))
        }

        await model.removeSavedCredential(hosted)
        XCTAssertEqual(credentials.removedByVersion, ["hosted-v1"])
        XCTAssertEqual(model.savedCredentialStates["hosted-v1"], .removed)
        XCTAssertNil(model.savedCredentialStates["private-v1"], "another version is untouched")
        XCTAssertEqual(credentials.deleted, [], "removal goes through the version, not a typed name")
    }

    func testAStoredVersionsSecretWorksAfterTheDraftWasSavedAndReset() async throws {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "work"
        model.draft.model = "synthetic-model"
        await model.save()
        XCTAssertEqual(model.draft.credentialReference, "", "the draft no longer names the saved secret")

        let hosted = try hostedVersion(model)
        await model.replaceSavedCredential(Self.syntheticSecret, for: hosted)
        await model.removeSavedCredential(hosted)
        XCTAssertEqual(credentials.replacedByVersion.count, 1)
        XCTAssertEqual(credentials.removedByVersion, ["hosted-v1"])
        XCTAssertEqual(credentials.added.count, 1, "replacing did not add another credential")
    }

    func testStoredVersionCredentialFailuresAreRedacted() async throws {
        let model = await storedModel()
        let hosted = try hostedVersion(model)
        let failures: [Error] = [
            CredentialError.invalidated, CredentialError.deviceLocked(.update), CredentialError.storage(.update, -25299),
            ProviderConfigurationError.core(
                CoreFailure(errorClass: .permanent, code: "credential_not_bound", message: "secret-looking-detail")),
            ProviderConfigurationError.core(
                CoreFailure(errorClass: .permanent, code: "profile_not_found", message: "secret-looking-detail")),
        ]
        for failure in failures {
            credentials.replaceByVersionResult = .failure(failure)
            await model.replaceSavedCredential(Self.syntheticSecret, for: hosted)
            guard case .failed(let message)? = model.savedCredentialStates["hosted-v1"] else {
                return XCTFail("\(failure) is reported")
            }
            XCTAssertFalse(message.isEmpty)
            XCTAssertFalse(message.contains("25299"))
            XCTAssertFalse(message.contains("secret-looking-detail"))
            XCTAssertFalse(message.contains("synthetic-secret-value"))
        }
    }

    func testALockDuringAStoredVersionActionEmptiesTheScreen() async throws {
        let model = await storedModel()
        let hosted = try hostedVersion(model)
        credentials.duringByVersion = { [unowned self] in scope = .captureOnly }
        await model.replaceSavedCredential(Self.syntheticSecret, for: hosted)
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.savedCredentialStates.isEmpty)
        XCTAssertNil(model.listing)
    }

    func testARelockDuringAStoredVersionActionThatThenFailsEmptiesTheScreen() async throws {
        let actions: [(String, @MainActor (ProviderSettingsModel, ProviderProfileListAnswer.Version) async -> Void)] = [
            ("replace", { await $0.replaceSavedCredential(Self.syntheticSecret, for: $1) }),
            ("check", { await $0.checkSavedCredential($1) }),
            ("remove", { await $0.removeSavedCredential($1) }),
        ]
        for (name, act) in actions {
            scope = .authenticated
            let credentials = FakeProviderSettingsCredentials()
            credentials.replaceByVersionResult = .failure(CredentialError.storage(.update, -25299))
            credentials.statusByVersionResult = .failure(CredentialError.storage(.status, -25299))
            credentials.removeByVersionResult = .failure(CredentialError.storage(.delete, -25299))
            credentials.duringByVersion = { [unowned self] in scope = .captureOnly }
            let model = ProviderSettingsModel(
                controls: FakeProviderSettingsControls(listing: Self.twoVersions), credentials: credentials,
                sweep: sweep, readScope: { [unowned self] in scope })
            await model.refresh()
            let hosted = try hostedVersion(model)

            await act(model, hosted)

            XCTAssertEqual(
                model.phase, .locked, "\(name): a failure that arrives after a relock locks rather than reports")
            XCTAssertTrue(model.savedCredentialStates.isEmpty, name)
            XCTAssertNil(model.listing, name)
            XCTAssertEqual(model.draft, ProviderSettingsDraft(), name)
        }
    }

    func testACapturelessSessionNeverReachesTheStoredVersionOperations() async throws {
        let model = await storedModel()
        let hosted = try hostedVersion(model)
        scope = .captureOnly
        await model.replaceSavedCredential(Self.syntheticSecret, for: hosted)
        await model.checkSavedCredential(hosted)
        await model.removeSavedCredential(hosted)
        XCTAssertTrue(credentials.replacedByVersion.isEmpty)
        XCTAssertTrue(credentials.checkedByVersion.isEmpty)
        XCTAssertTrue(credentials.removedByVersion.isEmpty)
    }

    func testARejectedSecretIsReportedForTheDraftOrTheVersionWithoutCallingTheService() async throws {
        let model = await storedModel()
        let hosted = try hostedVersion(model)
        model.noteRejectedSecret(.surroundingWhitespace)
        XCTAssertEqual(model.credentialState, .failed(ProviderSettingsText.rejectedSecret(.surroundingWhitespace)))
        model.noteRejectedSecret(.empty, for: hosted)
        XCTAssertEqual(model.savedCredentialStates["hosted-v1"], .failed(ProviderSettingsText.credentialEmptySecret))
        XCTAssertEqual(credentials.added.count + credentials.replacedByVersion.count, 0)
    }

    func testClearingTheScreenDropsCredentialAndCheckFeedback() async {
        let model = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await model.refresh()
        model.storeCredential(Self.syntheticSecret)
        model.clear()
        XCTAssertNil(model.credentialState)
        XCTAssertEqual(model.draft.credentialReference, "")
        XCTAssertTrue(model.connectionStates.isEmpty)
    }
}

private extension Result {
    var failureValue: Failure? {
        if case .failure(let failure) = self { return failure }
        return nil
    }
}
