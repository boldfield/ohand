import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private struct PrivacyBuiltRoot: @unchecked Sendable {
    let root: ServiceCompositionRoot
    let coordinator: LifecycleCoordinator
    let search: RetrievalSearchModule
}

private final class PrivacySearchOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<RetrievalPage, RetrievalSearchFailure>?

    var value: Result<RetrievalPage, RetrievalSearchFailure>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<RetrievalPage, RetrievalSearchFailure>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

private final class PrivacySaveOutcomeBox: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: CompositionSaveOutcome?

    var value: CompositionSaveOutcome? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ outcome: CompositionSaveOutcome) {
        lock.lock()
        stored = outcome
        lock.unlock()
    }
}

/// Drives `PrivacySettingsModel` over the real composition root and Rust core on an on-disk store: the core's own route
/// provisioning, grant derivation and policy listing.
@MainActor
final class PrivacySettingsRealCoreTests: XCTestCase {
    private static let interpretableText = "Remind me 2099-01-15 11:00:00 to call the roofer about the quarterly estimate"

    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var world: RealCoreRetentionWorld!
    private var session: ReadAuthenticationSession!
    private var root: ServiceCompositionRoot!
    private var coordinator: LifecycleCoordinator!
    private var keychain: FakeKeychain!
    private var credentialService: CredentialService!
    private var sender: PrivacyUnreachableSender!
    /// When set, the root sends through this instead: it holds each request until released, then answers it validly.
    private var heldSender: PrivacyHeldReplySender?
    private var searchModule: RetrievalSearchModule!

    override func setUpWithError() throws {
        try super.setUpWithError()
        keychain = FakeKeychain()
        sender = PrivacyUnreachableSender()
        credentialService = CredentialService(
            serviceName: "com.boldfield.ohand.tests.privacy-settings.\(UUID().uuidString)", keychain: keychain)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        world = try RealCoreRetentionWorld()
        session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        session.authenticate(reason: "test") { _ in }
        XCTAssertEqual(session.scope, .authenticated)
    }

    override func tearDown() {
        root?.shutDown()
        root = nil
        world.tearDown()
        world = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        super.tearDown()
    }

    /// Builds the root off the main thread, as the other real-core suites do: its route provisioning blocks until the
    /// core answers.
    private func makeModel() async throws -> PrivacySettingsModel {
        let layoutPath = world.layout.rootDirectory.path
        let readSession = session!
        let credentials = credentialService!
        let requestSender: ProviderRequestSender = (heldSender as ProviderRequestSender?) ?? sender!
        let built = try await Task.detached { () -> PrivacyBuiltRoot in
            let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = TimeZone(identifier: "UTC")!
            let search = RetrievalSearchModule(calendar: calendar)
            let root = try ServiceCompositionRoot(
                testing: CompositionConfiguration(
                    storageRootPath: layoutPath, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
                environment: CompositionEnvironment(
                    timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
                adapters: .injectedForTests(
                    sender: requestSender, readSession: readSession,
                    notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true),
                    credentials: credentials),
                modules: [coordinator, search])
            try coordinator.connect(to: root)
            return PrivacyBuiltRoot(root: root, coordinator: coordinator, search: search)
        }.value
        root = built.root
        coordinator = built.coordinator
        searchModule = built.search
        return PrivacySettingsModel.assembled(root: root)
    }

    private func execute(_ statement: String) {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(world.databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, statement, nil, nil, &message)
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK)
    }

    /// Closes the root and opens a new one over the same on-disk store, as a relaunch does.
    private func relaunch() async throws -> PrivacySettingsModel {
        root.shutDown()
        root = nil
        return try await makeModel()
    }

    private func search(_ query: String) async throws -> RetrievalPage {
        let box = PrivacySearchOutcomeBox()
        searchModule.search(RetrievalSearchRequest(query: query)) { box.set($0) }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        return try XCTUnwrap(box.value, "the search completed").get()
    }

