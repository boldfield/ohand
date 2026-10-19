import XCTest
@testable import OhAndServices

/// The local preconditions for offering an optional capability. They need no core: whether a request may be sent is
/// decided by the core's stored routes, grants and profiles, which the integration tests exercise.
final class OptionalCapabilitiesGateTests: XCTestCase {
    private let selfHostedProfileVersion = "9d3f6b20-1c7e-4a58-8e2b-6a0c4d9e7f11"

    private var verified: SelfHostedRegistration {
        SelfHostedRegistration(
            profileVersion: selfHostedProfileVersion, model: SelfHostedVerifiedBoundary.current.model)
    }

    private func shadowConfiguration(
        profileVersion: String? = "7c2d9a10-5b3e-4f61-9d0a-1e8f4a6b2c33", enabled: Bool = true
    ) -> ShadowReviewConfiguration {
        ShadowReviewConfiguration(
            reviewProfileVersion: profileVersion,
            policy: ShadowReviewPolicy(
                isEnabled: enabled, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 4,
                maxAttemptsPerSample: 2))
    }

    // MARK: absent by default

    func testNothingIsConfiguredByDefault() {
        let module = OptionalCapabilitiesModule()
        XCTAssertEqual(module.selfHostedState, .disabled(.notConfigured))
        XCTAssertEqual(module.shadowReviewState, .disabled(.notConfigured))
        XCTAssertNil(module.selfHosted)
        XCTAssertNil(module.shadowReview)
        XCTAssertTrue(module.jobCapabilities.isEmpty, "the module adds no job type of its own")
    }

    func testRegistrationCarriesNoEndpointCredentialOrRoute() {
        let labels = Mirror(reflecting: verified).children.compactMap { $0.label }
        XCTAssertEqual(labels, ["profileVersion", "model", "evidence"])
        let configurationLabels = Mirror(reflecting: OptionalCapabilitiesConfiguration.none).children.compactMap { $0.label }
        XCTAssertEqual(configurationLabels, ["selfHosted", "shadowReview"])
    }

    // MARK: self-hosted

    func testAVerifiedRegistrationIsEnabled() {
        XCTAssertEqual(OptionalCapabilityVerification.selfHostedState(verified), .enabled)
        let module = OptionalCapabilitiesModule(configuration: OptionalCapabilitiesConfiguration(selfHosted: verified))
        XCTAssertEqual(module.selfHosted, verified)
    }

    func testABlankProfileVersionStaysDisabled() {
        for blank in ["", "   ", "\n"] {
            var registration = verified
            registration.profileVersion = blank
            XCTAssertEqual(
                OptionalCapabilityVerification.selfHostedState(registration), .disabled(.missingProfileVersion))
        }
    }

    func testAMissingOrAlteredEvidenceReferenceStaysDisabled() {
        for kind in SelfHostedEvidenceKind.allCases {
            var registration = verified
            registration.evidence.removeAll { $0.kind == kind }
            XCTAssertEqual(
                OptionalCapabilityVerification.selfHostedState(registration), .disabled(.evidenceIncomplete),
                "without \(kind.rawValue)")
        }
        var forged = verified
        forged.evidence = forged.evidence.map {
            SelfHostedEvidenceReference(
                kind: $0.kind, evidenceID: $0.evidenceID + "-forged", artifactPath: $0.artifactPath,
                collectedAt: $0.collectedAt)
        }
        XCTAssertEqual(OptionalCapabilityVerification.selfHostedState(forged), .disabled(.evidenceIncomplete))
        var empty = verified
        empty.evidence = []
        XCTAssertEqual(OptionalCapabilityVerification.selfHostedState(empty), .disabled(.evidenceIncomplete))
    }

    func testEverySmokeVerifiedModelIsEnabledWithItsOwnEvidence() {
        let boundary = SelfHostedVerifiedBoundary.current
        XCTAssertGreaterThanOrEqual(boundary.models.count, 2)
        for verifiedModel in boundary.models {
            let registration = SelfHostedRegistration(profileVersion: selfHostedProfileVersion, model: verifiedModel.model)
            XCTAssertEqual(OptionalCapabilityVerification.selfHostedState(registration), .enabled, verifiedModel.model)
        }
    }

