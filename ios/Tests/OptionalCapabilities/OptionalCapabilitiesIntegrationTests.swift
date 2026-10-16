import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Both optional capabilities joined to the production root through the extension seam, over the real Rust core, with a
/// recording sender in place of the network. Everything here is synthetic.
final class OptionalCapabilitiesIntegrationTests: OptionalCapabilitiesTestCase {
    private func selfHostedSender() throws -> RecordingProviderSender {
        RecordingProviderSender(repliesByHost: ["spark.example.test": try OptionalCapabilitiesReplies.selfHosted()])
    }

    private func reviewSender(itemType: String) throws -> RecordingProviderSender {
        RecordingProviderSender(repliesByHost: ["api.anthropic.com": try OptionalCapabilitiesReplies.review(itemType: itemType)])
    }

    private func save(_ root: ServiceCompositionRoot, text: String = "buy stamps") -> CompositionSaveOutcome? {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) { outcome = $0 }
        _ = pump(until: { outcome != nil })
        return outcome
    }

    private func assertNotReviewed(_ run: ShadowReviewRunResult?, file: StaticString = #filePath, line: UInt = #line) {
        switch run {
        case .completed(let record, _)?:
            XCTAssertNotEqual(record.outcome, .reviewed, "no verdict was produced", file: file, line: line)
            XCTAssertNil(record.verdict, file: file, line: line)
        case .failed?:
            break
        case nil:
            XCTFail("the run was not answered", file: file, line: line)
        }
    }

    // MARK: neither feature configured

    func testBaseAppConstructsAndCapturesWithNeitherFeatureConfigured() throws {
        try prepareSelfHostedStore(SelfHostedScenario(interpretationGranted: false, includesProfile: false))
        let sender = RecordingProviderSender()
        let module = OptionalCapabilitiesModule()
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }

        XCTAssertNil(module.shadowReview)
        XCTAssertNil(module.selfHosted)
        guard case .saved(_, _, false)? = save(root) else {
            return XCTFail("expected a confirmed save without any optional capability")
        }
        root.start(isActive: true)
        settle()
        XCTAssertTrue(sender.requests.isEmpty, "no optional capability means no hidden endpoint is contacted")
    }

    func testTheModuleDoesNotTakeTheShadowClientUnlessShadowReviewIsOn() throws {
        try prepareShadowReviewStore()
        let probe = ShadowClientProbeModule(configuration: shadowReviewConfiguration())
        let root = try makeRoot(sender: RecordingProviderSender(), modules: [OptionalCapabilitiesModule(), probe])
        defer { root.shutDown() }
        XCTAssertNotNil(probe.service, "an unconfigured module leaves the fixed-range client free")
    }

    func testShadowReviewTakesTheClientOnlyOnceAcrossModules() throws {
        try prepareShadowReviewStore()
        let enabled = OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration())
        let module = OptionalCapabilitiesModule(configuration: enabled)
        let probe = ShadowClientProbeModule(configuration: shadowReviewConfiguration())
        XCTAssertThrowsError(try makeRoot(sender: RecordingProviderSender(), modules: [module, probe])) {
            XCTAssertEqual($0 as? CompositionError, .coreClientAlreadyTaken(ServiceCompositionRoot.shadowReviewClientName))
        }
    }

    // MARK: self-hosted

    func testVerifiedSelfHostedRegistrationRunsEndToEndAgainstItsOwnEndpoint() throws {
        try prepareSelfHostedStore()
        let sender = try selfHostedSender()
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        XCTAssertEqual(module.selfHosted, verifiedRegistration())
        XCTAssertNil(module.shadowReview, "shadow review stays off unless it was configured")

        root.start(isActive: true)
        XCTAssertTrue(pump(until: { self.jobStatus() == "completed" }, timeout: 30), "the job completes")

        let request = try XCTUnwrap(sender.requests.first)
        XCTAssertEqual(sender.requests.count, 1)
        XCTAssertEqual(request.url.absoluteString, "https://spark.example.test/v1/chat/completions")
        XCTAssertEqual(request.method, .post)
        XCTAssertEqual(request.authorization.capability, .textInterpretation)
        XCTAssertEqual(request.authorization.authorizedOrigins, [SelfHostedScenario.origin])
        XCTAssertEqual(
            request.credential,
            ProviderCredentialAttachment(
                reference: SelfHostedScenario.credentialReference, headerName: "Authorization", scheme: "Bearer"))
        XCTAssertTrue(String(decoding: request.body ?? Data(), as: UTF8.self).contains(SelfHostedScenario.captureText))
    }

    func testAbsentRegistrationLeavesTheStoredSelfHostedProfileUnused() throws {
        try prepareSelfHostedStore()
        for modules in [[], [OptionalCapabilitiesModule()]] as [[CompositionModule]] {
            let sender = try selfHostedSender()
            let root = try makeRoot(sender: sender, modules: modules)
            root.start(isActive: true)
            XCTAssertTrue(waitForDrain(root), "the drain ends")
            XCTAssertTrue(sender.requests.isEmpty, "a supported stored profile is not enough without the module")
            XCTAssertNotEqual(jobStatus(), "completed")
            root.shutDown()
        }
    }

    func testUnverifiedRegistrationLeavesTheStoredSelfHostedProfileUnused() throws {
        // The stored profile is supported and serves the verified model: only the module's gate can stop the send.
        try prepareSelfHostedStore()
        var wrongModel = verifiedRegistration()
        wrongModel.model = "unlisted-model:latest"
        let incompleteEvidence = SelfHostedRegistration(
            profileVersion: SelfHostedScenario.profileVersion, model: SelfHostedVerifiedBoundary.current.model,
            evidence: [])
        for registration in [wrongModel, incompleteEvidence] {
            let sender = try selfHostedSender()
            let module = OptionalCapabilitiesModule(
                configuration: OptionalCapabilitiesConfiguration(selfHosted: registration))
            XCTAssertTrue(module.enabledProviderProtocols.isEmpty)
            let root = try makeRoot(sender: sender, modules: [module])
            root.start(isActive: true)
            XCTAssertTrue(waitForDrain(root), "the drain ends")
            XCTAssertTrue(sender.requests.isEmpty, "an unverified registration keeps self-hosted disabled")
            XCTAssertNotEqual(jobStatus(), "completed")
            root.shutDown()
        }
    }

    func testVerifiedRegistrationDeclaresOnlyTheSelfHostedProtocol() {
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        XCTAssertEqual(module.enabledProviderProtocols, [.selfHosted])
        XCTAssertTrue(OptionalCapabilitiesModule().enabledProviderProtocols.isEmpty)
    }

    private func availability(of module: OptionalCapabilitiesModule) -> SelfHostedProfileAvailability {
        _ = pump(until: { module.selfHostedAvailability != .unchecked }, timeout: 10)
        return module.selfHostedAvailability
    }

    func testModuleReportsWhetherTheRegisteredProfileIsUsable() throws {
        let scenarios: [(SelfHostedScenario, SelfHostedProfileAvailability)] = [
            (SelfHostedScenario(), .available),
            (SelfHostedScenario(isRevoked: true), .unavailable(.revoked)),
            (SelfHostedScenario(includesProfile: false), .unavailable(.missing)),
        ]
        for (scenario, expected) in scenarios {
            try? FileManager.default.removeItem(at: storageRoot)
            try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
            try prepareSelfHostedStore(scenario)
            let module = OptionalCapabilitiesModule(
                configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
            let root = try makeRoot(sender: RecordingProviderSender(), modules: [module])
            XCTAssertEqual(availability(of: module), expected)
            root.shutDown()
        }
    }

    func testModuleReportsAProfileOfAnotherProtocolAsUnusable() throws {
        try prepareShadowReviewStore()
        let registration = SelfHostedRegistration(
            profileVersion: ShadowReviewScenario.profileVersion, model: SelfHostedVerifiedBoundary.current.model)
        let module = OptionalCapabilitiesModule(configuration: OptionalCapabilitiesConfiguration(selfHosted: registration))
        let root = try makeRoot(sender: RecordingProviderSender(), modules: [module])
        defer { root.shutDown() }
        XCTAssertEqual(availability(of: module), .unavailable(.notSelfHosted))
    }

    func testUnconfiguredModuleNeverChecksTheStore() throws {
        try prepareSelfHostedStore()
        let module = OptionalCapabilitiesModule()
        let root = try makeRoot(sender: RecordingProviderSender(), modules: [module])
        defer { root.shutDown() }
        settle(0.5)
        XCTAssertEqual(module.selfHostedAvailability, .unchecked)
    }

    func testRegistrationWithoutAStoredProfileContactsNothing() throws {
        try prepareSelfHostedStore(SelfHostedScenario(includesProfile: false))
        let sender = try selfHostedSender()
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        root.start(isActive: true)
        XCTAssertTrue(waitForDrain(root), "the drain ends")
        XCTAssertTrue(sender.requests.isEmpty, "the registration carries no endpoint of its own")
        XCTAssertNotEqual(jobStatus(), "completed")
    }

    func testDeniedRouteSendsNothing() throws {
        try prepareSelfHostedStore(SelfHostedScenario(interpretationGranted: false))
        let sender = try selfHostedSender()
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        root.start(isActive: true)
        XCTAssertTrue(waitForDrain(root), "the drain ends")
        XCTAssertTrue(sender.requests.isEmpty, "a route without a grant never reaches the transport")
        XCTAssertNotEqual(jobStatus(), "completed")
    }

    func testRevokedSelfHostedProfileSendsNothing() throws {
        try prepareSelfHostedStore(SelfHostedScenario(isRevoked: true))
        let sender = try selfHostedSender()
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        root.start(isActive: true)
        XCTAssertTrue(waitForDrain(root), "the drain ends")
        XCTAssertTrue(sender.requests.isEmpty, "a revoked profile never reaches the transport")
        XCTAssertNotEqual(jobStatus(), "completed")
    }

    func testUnverifiedModelCapabilitySendsNothingEvenWhenARegistrationIsPresent() throws {
        try prepareSelfHostedStore(SelfHostedScenario(model: "unlisted-model:latest", capabilitySupport: "unverified"))
        let sender = try selfHostedSender()
        var registration = verifiedRegistration()
        registration.model = "unlisted-model:latest"
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: registration))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        XCTAssertNil(module.selfHosted, "an unverified model is not offered")
        root.start(isActive: true)
        XCTAssertTrue(waitForDrain(root), "the drain ends")
        XCTAssertTrue(sender.requests.isEmpty, "the core refuses a capability that is not supported")
        XCTAssertNotEqual(jobStatus(), "completed")
    }

    func testSelfHostedFailureLeavesTheCaptureDurable() throws {
        try prepareSelfHostedStore()
        let sender = FailingSender(.connectionFailed)
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: verifiedRegistration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        root.start(isActive: true)
        XCTAssertTrue(waitForDrain(root), "the drain ends")
        XCTAssertNotEqual(jobStatus(), "completed")
        guard case .saved? = save(root) else { return XCTFail("capture stays available during a provider outage") }
    }

    // MARK: shadow review

    func testShadowReviewRegisteredThroughTheModuleRecordsAVerdictWithoutChangingAuthoritativeState() throws {
        try prepareShadowReviewStore(
            ShadowReviewScenario(credentialReference: "synthetic-credential", primaryItemType: "idea"))
        let sender = try reviewSender(itemType: "action")
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.shadowReview)
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: databasePath)

        let jobID = try selectCase(service)
        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }), "the review completes")

        guard case .completed(let record, nil)? = run else {
            return XCTFail("expected a completed review, got \(String(describing: run))")
        }
        XCTAssertEqual(record.outcome, .reviewed)
        XCTAssertEqual(record.verdict, .disagreement)
        XCTAssertEqual(sender.requests.count, 1)
        XCTAssertEqual(sender.requests.first?.authorization.capability, .review)
        XCTAssertEqual(sender.requests.first?.url.host, "api.anthropic.com")
        XCTAssertEqual(
            try ShadowReviewSeed.authoritativeSnapshot(path: databasePath), before,
            "a disagreeing verdict changes no item, proposal, correction, grant, profile or capture")
    }

    func testRevokedReviewGrantDeniesTheReviewBeforeAnySend() throws {
        try prepareShadowReviewStore()
        let sender = try reviewSender(itemType: "action")
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.shadowReview)
        let jobID = try selectCase(service)
        try ShadowReviewSeed.revokeReviewGrant(path: databasePath)
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: databasePath)

        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }))

        guard case .completed(let record, let denial?)? = run else {
            return XCTFail("expected a refused run, got \(String(describing: run))")
        }
        XCTAssertEqual(record.outcome, .unreviewed)
        XCTAssertFalse(denial.isEmpty)
        XCTAssertTrue(sender.requests.isEmpty, "a revoked grant is never sent to")
        XCTAssertEqual(try ShadowReviewSeed.authoritativeSnapshot(path: databasePath), before)
    }

    func testRevokedReviewProfileSendsNothing() throws {
        try prepareShadowReviewStore()
        let sender = try reviewSender(itemType: "action")
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.shadowReview)
        let jobID = try selectCase(service)
        try runSQL(["UPDATE provider_profiles SET revoked_at = '2026-10-08T09:10:00Z'"])

        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }))

        assertNotReviewed(run)
        XCTAssertTrue(sender.requests.isEmpty, "a revoked review profile is never sent to")
    }

    func testReviewProviderOutageNeitherBlocksCaptureNorChangesAuthoritativeState() throws {
        try prepareShadowReviewStore()
        let sender = FailingSender(.connectionFailed)
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: sender, modules: [module], captureRouteID: ShadowReviewScenario.routeID)
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.shadowReview)
        guard case .saved? = save(root) else { return XCTFail("capture is not blocked by the reviewer") }
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: databasePath)

        let jobID = try selectCase(service)
        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }, timeout: 30))

        assertNotReviewed(run)
        XCTAssertEqual(try ShadowReviewSeed.authoritativeSnapshot(path: databasePath), before)
        guard case .saved? = save(root, text: "water the plants") else {
            return XCTFail("capture still works after the failed review")
        }
    }

    func testShadowReviewStaysOffWithoutAnEnabledBudgetOrAnIndependentProfile() throws {
        try prepareShadowReviewStore()
        let offBudget = OptionalCapabilitiesConfiguration(
            shadowReview: ShadowReviewConfiguration(
                reviewProfileVersion: ShadowReviewScenario.profileVersion, policy: .disabled))
        let samePrimary = OptionalCapabilitiesConfiguration(
            selfHosted: SelfHostedRegistration(
                profileVersion: ShadowReviewScenario.profileVersion, model: SelfHostedVerifiedBoundary.current.model),
            shadowReview: shadowReviewConfiguration())
        for configuration in [offBudget, samePrimary] {
            let module = OptionalCapabilitiesModule(configuration: configuration)
            let probe = ShadowClientProbeModule(configuration: shadowReviewConfiguration())
            let sender = try reviewSender(itemType: "action")
            let root = try makeRoot(sender: sender, modules: [module, probe])
            XCTAssertNil(module.shadowReview)
            XCTAssertNotNil(probe.service, "the module did not take the client")
            XCTAssertTrue(sender.requests.isEmpty)
            root.shutDown()
        }
    }

    func testShuttingDownTheRootReleasesTheShadowService() throws {
        try prepareShadowReviewStore()
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(shadowReview: shadowReviewConfiguration()))
        let root = try makeRoot(sender: RecordingProviderSender(), modules: [module])
        XCTAssertNotNil(module.shadowReview)
        root.shutDown()
        XCTAssertNil(module.shadowReview)
    }
}