    private func scalar(_ query: String) -> String {
        var database: OpaquePointer?
        guard sqlite3_open_v2(world.databasePath, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { return "" }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, query, -1, &statement, nil) == SQLITE_OK else { return "" }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    private func waitUntil(timeout: TimeInterval = 30, _ condition: () -> Bool) async throws -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline { try await Task.sleep(nanoseconds: 50_000_000) }
        return condition()
    }

    /// Starts the job runner in the foreground and drains it, as a launch does.
    private func startProcessing() {
        root.start(isActive: true)
        root.jobRunner.requestDrain()
    }

    /// A profile that is the `text_interpretation` default with a stored credential bound, so a new capture's job is
    /// pinned to it and only a missing grant can stop its dispatch.
    private func createDispatchableDefault() async throws -> String {
        let reference = try root.providerCredentials.addCredential(Data("synthetic-key-0001".utf8))
        let created = try await root.providerConfiguration.createProfile(
            ProviderProfileDraft(
                profileID: "hosted", providerProtocol: "anthropic", endpoint: nil, model: "synthetic-model",
                credentialReference: reference, timeoutSeconds: 30))
        try await root.providerConfiguration.setDefault(
            capability: "text_interpretation", profileVersion: created.profileVersion)
        return created.profileVersion
    }

    private static func interpretationReply() throws -> Data {
        let phraseStart = 10
        let phraseEnd = phraseStart + "2099-01-15 11:00:00".count
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": interpretableText.count]],
            "reminder_proposal": [
                "instant": "2099-01-15T11:00:00Z",
                "timezone_id": "UTC",
                "quality": "explicit",
                "source_span": ["start": phraseStart, "end": phraseEnd],
            ],
        ]
        let reply: [String: Any] = [
            "id": "msg_privacy_settings_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_privacy_settings_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }

    private func saveCapture(_ text: String = "Synthetic capture text") async throws {
        let box = PrivacySaveOutcomeBox()
        root.saveText(captureID: UUID().uuidString.lowercased(), text: text, capturedAt: Date()) {
            box.set($0)
        }
        let deadline = Date().addingTimeInterval(30)
        while box.value == nil && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        guard case .saved? = box.value else {
            XCTFail("expected a confirmed save, got \(String(describing: box.value))")
            return
        }
    }

    private func createProfile(_ name: String, providerProtocol: String, endpoint: String? = nil) async throws -> String {
        let created = try await root.providerConfiguration.createProfile(
            ProviderProfileDraft(
                profileID: name, providerProtocol: providerProtocol, endpoint: endpoint, model: "synthetic-model",
                credentialReference: "cred-\(name)", timeoutSeconds: 30))
        return created.profileVersion
    }

    func testFreshInstallCapturesBeforeAnyProviderSetupAndEveryRouteIsLocalOnly() async throws {
        let model = try await makeModel()
        try await saveCapture("synthetic roofer words")
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly)
        XCTAssertEqual(model.routes.map(\.routeID), ["route-personal"])
        XCTAssertTrue(model.offeredVersions(for: "text_interpretation").isEmpty)

        let page = try await search("roofer")
        XCTAssertEqual(page.totalAccessible, 1, "the capture is retrieved with no provider profile saved")
        XCTAssertEqual(page.rows.first?.text, "synthetic roofer words")
        XCTAssertEqual(sender.attemptCount, 0, "capture and search asked no provider anything")
        await model.refresh()
        XCTAssertTrue(model.isLocalOnly, "capture and search authorized nothing")
    }

    func testAConfirmedGrantAndTheRouteLabelsAreStickyAcrossARelaunch() async throws {
        var model = try await makeModel()
        let version = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        let before = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        guard case .allowed = model.actionState else { return XCTFail("expected allowed") }

        model = try await relaunch()
        await model.refresh()

        XCTAssertEqual(model.phase, .loaded)
        let after = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(after.routeName, before.routeName)
        XCTAssertEqual(after.scope, before.scope)
        XCTAssertEqual(after.grantedCapabilities.map(\.capability), ["text_interpretation"])
        XCTAssertEqual(after.grantedCapabilities.map(\.destinationClasses), [["cloud"]])
    }

