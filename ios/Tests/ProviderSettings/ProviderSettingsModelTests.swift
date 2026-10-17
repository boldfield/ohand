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
    var checkResult: Result<ProviderConnectionOutcome, Error> = .success(.ok)
    private(set) var checked: [String] = []
    /// Invoked in the middle of every call, so a test can change the session while it is in flight.
    var during: (() -> Void)?

    init(listing: ProviderProfileListAnswer) { self.listing = .success(listing) }

    func createProfile(_ draft: ProviderProfileDraft) async throws -> ProviderProfileCreatedAnswer {
        created.append(draft)
        during?()
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

    func listProfiles() async throws -> ProviderProfileListAnswer {
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

    func addCredential(_ secret: Data) throws -> String {
        added.append(secret)
        return try addResult.get()
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

    private func makeModel(_ controls: FakeProviderSettingsControls) -> ProviderSettingsModel {
        ProviderSettingsModel(controls: controls, credentials: credentials, readScope: { [unowned self] in scope })
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
        controls.during = { [unowned self] in scope = .captureOnly }

        await model.save()

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
}

private extension Result {
    var failureValue: Failure? {
        if case .failure(let failure) = self { return failure }
        return nil
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
            .failed(ProviderSettingsText.failure(controls.checkResult.failureValueForView!)))
        XCTAssertFalse(allVisibleText(model).contains("leak.example"))

        controls.checkResult = .success(.ok)
        controls.during = { [unowned self] in scope = .captureOnly }
        await model.checkConnection(version)
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.connectionStates.isEmpty)
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