    func testAVerifiedModelCitingAnotherModelsSmokeArtifactStaysDisabled() {
        let boundary = SelfHostedVerifiedBoundary.current
        let first = boundary.models[0]
        let second = boundary.models[1]
        let borrowed = SelfHostedRegistration(
            profileVersion: selfHostedProfileVersion, model: second.model,
            evidence: [boundary.workerProbe, first.adapterSmoke, boundary.phoneReachability])
        XCTAssertEqual(OptionalCapabilityVerification.selfHostedState(borrowed), .disabled(.evidenceIncomplete))
    }

    func testAModelOutsideTheVerifiedBoundaryStaysDisabled() {
        for model in ["", "deepseek-flash-iq3", "another-listed-model:latest", "DEEPSEEK-FLASH-IQ3:LATEST", "gpt-oss:20b"] {
            var registration = verified
            registration.model = model
            XCTAssertEqual(
                OptionalCapabilityVerification.selfHostedState(registration), .disabled(.modelNotVerified), model)
        }
    }

    // MARK: shadow review

    func testShadowReviewNeedsANamedProfileAndAnEnabledBudget() {
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(nil, selfHosted: nil), .disabled(.notConfigured))
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(ShadowReviewConfiguration.off, selfHosted: nil), .disabled(.reviewProfileMissing))
        for blank in [nil, "", "  "] {
            XCTAssertEqual(
                OptionalCapabilityVerification.shadowReviewState(
                    shadowConfiguration(profileVersion: blank), selfHosted: nil),
                .disabled(.reviewProfileMissing))
        }
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(shadowConfiguration(enabled: false), selfHosted: nil),
            .disabled(.policyDisabled))
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(shadowConfiguration(), selfHosted: nil), .enabled)
    }

    func testTheReviewerMustNotBeThePrimaryProfile() {
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(
                shadowConfiguration(profileVersion: selfHostedProfileVersion), selfHosted: verified),
            .disabled(.reviewProfileNotIndependent))
        XCTAssertEqual(
            OptionalCapabilityVerification.shadowReviewState(shadowConfiguration(), selfHosted: verified), .enabled)
    }

    func testEachCapabilityIsDecidedOnItsOwn() {
        var unverified = verified
        unverified.model = "unlisted"
        let module = OptionalCapabilitiesModule(
            configuration: OptionalCapabilitiesConfiguration(selfHosted: unverified, shadowReview: shadowConfiguration()))
        XCTAssertEqual(module.selfHostedState, .disabled(.modelNotVerified))
        XCTAssertEqual(module.shadowReviewState, .enabled)
    }

    // MARK: the claim and the core's evidence agree

    func testTheVerifiedBoundaryMatchesTheCoreSource() throws {
        let repositoryRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let source = try String(
            contentsOf: repositoryRoot.appendingPathComponent("core/src/providers/self_hosted/mod.rs"),
            encoding: .utf8)
        let boundary = SelfHostedVerifiedBoundary.current
        let coreEntryCount = source.components(separatedBy: "\n")
            .filter { $0.hasPrefix("        model: \"") }.count
        XCTAssertEqual(boundary.models.count, coreEntryCount, "one element per SMOKE_EVIDENCE entry")
        for verifiedModel in boundary.models {
            XCTAssertTrue(source.contains("model: \"\(verifiedModel.model)\""), "the verified model")
        }
        let references = [boundary.workerProbe, boundary.phoneReachability] + boundary.models.map(\.adapterSmoke)
        XCTAssertEqual(
            Set(references.map(\.kind)), Set(SelfHostedEvidenceKind.allCases), "one reference per kind")
        for reference in references {
            for value in [reference.evidenceID, reference.artifactPath, reference.collectedAt] {
                XCTAssertTrue(source.contains("\"\(value)\""), "\(reference.kind.rawValue): \(value)")
            }
            XCTAssertTrue(
                FileManager.default.fileExists(atPath: repositoryRoot.appendingPathComponent(reference.artifactPath).path),
                "the artifact for \(reference.kind.rawValue) is in the repository")
        }
        for verifiedModel in boundary.models {
            XCTAssertTrue(
                source.contains("model: \"\(verifiedModel.model)\",\n        id: \"\(verifiedModel.adapterSmoke.evidenceID)\""),
                "\(verifiedModel.model) cites its own artifact")
        }
    }
}