    func testARevocationWhileWorkIsQueuedStopsDispatchAndSurvivesARelaunch() async throws {
        var model = try await makeModel()
        let version = try await createDispatchableDefault()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))

        // Work is queued behind the grant; revoking before the runner starts means it cannot be sent.
        try await saveCapture(Self.interpretableText)
        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed = model.actionState else { return XCTFail("expected changed, got \(String(describing: model.actionState))") }
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
        XCTAssertTrue(model.isLocalOnly)

        startProcessing()
        // The core denies the job at authorization and holds it for configuration; wait for that stored reason.
        let denied = try await waitUntil { self.scalar("SELECT failure_reason FROM jobs LIMIT 1") == "unauthorized" }
        XCTAssertTrue(denied, "the job was denied: \(jobStatus())")
        XCTAssertNotEqual(jobStatus(), "completed")
        XCTAssertEqual(sender.attemptCount, 0, "a revoked capability sends nothing, even for work queued before it")
        await model.refresh()
        XCTAssertEqual(
            model.followedWork.map(\.state),
            [.settled(PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: "awaiting_configuration"))],
            "the screen shows the core's refusal of the work that was queued at the change")

        model = try await relaunch()
        await model.refresh()
        XCTAssertEqual(model.phase, .loaded)
        XCTAssertTrue(model.isLocalOnly, "the revocation survives a relaunch")
        XCTAssertEqual(sender.attemptCount, 0)
    }

    private func jobStatus() -> String { scalar("SELECT status FROM jobs LIMIT 1") }

    /// A dispatchable profile (credential bound), optionally the text-interpretation default.
    private func createDispatchableProfile(_ name: String, makeDefault: Bool) async throws -> String {
        let reference = try root.providerCredentials.addCredential(Data("synthetic-key-\(name)".utf8))
        let created = try await root.providerConfiguration.createProfile(
            ProviderProfileDraft(
                profileID: name, providerProtocol: "anthropic", endpoint: nil, model: "synthetic-model",
                credentialReference: reference, timeoutSeconds: 30))
        if makeDefault {
            try await root.providerConfiguration.setDefault(
                capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        return created.profileVersion
    }

    /// Grants, saves a capture and lets the runner send it, leaving the request held in flight.
    private func startHeldRequest(_ held: PrivacyHeldReplySender) async throws -> (PrivacySettingsModel, String) {
        let model = try await makeModel()
        let version = try await createDispatchableDefault()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        try await saveCapture(Self.interpretableText)
        startProcessing()
        let sent = try await waitUntil { held.attemptCount == 1 }
        XCTAssertTrue(sent, "the request left the device: \(jobStatus())")
        XCTAssertEqual(jobStatus(), "running")
        await model.refresh()
        XCTAssertEqual(model.queuedWork.flatMap(\.jobs.jobs).map(\.status), ["running"], "the screen shows the sent request")
        return (model, version)
    }

    func testARouteRevocationWhileARequestIsInFlightDiscardsTheLateReplyAndTheScreenSaysSo() async throws {
        let held = PrivacyHeldReplySender(reply: try Self.interpretationReply())
        heldSender = held
        defer { held.release() }
        let (model, _) = try await startHeldRequest(held)

        model.proposeRevoke(routeID: "route-personal", capability: "text_interpretation")
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed, got \(String(describing: model.actionState))") }
        XCTAssertTrue(message.contains("Stopped"), message)
        XCTAssertTrue(message.contains("A request already sent cannot be recalled"), message)
        XCTAssertFalse(message.contains("is not sent"), "the request already left the device: \(message)")
        XCTAssertFalse(message.contains("not applied"), "the message makes no per-job claim it cannot attribute: \(message)")
        let jobID = scalar("SELECT job_id FROM jobs LIMIT 1")
        XCTAssertEqual(model.followedWork.map(\.jobID), [jobID], "the request in flight is followed")
        XCTAssertEqual(model.followedWork.first?.state, .listed("running"))
        XCTAssertTrue(
            PrivacySettingsText.followedJob(try XCTUnwrap(model.followedWork.first)).contains("request already sent"))

        held.release()
        let settled = try await waitUntil { self.jobStatus() != "running" }
        XCTAssertTrue(settled, "the late reply was handled: \(jobStatus())")
        XCTAssertNotEqual(jobStatus(), "completed", "a reply after the revocation does not complete the job")
        XCTAssertEqual(scalar("SELECT failure_reason FROM jobs LIMIT 1"), "unauthorized", "the core denied the late reply")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM proposals"), "0", "nothing was applied")
        XCTAssertNotEqual(scalar("SELECT processing_state FROM items LIMIT 1"), "processed")

        await model.refresh()
        XCTAssertEqual(model.routesHoldingWork, ["Personal route · route-personal"])
        XCTAssertFalse(
            model.queuedWork.flatMap(\.jobs.jobs).contains { $0.status == "running" }, "the screen shows no request as sent")
        // The screen reads the settled job back through the core's own item status: refused, nothing applied.
        let followed = try XCTUnwrap(model.followedWork.first)
        XCTAssertEqual(followed.jobID, jobID)
        XCTAssertEqual(
            followed.state,
            .settled(PrivacyItemProcessing(processingState: "unprocessed", processingJobStatus: "awaiting_configuration")))
        let shown = PrivacySettingsText.followedJob(followed)
        XCTAssertTrue(shown.contains("refused"), shown)
        XCTAssertTrue(shown.contains("Nothing was applied"), shown)
    }

    func testAProfileRevocationWhileARequestIsInFlightRetiresTheJobAndTheLateReplyIsNotApplied() async throws {
        let held = PrivacyHeldReplySender(reply: try Self.interpretationReply())
        heldSender = held
        defer { held.release() }
        let (model, version) = try await startHeldRequest(held)
        let jobID = scalar("SELECT job_id FROM jobs LIMIT 1")

        model.proposeRevokeProfile(profileVersion: version)
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed, got \(String(describing: model.actionState))") }
        XCTAssertTrue(message.contains("1 job retired"), message)
        XCTAssertTrue(message.contains("1 had already been sent"), message)
        XCTAssertEqual(jobStatus(), "cancelled")
        XCTAssertEqual(scalar("SELECT failure_reason FROM jobs LIMIT 1"), "profile_revoked")
        XCTAssertEqual(model.retiredWork.first?.jobs.map(\.jobID), [jobID], "the retired job stays open to an explicit move")
        XCTAssertTrue(model.queuedWork.isEmpty)

        held.release()
        let returned = try await waitUntil { held.returnedCount == 1 }
        XCTAssertTrue(returned)
        try await Task.sleep(nanoseconds: 1_000_000_000)
        XCTAssertEqual(jobStatus(), "cancelled", "a late reply cannot bring the job back")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM proposals"), "0", "nothing was applied")
        XCTAssertNotEqual(scalar("SELECT processing_state FROM items LIMIT 1"), "processed")
    }

    func testRetiredWorkIsMovedOnPurposeAndTheCoresLinkIsInspectableAndIdempotent() async throws {
        let model = try await makeModel()
        let version = try await createDispatchableDefault()
        let other = try await createDispatchableProfile("other", makeDefault: false)
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        try await saveCapture(Self.interpretableText)
        let jobID = scalar("SELECT job_id FROM jobs LIMIT 1")
        await model.refresh()
        XCTAssertEqual(model.queuedWork.flatMap(\.jobs.jobs).map(\.jobID), [jobID])

        model.proposeRevokeProfile(profileVersion: version)
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let message) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(message.contains("1 job retired"), message)
        XCTAssertEqual(scalar("SELECT failure_reason FROM jobs WHERE job_id = '\(jobID)'"), "profile_revoked")
        XCTAssertTrue(model.queuedWork.isEmpty)

        model.proposeRequeue(jobID: jobID, toProfileVersion: other)
        let shown = try XCTUnwrap(model.requeueProposal, "the retired job can be moved on purpose")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM job_requeues"), "0", "opening the question moves nothing")
        await model.confirmRequeue(shown)
        guard case .changed = model.actionState else { return XCTFail("expected changed, got \(String(describing: model.actionState))") }
        let record = try XCTUnwrap(model.lastRequeue, "the core's link is shown")
        XCTAssertEqual(record.oldJobID, jobID)
        XCTAssertEqual(record.fromProfileVersion, version)
        XCTAssertEqual(record.toProfileVersion, other)
        XCTAssertFalse(model.requeueRecordUnreadable)
        XCTAssertEqual(
            scalar("SELECT old_job_id || '|' || new_job_id || '|' || from_profile_version || '|' || to_profile_version FROM job_requeues"),
            "\(jobID)|\(record.newJobID)|\(version)|\(other)", "the shown link is the core's stored row")
        XCTAssertEqual(scalar("SELECT profile_version FROM jobs WHERE job_id = '\(record.newJobID)'"), other)

        let repeated = try await root.jobConfiguration.requeue(jobID: jobID, newJobID: shown.newJobID, toProfileVersion: other)
        XCTAssertEqual(repeated.jobID, record.newJobID, "repeating the same action answers the same replacement")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM job_requeues"), "1")
    }

    func testRepinMovesNothingWithoutAnApprovedProviderThenMovesWaitingWorkOnceOneIsSelected() async throws {
        let model = try await makeModel()
        let version = try await createDispatchableProfile("hosted", makeDefault: false)
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))
        try await saveCapture(Self.interpretableText)
        startProcessing()
        let waiting = try await waitUntil { self.jobStatus() == "failed" }
        XCTAssertTrue(waiting, "with no default selected the job stops for configuration: \(jobStatus())")

        model.proposeRepinWaitingWork(routeID: "route-personal")
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let none) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(none.hasPrefix("Nothing was moved"), none)
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM jobs"), "1")
        XCTAssertEqual(model.phase, .loaded, "the app stays usable with no approved provider")
        try await saveCapture("Synthetic second capture")

        try await root.providerConfiguration.setDefault(capability: "text_interpretation", profileVersion: version)
        await model.refresh()
        model.proposeRepinWaitingWork(routeID: "route-personal")
        await model.confirmChange(try XCTUnwrap(model.change))
        guard case .changed(let moved) = model.actionState else { return XCTFail("expected changed") }
        XCTAssertTrue(moved.hasPrefix("Queued"), moved)
        XCTAssertGreaterThanOrEqual(Int(scalar("SELECT COUNT(*) FROM jobs WHERE profile_version = '\(version)'")) ?? 0, 1)
        XCTAssertGreaterThanOrEqual(Int(scalar("SELECT COUNT(*) FROM job_requeues")) ?? 0, 1, "the core recorded the move")
    }

    func testProcessingACaptureCreatesNoGrantAndSendsNothingWithoutOne() async throws {
        let model = try await makeModel()
        let version = try await createDispatchableDefault()
        await model.refresh()
        let before = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertTrue(before.grantedCapabilities.isEmpty)

        try await saveCapture(Self.interpretableText)
        startProcessing()
        let settled = try await waitUntil {
            let status = self.scalar("SELECT status FROM jobs LIMIT 1")
            return status == "completed" || status == "failed" || status == "dead"
        }
        XCTAssertTrue(settled, "processing ran and the job settled: \(scalar("SELECT status FROM jobs LIMIT 1"))")
        XCTAssertNotEqual(
            scalar("SELECT profile_version FROM jobs LIMIT 1"), version, "without a grant the job was not pinned to the profile")
        await model.refresh()

        XCTAssertTrue(model.isLocalOnly, "processing a capture created no grant")
        XCTAssertEqual(sender.attemptCount, 0, "no request reached a provider without a grant")
    }

    func testAGrantOnOneCapabilityLeavesTheOtherUngrantedAfterProcessing() async throws {
        let model = try await makeModel()
        let version = try await createDispatchableDefault()
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        await model.confirm(try XCTUnwrap(model.proposal))

        try await saveCapture(Self.interpretableText)
        startProcessing()
        let attempted = try await waitUntil { self.sender.attemptCount > 0 }
        XCTAssertTrue(attempted, "the granted capability's request was attempted: \(scalar("SELECT status || '/' || profile_version FROM jobs LIMIT 1"))")
        XCTAssertNotEqual(scalar("SELECT failure_reason FROM jobs LIMIT 1"), "unauthorized")
        await model.refresh()

        let personal = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(personal.grantedCapabilities.map(\.capability), ["text_interpretation"])
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "review"))
    }

    func testCancellingSetupLeavesPolicyUnchangedAndCaptureUsable() async throws {
        let model = try await makeModel()
        let version = try await createProfile("hosted", providerProtocol: "open_ai")
        await model.refresh()
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: version)
        XCTAssertNotNil(model.proposal)
        model.cancel()
        await model.refresh()
        XCTAssertTrue(model.isLocalOnly, "the core holds no grant after a cancelled setup")
        try await saveCapture()
    }

    func testAcceptingOneRouteAndDestinationAuthorizesNoOther() async throws {
        let model = try await makeModel()
        execute(
            "INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) "
                + "VALUES ('route-work', 'Work notes', 'work', '[]', '2026-10-01T07:00:00Z')")
        let hosted = try await createProfile("hosted", providerProtocol: "open_ai")
        let privateServer = try await createProfile(
            "private", providerProtocol: "self_hosted", endpoint: "https://private.example/v1")
        await model.refresh()
        XCTAssertEqual(Set(model.routes.map(\.routeID)), ["route-personal", "route-work"])

        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: hosted)
        await model.confirm(try XCTUnwrap(model.proposal))
        guard case .allowed = model.actionState else { return XCTFail("expected allowed, got \(String(describing: model.actionState))") }

        XCTAssertTrue(model.isGranted(routeID: "route-personal", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-work", capability: "text_interpretation"))
        XCTAssertFalse(model.isGranted(routeID: "route-personal", capability: "review"))
        let personal = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(personal.grantedCapabilities.map(\.destinationClasses), [["cloud"]])

        // The private server version has no grant of its own: proposing and cancelling it changes nothing.
        model.propose(routeID: "route-personal", capability: "text_interpretation", profileVersion: privateServer)
        XCTAssertNotNil(model.proposal)
        model.cancel()
        await model.refresh()
        let afterCancel = try XCTUnwrap(model.routes.first { $0.routeID == "route-personal" })
        XCTAssertEqual(afterCancel.grantedCapabilities.map(\.destinationClasses), [["cloud"]])
        let work = try XCTUnwrap(model.routes.first { $0.routeID == "route-work" })
        XCTAssertTrue(work.grantedCapabilities.isEmpty)
    }
}

/// No request ever leaves the test.
private final class PrivacyUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private var attempts = 0

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        attempts += 1
        lock.unlock()
        throw ProviderTransportError.connectionFailed
    }
}

/// Holds every request until released, then answers it with one valid reply, so a test controls when a late result
/// arrives.
private final class PrivacyHeldReplySender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private let reply: Data
    private var released = false
    private var attempts = 0
    private var returned = 0

    init(reply: Data) { self.reply = reply }

    var attemptCount: Int { lock.withLock { attempts } }
    var returnedCount: Int { lock.withLock { returned } }

    func release() { lock.withLock { released = true } }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.withLock { attempts += 1 }
        while !lock.withLock({ released }) {
            if Task.isCancelled { throw ProviderTransportError.cancelled }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        lock.withLock { returned += 1 }
        return ProviderHTTPResponse(status: 200, headers: ["Content-Type": "application/json"], body: reply)
    }
}
