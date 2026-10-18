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

    init(listing: ProviderProfileListAnswer) { self.listing = .success(listing) }

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
    /// The references handed out for new credentials, in order; the last one repeats.
    var newReferences = ["11111111-1111-1111-1111-111111111111"]
    var addResult: Result<Void, Error> = .success(())
    var updateResult: Result<Void, Error> = .success(())
    var deleteResult: Result<Void, Error> = .success(())
    var statusResult: Result<CredentialStatus, Error> = .success(.present)
    private(set) var added: [Data] = []
    private(set) var updated: [(Data, String)] = []
    private(set) var deleted: [String] = []

    private(set) var addedReferences: [String] = []

    func newCredentialReference() -> String {
        newReferences.count > 1 ? newReferences.removeFirst() : newReferences[0]
    }

    /// Invoked as a new credential is written, so a test can see what was recorded before the write.
    var duringAdd: ((String) -> Void)?

    func addCredential(_ secret: Data, reference: String) throws {
        added.append(secret)
        addedReferences.append(reference)
        duringAdd?(reference)
        try addResult.get()
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

/// An in-memory journal. Sharing one between two models stands in for the journal surviving a restart.
final class FakeProviderSettingsJournal: ProviderSettingsReferenceJournal {
    private(set) var stored: [ProviderSettingsJournalEntry] = []
    var references: [String] { stored.map(\.reference) }

    func entries() -> [ProviderSettingsJournalEntry] { stored }
    func record(_ entry: ProviderSettingsJournalEntry) {
        stored.removeAll { $0.reference == entry.reference }
        stored.append(entry)
    }
    func forget(_ reference: String) { stored.removeAll { $0.reference == reference } }
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
    private let journal = FakeProviderSettingsJournal()

    private func makeModel(_ controls: FakeProviderSettingsControls) -> ProviderSettingsModel {
        ProviderSettingsModel(
            controls: controls, credentials: credentials, journal: journal, readScope: { [unowned self] in scope })
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

    func testALockThatBlocksDeletionRetriesItOnTheNextAuthenticatedRead() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        scope = .captureOnly
        await model.refresh()
        XCTAssertEqual(model.phase, .locked)
        XCTAssertEqual(credentials.deleted.count, 1, "the lock-time attempt")

        credentials.deleteResult = .success(())
        scope = .authenticated
        await model.refresh()
        XCTAssertEqual(credentials.deleted.count, 2, "retried once authenticated again")
        await model.refresh()
        XCTAssertEqual(credentials.deleted.count, 2, "and only until it succeeds")
    }

    private func storeAndNameSecret(_ model: ProviderSettingsModel) {
        model.storeCredential(Self.syntheticSecret)
        model.draft.profileName = "work"
        model.draft.model = "synthetic-model"
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
        XCTAssertEqual(journal.references, [], "nothing is left to sweep later")
    }

    func testARelockDuringASaveThenAnAuthenticatedReadNeverDeletesTheSavedVersionsSecret() async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        controls.suspendCreate = { [unowned self] in scope = .captureOnly }

        await model.save()
        XCTAssertEqual(controls.created.count, 1)
        XCTAssertEqual(model.phase, .locked)

        scope = .authenticated
        credentials.deleteResult = .success(())
        await model.refresh()
        XCTAssertEqual(credentials.deleted, [], "neither the lock nor the later read touches the saved secret")
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
        XCTAssertEqual(credentials.deleted, ["11111111-1111-1111-1111-111111111111"])
        XCTAssertEqual(journal.references, [])
    }

    func testAnUnsavedSecretWhoseDeletionWasBlockedIsDeletedAfterARestart() async {
        let firstRun = await storedModel()
        firstRun.storeCredential(Self.syntheticSecret)
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        firstRun.clear()
        XCTAssertEqual(journal.references, ["11111111-1111-1111-1111-111111111111"], "kept for the next run")

        // The process ends here: a new model shares only the journal and the credential store.
        credentials.deleteResult = .success(())
        let secondRun = makeModel(FakeProviderSettingsControls(listing: Self.twoVersions))
        await secondRun.refresh()

        XCTAssertEqual(credentials.deleted.last, "11111111-1111-1111-1111-111111111111")
        XCTAssertEqual(journal.references, [])
    }

    /// The listing a later run reads: the profiles of `twoVersions` plus the version of "work" the save would make.
    private static let twoVersionsAndWork = providerListing(
        versions: [
            ("hosted", "hosted-v1", "open_ai", false, true), ("private", "private-v1", "self_hosted", false, true),
            ("old", "old-v1", "anthropic", true, true), ("weak", "weak-v1", "anthropic", false, false),
            ("work", "work-v1", "open_ai", false, true),
        ], defaultVersion: nil)

    /// Starts a save of a freshly stored secret and, while the core has not answered, ends the process: a second model
    /// sharing only the journal and the credential store reads `nextListing`. `afterRestart` runs once it has read.
    private func interruptSave(nextListing: ProviderProfileListAnswer, afterRestart: @escaping () -> Void) async {
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let firstRun = makeModel(controls)
        await firstRun.refresh()
        storeAndNameSecret(firstRun)
        controls.suspendCreate = { [unowned self] in
            XCTAssertEqual(
                journal.references, ["11111111-1111-1111-1111-111111111111"], "held while the save is pending")
            XCTAssertNotNil(journal.stored.first?.pendingSave)
            XCTAssertFalse(
                journal.stored.description.contains("work"), "the journal keeps no profile name, only a digest")
            await firstRun.refresh()
            XCTAssertEqual(credentials.deleted, [], "the first run never deletes a reference its pending save names")

            // The process ends here.
            let secondRun = makeModel(FakeProviderSettingsControls(listing: nextListing))
            await secondRun.refresh()
            afterRestart()
        }
        await firstRun.save()
    }

    func testASaveInterruptedBeforeItCommittedLeavesItsSecretToBeDeletedAfterARestart() async {
        var deletedAfterRestart: [String] = []
        var journalAfterRestart: [String] = ["unset"]
        await interruptSave(nextListing: Self.twoVersions) { [unowned self] in
            deletedAfterRestart = credentials.deleted
            journalAfterRestart = journal.references
        }
        XCTAssertEqual(deletedAfterRestart, ["11111111-1111-1111-1111-111111111111"], "no version names it")
        XCTAssertEqual(journalAfterRestart, [])
    }

    func testASaveInterruptedAfterItCommittedKeepsItsSecretAfterARestart() async {
        var deletedAfterRestart: [String] = ["unset"]
        var journalAfterRestart: [String] = ["unset"]
        await interruptSave(nextListing: Self.twoVersionsAndWork) { [unowned self] in
            deletedAfterRestart = credentials.deleted
            journalAfterRestart = journal.references
        }
        XCTAssertEqual(deletedAfterRestart, [], "the new version of the profile names it")
        XCTAssertEqual(journalAfterRestart, [], "settled, so nothing is swept later")
    }

    private static let firstReference = "11111111-1111-1111-1111-111111111111"
    private static let secondReference = "33333333-3333-3333-3333-333333333333"

    /// Saves a first secret for "work" and, while the core has not answered, leaves the screen, reopens it, stores a
    /// second secret for "work" and tries to save it too. Then the process ends and a second model reads
    /// `nextListing`. Returns what was deleted and journaled after the restart, how many saves reached the core and what
    /// the second attempt reported.
    private func overlapSaves(
        nextListing: ProviderProfileListAnswer
    ) async -> (deleted: [String], journaled: [String], created: Int, refusal: ProviderSettingsActionState?) {
        credentials.newReferences = [Self.firstReference, Self.secondReference]
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let firstRun = makeModel(controls)
        await firstRun.refresh()
        storeAndNameSecret(firstRun)
        var outcome: (deleted: [String], journaled: [String], created: Int, refusal: ProviderSettingsActionState?) =
            ([], [], 0, nil)
        controls.suspendCreate = { [unowned self, unowned controls] in
            firstRun.clear()
            await firstRun.refresh()
            storeAndNameSecret(firstRun)
            XCTAssertEqual(firstRun.draft.credentialReference, Self.secondReference)
            XCTAssertFalse(firstRun.canSave, "the reopened screen cannot save while the first save is unanswered")
            await firstRun.save()
            let refusal = firstRun.saveState

            // The process ends here.
            let secondRun = makeModel(FakeProviderSettingsControls(listing: nextListing))
            await secondRun.refresh()
            outcome = (credentials.deleted, journal.references, controls.created.count, refusal)
        }
        await firstRun.save()
        return outcome
    }

    func testTwoSavesOfOneProfileNeverOverlapSoARestartKeepsOnlyTheCommittedSecret() async {
        let outcome = await overlapSaves(nextListing: Self.twoVersionsAndWork)
        XCTAssertEqual(outcome.created, 1, "the second save never reached the core")
        XCTAssertEqual(outcome.refusal, .failed(ProviderSettingsText.earlierSaveStillFinishing))
        XCTAssertEqual(outcome.deleted, [Self.secondReference], "work-v1 names the first secret, nothing names the second")
        XCTAssertEqual(outcome.journaled, [])
    }

    func testTwoSavesOfOneProfileNeverOverlapSoARestartDeletesBothWhenNeitherCommitted() async {
        let outcome = await overlapSaves(nextListing: Self.twoVersions)
        XCTAssertEqual(outcome.created, 1)
        XCTAssertEqual(outcome.refusal, .failed(ProviderSettingsText.earlierSaveStillFinishing))
        XCTAssertEqual(Set(outcome.deleted), [Self.firstReference, Self.secondReference])
        XCTAssertEqual(outcome.journaled, [])
    }

    func testASaveAfterAnUnseenCommitIsSettledOnlyByItsOwnVersion() async {
        credentials.newReferences = [Self.firstReference, Self.secondReference]
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        storeAndNameSecret(model)
        // The screen is left and reopened while the first save is pending; it reads the list before that save
        // commits, so the list it shows lacks work-v1.
        controls.suspendCreate = { [unowned self, unowned controls] in
            model.clear()
            await model.refresh()
            controls.listing = .success(Self.twoVersionsAndWork)
        }
        await model.save()
        XCTAssertEqual(model.listing, Self.twoVersions, "the shown list predates the first save's version")
        XCTAssertEqual(journal.references, [], "the first save committed")

        storeAndNameSecret(model)
        var deletedAfterRestart: [String] = ["unset"]
        var journalAfterRestart: [String] = ["unset"]
        controls.suspendCreate = { [unowned self] in
            XCTAssertEqual(journal.stored.first?.pendingSave?.knownVersions, ["work-v1"], "read afresh, not from the list")
            // The process ends here, before the second save commits.
            let secondRun = makeModel(FakeProviderSettingsControls(listing: Self.twoVersionsAndWork))
            await secondRun.refresh()
            deletedAfterRestart = credentials.deleted
            journalAfterRestart = journal.references
        }
        await model.save()

        XCTAssertEqual(deletedAfterRestart, [Self.secondReference], "work-v1 is the first save's, not the second's")
        XCTAssertEqual(journalAfterRestart, [])
    }

    func testAnInterruptedSaveFoundUncommittedStaysUnsavedWhenItsDeletionFails() async {
        journal.record(
            ProviderSettingsJournalEntry(
                reference: Self.firstReference,
                pendingSave: ProviderSettingsPendingSave(profileID: "work", knownVersions: [])))
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        let controls = FakeProviderSettingsControls(listing: Self.twoVersions)
        let model = makeModel(controls)
        await model.refresh()
        XCTAssertEqual(
            journal.stored, [ProviderSettingsJournalEntry(reference: Self.firstReference)],
            "settled as never committed, so no later version of the profile can claim it")

        credentials.deleteResult = .success(())
        controls.listing = .success(Self.twoVersionsAndWork)
        await model.refresh()
        XCTAssertEqual(credentials.deleted, [Self.firstReference, Self.firstReference])
        XCTAssertEqual(journal.references, [])
    }

    func testANewSecretIsJournaledBeforeItIsWrittenAndKeptForLaterIfItsWriteAndCleanupFail() async {
        let model = await storedModel()
        var journaledDuringWrite: [String] = []
        credentials.duringAdd = { [unowned self] _ in journaledDuringWrite = journal.references }
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(journaledDuringWrite, [Self.firstReference])
        XCTAssertEqual(credentials.addedReferences, [Self.firstReference])
        model.removeCredential()

        credentials.newReferences = [Self.secondReference]
        credentials.addResult = .failure(CredentialError.emptySecret)
        credentials.deleteResult = .failure(ProviderConfigurationError.notAuthenticated)
        model.storeCredential(Self.syntheticSecret)
        XCTAssertEqual(model.draft.credentialReference, "", "a failed write names nothing")
        XCTAssertEqual(journal.references, [Self.secondReference], "kept so a later read can delete it")

        credentials.deleteResult = .success(())
        await model.refresh()
        XCTAssertEqual(credentials.deleted.last, Self.secondReference)
        XCTAssertEqual(journal.references, [])
    }

    func testAnInterruptedSaveIsNotSettledByAVersionThatExistedBeforeIt() {
        let pending = ProviderSettingsPendingSave(profileID: "work", knownVersions: ["work-v1"])
        XCTAssertFalse(pending.committed(in: Self.twoVersionsAndWork), "work-v1 was already there")
        XCTAssertTrue(
            ProviderSettingsPendingSave(profileID: "work", knownVersions: []).committed(in: Self.twoVersionsAndWork))
        XCTAssertFalse(
            ProviderSettingsPendingSave(profileID: "other", knownVersions: []).committed(in: Self.twoVersionsAndWork))
    }

    func testTheDefaultsJournalKeepsEntriesAcrossInstances() throws {
        let suite = "provider-settings-journal-test-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let pending = ProviderSettingsPendingSave(profileID: "work", knownVersions: ["work-v1"])
        let writer = UserDefaultsReferenceJournal(defaults: defaults)
        writer.record(ProviderSettingsJournalEntry(reference: "ref-a"))
        writer.record(ProviderSettingsJournalEntry(reference: "ref-b", pendingSave: pending))

        let reader = UserDefaultsReferenceJournal(defaults: defaults)
        XCTAssertEqual(
            reader.entries(),
            [
                ProviderSettingsJournalEntry(reference: "ref-a"),
                ProviderSettingsJournalEntry(reference: "ref-b", pendingSave: pending),
            ])
        reader.record(ProviderSettingsJournalEntry(reference: "ref-b"))
        reader.forget("ref-a")
        XCTAssertEqual(writer.entries(), [ProviderSettingsJournalEntry(reference: "ref-b")])
    }

    func testASweepNeverDeletesTheSecretOfTheDraftBeingEdited() async {
        let model = await storedModel()
        model.storeCredential(Self.syntheticSecret)
        await model.refresh()
        XCTAssertEqual(credentials.deleted, [])
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
