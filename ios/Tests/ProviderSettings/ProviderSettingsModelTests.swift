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
}

private func decodeCreated(profileID: String, version: String) -> ProviderProfileCreatedAnswer {
    let json = #"{"operation_id":1,"profile_id":"\#(profileID)","profile_version":"\#(version)"}"#
    return try! JSONDecoder().decode(ProviderProfileCreatedAnswer.self, from: Data(json.utf8))
}

func providerListing(
    versions: [(profile: String, version: String, providerProtocol: String, revoked: Bool, supported: Bool)],
    defaultVersion: String?
) -> ProviderProfileListAnswer {
    var profiles: [String: [String]] = [:]
    for entry in versions {
        let support = entry.supported ? "supported" : "unsupported"
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

    private func makeModel(_ controls: FakeProviderSettingsControls) -> ProviderSettingsModel {
        ProviderSettingsModel(controls: controls, readScope: { [unowned self] in scope })
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
}
