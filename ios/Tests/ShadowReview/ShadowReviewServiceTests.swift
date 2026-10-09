import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Shadow review on the simulator: the real Rust core samples and authorizes each case, the real
/// `ProviderExchangeCoordinator` and `ProviderHTTPTransport` carry the one review request to a loopback fixture
/// server, and the core stores only a bounded diagnostic outcome.
final class ShadowReviewServiceTests: ProviderTransportTestCase {
    private var storeDirectory: URL!
    private var session: ShadowReviewSession?
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private let instrumentation = RecordingInstrumentation()

    private let enabledPolicy = ShadowReviewPolicy(
        isEnabled: true, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 4, maxAttemptsPerSample: 2)

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        storeDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ohand-shadow-review-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: storeDirectory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        session?.close()
        session = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core was released")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "every result buffer was released")
        try? FileManager.default.removeItem(at: storeDirectory)
        super.tearDown()
    }

    // MARK: helpers

    private func configuration(
        policy: ShadowReviewPolicy? = nil, observed: Bool = true
    ) -> ShadowReviewConfiguration {
        ShadowReviewConfiguration(
            reviewProfileVersion: ShadowReviewScenario.profileVersion,
            policy: policy ?? enabledPolicy,
            instrumentation: observed ? instrumentation : nil)
    }

    private func open(
        sender: ProviderRequestSender,
        scenario: ShadowReviewScenario,
        configuration: ShadowReviewConfiguration
    ) throws -> ShadowReviewSession {
        let storePath = try ShadowReviewSeed.makeStore(in: storeDirectory, scenario: scenario)
        let opened = try ShadowReviewSession(storePath: storePath, sender: sender, configuration: configuration)
        session = opened
        return opened
    }

    private func serverSession(
        reply: Data? = nil,
        hang: Bool = false,
        scenario: ((String) -> ShadowReviewScenario)? = nil,
        configuration config: ShadowReviewConfiguration? = nil
    ) throws -> (ShadowReviewSession, FixtureRoutingSender, FixtureTLSServer) {
        let server = try startServer { _ in
            hang ? .hang : .complete(status: 200, headers: ["Content-Type": "application/json"], body: reply ?? Data())
        }
        let reference = try storeSecret()
        let sender = FixtureRoutingSender(underlying: transport, fixtureServer: server)
        let seeded = scenario?(reference) ?? ShadowReviewScenario(credentialReference: reference)
        let opened = try open(sender: sender, scenario: seeded, configuration: config ?? configuration())
        return (opened, sender, server)
    }

    private func anthropicReply(itemType: String) throws -> Data {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": itemType,
            "source_spans": [["start": 0, "end": 4]],
        ]
        let reply: [String: Any] = [
            "id": "msg_synthetic_01",
            "type": "message",
            "role": "assistant",
            "model": ShadowReviewScenario.reviewModel,
            "content": [["type": "tool_use", "id": "toolu_synthetic_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }

    private func selectedJobID(_ session: ShadowReviewSession, file: StaticString = #filePath, line: UInt = #line) throws -> String {
        guard case .selected(let record)? = session.offer() else {
            XCTFail("expected the case to be selected", file: file, line: line)
            throw XCTSkip("no selected case")
        }
        XCTAssertEqual(record.outcome, .pending, file: file, line: line)
        return record.jobID
    }

    private func assertRedacted(_ text: String, file: StaticString = #filePath, line: UInt = #line) {
        for secret in [
            ShadowReviewScenario.routeID, ShadowReviewScenario.captureText, syntheticSecret, replacementSecret,
            "x-api-key",
        ] {
            XCTAssertFalse(text.contains(secret), "\(secret) leaked", file: file, line: line)
        }
    }

    // MARK: configuration

    func testReviewIsOffByDefaultAndNeverReachesTheCoreOrTheNetwork() throws {
        let (session, sender, server) = try serverSession(configuration: .off)

        XCTAssertFalse(session.service.isConfigured)
        XCTAssertEqual(session.offer(), .skipped(.notConfigured))
        XCTAssertTrue(session.events.isEmpty, "the core was never called")
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(ShadowReviewPolicy.disabled.isEnabled, false)
    }

    func testConfigurationNamesTheSeparateApprovedProfileAndNeedsBothItAndAnEnabledPolicy() throws {
        let configured = configuration()
        XCTAssertEqual(configured.reviewProfileVersion, ShadowReviewScenario.profileVersion)

        let (session, _, _) = try serverSession(configuration: configured)
        XCTAssertTrue(session.service.isConfigured)
        session.service.updateConfiguration(ShadowReviewConfiguration(reviewProfileVersion: nil, policy: enabledPolicy))
        XCTAssertFalse(session.service.isConfigured, "a policy without a review profile selects nothing")
        XCTAssertEqual(session.offer(), .skipped(.notConfigured))
        session.service.updateConfiguration(ShadowReviewConfiguration(reviewProfileVersion: "p", policy: .disabled))
        XCTAssertFalse(session.service.isConfigured, "the default policy is off")
    }

    // MARK: dispatch

    func testAgreementIsOneRequestToTheReviewProfileAndStoresARedactedVerdict() throws {
        let (session, sender, server) = try serverSession(reply: try anthropicReply(itemType: "action"))
        let jobID = try selectedJobID(session)
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath)

        let result = session.review(jobID: jobID)

        guard case .completed(let record, let denial)? = result else { return XCTFail("expected a completed run") }
        XCTAssertNil(denial)
        XCTAssertEqual(record.outcome, .reviewed)
        XCTAssertEqual(record.verdict, .agreement)
        XCTAssertEqual(record.differences, [])
        XCTAssertEqual(record.attemptsUsed, 1)
        XCTAssertEqual(record.reviewProfileVersion, ShadowReviewScenario.profileVersion)

        XCTAssertEqual(sender.requestsFromCore.count, 1)
        let fromCore = try XCTUnwrap(sender.requestsFromCore.first)
        XCTAssertEqual(fromCore.url.absoluteString, "https://api.anthropic.com/v1/messages")
        XCTAssertEqual(fromCore.authorization.authorizedOrigins, [ShadowReviewScenario.vendorOrigin])
        let reference = try XCTUnwrap(fromCore.credential?.reference)
        XCTAssertEqual(fromCore.credential?.headerName, "x-api-key")
        XCTAssertNil(fromCore.credential?.scheme)
        let body = String(decoding: try XCTUnwrap(fromCore.body), as: UTF8.self)
        XCTAssertTrue(body.contains(ShadowReviewScenario.reviewModel), "the separate review profile's model is used")
        XCTAssertEqual(server.requests.count, 1)
        XCTAssertEqual(server.requests.first?.headers["x-api-key"], syntheticSecret)

        assertRedacted(session.eventText)
        XCTAssertFalse(session.eventText.contains(reference))
        XCTAssertEqual(try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath), before)
        XCTAssertEqual(instrumentation.events.map(\.name), ["selected", "dispatched", "reviewed"])
        XCTAssertEqual(session.record(jobID: jobID), .found(record))
    }

    func testDisagreementStoresDifferenceCodesOnlyAndNeverStartsASecondRound() throws {
        let (session, sender, _) = try serverSession(reply: try anthropicReply(itemType: "idea"))
        let jobID = try selectedJobID(session)
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath)

        guard case .completed(let record, nil)? = session.review(jobID: jobID) else {
            return XCTFail("expected a completed run")
        }

        XCTAssertEqual(record.outcome, .reviewed)
        XCTAssertEqual(record.verdict, .disagreement)
        XCTAssertEqual(record.differences, ["item_type"])
        assertRedacted(session.eventText)
        XCTAssertFalse(session.eventText.contains("idea"), "the reviewer's content is not stored or reported")
        XCTAssertEqual(try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath), before)

        XCTAssertEqual(session.review(jobID: jobID), .failed(code: "not_leasable"))
        XCTAssertEqual(sender.requestsFromCore.count, 1, "a disagreement never triggers another request")
        XCTAssertEqual(instrumentation.events.last?.name, "failed")
    }

    // MARK: failure, timeout and cancellation

    func testTimeoutRecordsTheCaseAsUnreviewedWithoutRetrying() throws {
        let failing = FailingSender(.timeout)
        let reference = try storeSecret()
        let session = try open(
            sender: failing, scenario: ShadowReviewScenario(credentialReference: reference),
            configuration: configuration())
        let jobID = try selectedJobID(session)

        guard case .completed(let record, nil)? = session.review(jobID: jobID) else {
            return XCTFail("expected a completed run")
        }

        XCTAssertEqual(record.outcome, .unreviewed)
        XCTAssertEqual(record.reason, "timeout")
        XCTAssertNil(record.verdict)
        XCTAssertEqual(failing.sendCount, 1)
        XCTAssertEqual(session.review(jobID: jobID), .failed(code: "not_leasable"))
        XCTAssertEqual(failing.sendCount, 1)
        XCTAssertEqual(instrumentation.events.map(\.name), ["selected", "dispatched", "unreviewed", "failed"])
    }

    func testCancellationStopsTheInFlightRequestAndRecordsTheCaseAsUnreviewed() throws {
        let (session, sender, server) = try serverSession(hang: true)
        let jobID = try selectedJobID(session)
        let before = try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath)

        let running = session.startReview(jobID: jobID)
        XCTAssertTrue(session.pump(until: { server.requests.count == 1 }), "the request reached the server")
        session.service.cancel(jobID: jobID)
        XCTAssertTrue(session.pump(until: { running.value != nil }))

        guard case .completed(let record, nil)? = running.value else { return XCTFail("expected a completed run") }
        XCTAssertEqual(record.outcome, .unreviewed)
        XCTAssertEqual(record.reason, "cancelled")
        XCTAssertEqual(sender.requestsFromCore.count, 1)
        XCTAssertEqual(try ShadowReviewSeed.authoritativeSnapshot(path: session.storePath), before)
        session.service.cancel(jobID: jobID)
    }

    // MARK: route denial and budget

    func testRouteWithoutAReviewGrantNeverSelectsOrSendsEvenWithAnApprovedInterpretationGrant() throws {
        let (session, sender, server) = try serverSession(scenario: {
            var scenario = ShadowReviewScenario(credentialReference: $0)
            scenario.reviewGranted = false
            return scenario
        })

        guard case .skipped(let skip)? = session.offer() else { return XCTFail("expected a skip") }

        XCTAssertEqual(skip.code, "not_authorized")
        XCTAssertNotNil(skip.denial)
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
        assertRedacted(session.eventText)
    }

    func testRevokingTheReviewGrantAfterSelectionDeniesTheDispatchAndRecordsUnreviewed() throws {
        let (session, sender, server) = try serverSession(reply: try anthropicReply(itemType: "action"))
        let jobID = try selectedJobID(session)
        try ShadowReviewSeed.revokeReviewGrant(path: session.storePath)

        guard case .completed(let record, let denial?)? = session.review(jobID: jobID) else {
            return XCTFail("expected a refused run")
        }

        XCTAssertEqual(record.outcome, .unreviewed)
        XCTAssertEqual(record.reason, "dispatch_denied")
        XCTAssertTrue(sender.requestsFromCore.isEmpty, "a refused case is never sent")
        XCTAssertEqual(server.acceptedConnectionCount, 0)
        XCTAssertEqual(keychain.callCount(.read), 0)
        XCTAssertEqual(instrumentation.events.last, .denied(jobID: jobID, code: denial))
        assertRedacted(session.eventText)
    }

    func testSwitchingTheBudgetOffAfterSelectionRefusesTheDispatchThroughTheCore() throws {
        let (session, sender, _) = try serverSession(reply: try anthropicReply(itemType: "action"))
        let jobID = try selectedJobID(session)
        session.service.updateConfiguration(configuration(policy: .disabled))

        guard case .completed(let record, "disabled"?)? = session.review(jobID: jobID) else {
            return XCTFail("expected a refused run")
        }

        XCTAssertEqual(record.outcome, .unreviewed)
        XCTAssertTrue(sender.requestsFromCore.isEmpty)
    }

    func testRequestBudgetBoundsSelectionAndARepeatedOfferReservesNothingMore() throws {
        let failing = FailingSender(.connectionFailed)
        let reference = try storeSecret()
        let session = try open(
            sender: failing, scenario: ShadowReviewScenario(credentialReference: reference),
            configuration: configuration(
                policy: ShadowReviewPolicy(
                    isEnabled: true, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 1,
                    maxAttemptsPerSample: 2)))

        guard case .skipped(let skip)? = session.offer() else { return XCTFail("expected a budget skip") }
        XCTAssertEqual(skip.code, "budget_exhausted")
        XCTAssertEqual(failing.sendCount, 0)

        session.service.updateConfiguration(configuration())
        let jobID = try selectedJobID(session)
        guard case .alreadySelected(let again)? = session.offer() else { return XCTFail("expected the same case") }
        XCTAssertEqual(again.jobID, jobID)
        XCTAssertEqual(failing.sendCount, 0)
    }

    func testAnExhaustedAttemptBudgetEndsTheCaseInsteadOfRetrying() throws {
        let failing = FailingSender(.connectionFailed)
        let reference = try storeSecret()
        let session = try open(
            sender: failing, scenario: ShadowReviewScenario(credentialReference: reference),
            configuration: configuration(
                policy: ShadowReviewPolicy(
                    isEnabled: true, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 4,
                    maxAttemptsPerSample: 1)))
        let jobID = try selectedJobID(session)

        guard case .completed(let record, nil)? = session.review(jobID: jobID) else {
            return XCTFail("expected a completed run")
        }

        XCTAssertEqual(record.outcome, .error)
        XCTAssertEqual(session.review(jobID: jobID), .failed(code: "not_leasable"))
        XCTAssertEqual(failing.sendCount, 1)
    }

    func testATransientFailureStaysPendingAndIsNotRetriedImmediately() throws {
        let failing = FailingSender(.connectionFailed)
        let reference = try storeSecret()
        let session = try open(
            sender: failing, scenario: ShadowReviewScenario(credentialReference: reference),
            configuration: configuration())
        let jobID = try selectedJobID(session)

        guard case .completed(let record, nil)? = session.review(jobID: jobID) else {
            return XCTFail("expected a completed run")
        }

        XCTAssertEqual(record.outcome, .pending)
        XCTAssertEqual(record.attemptsUsed, 1)
        XCTAssertEqual(session.review(jobID: jobID), .failed(code: "not_leasable"), "the retry waits for its backoff")
        XCTAssertEqual(failing.sendCount, 1)
    }

    // MARK: reading

    func testReadingAnUnknownCaseFailsWithNotFound() throws {
        let (session, _, _) = try serverSession()
        XCTAssertEqual(session.record(jobID: "no-such-case"), .failed(code: "not_found"))
    }

    func testReviewWorksWithoutAnyInstrumentation() throws {
        let (session, _, _) = try serverSession(
            reply: try anthropicReply(itemType: "action"), configuration: configuration(observed: false))
        let jobID = try selectedJobID(session)

        guard case .completed(let record, nil)? = session.review(jobID: jobID) else {
            return XCTFail("expected a completed run")
        }
        XCTAssertEqual(record.verdict, .agreement)
        XCTAssertTrue(instrumentation.events.isEmpty)
    }
}

private extension ShadowReviewInstrumentationEvent {
    var name: String {
        switch self {
        case .skipped: return "skipped"
        case .selected: return "selected"
        case .dispatched: return "dispatched"
        case .denied: return "denied"
        case .reviewed: return "reviewed"
        case .unreviewed: return "unreviewed"
        case .failed: return "failed"
        }
    }
}
