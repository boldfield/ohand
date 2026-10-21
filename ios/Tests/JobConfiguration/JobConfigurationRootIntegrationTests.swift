import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers every provider request with one scripted reply and records the requests the core built.
private final class RecordingProviderSender: ProviderRequestSender, @unchecked Sendable {
    private let lock = NSLock()
    private let reply: Data
    private var recorded: [ProviderHTTPRequest] = []

    init(reply: Data) {
        self.reply = reply
    }

    var requests: [ProviderHTTPRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        recorded.append(request)
        lock.unlock()
        return ProviderHTTPResponse(status: 200, headers: ["Content-Type": "application/json"], body: reply)
    }
}

private final class ConfigurationResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ newValue: Result<Value, Error>) {
        lock.lock()
        stored = newValue
        lock.unlock()
    }
}

/// Drives `ServiceCompositionRoot.jobConfiguration` into the real Rust core over an on-disk store. Profiles, defaults and
/// grants are written through a second core handle on the same store, the way the settings flows do, so the root's own
/// handle is exercised only by the controls under test and by the job runner.
final class JobConfigurationRootIntegrationTests: XCTestCase {
    private static let routeID = "route-job-configuration"
    private static let captureText = "Remind me 2099-01-15 11:00:00 to call the roofer about the quarterly estimate"
    private static let credentialReference = "synthetic-credential"
    private static let requestVersion = "5a1c0000-0000-4000-8000-0000000000a3"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var configurationHandle: CoreHandle?
    private var configurationEvents: [CoreEvent] = []
    private var nextConfigurationOperationID: UInt64 = 1

    override func setUpWithError() throws {
        try super.setUpWithError()
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("job-configuration-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        configurationHandle?.close()
        configurationHandle = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
        try super.tearDownWithError()
    }

    // MARK: tests

    func testWorkCapturedWithNoProviderMovesOnlyWhenTheUserRepinsAndThenDispatches() throws {
        let sender = RecordingProviderSender(reply: try anthropicReply())
        let root = try makeRoot(authenticated: true, sender: sender)
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let waitingJobID = scalar("SELECT job_id FROM jobs WHERE item_id = '\(itemID)'")
        XCTAssertFalse(waitingJobID.isEmpty, "the capture queued an unpinned interpretation job")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM jobs WHERE profile_version IS NOT NULL"), "0")
        root.start(isActive: true)
        root.jobRunner.requestDrain()
        XCTAssertTrue(
            pump(until: { self.scalar("SELECT status FROM jobs WHERE job_id = '\(waitingJobID)'") == "failed" }),
            "the unpinned job stops for configuration: \(scalar("SELECT status FROM jobs WHERE job_id = '\(waitingJobID)'"))")
        XCTAssertEqual(scalar("SELECT processing_state FROM items WHERE item_id = '\(itemID)'"), "unprocessed")
        XCTAssertTrue(sender.requests.isEmpty, "nothing reached a provider without a configured profile")

        let profileVersion = try createDefaultProfile(profileID: "profile-a")
        try grant(profileVersion: profileVersion)
        root.jobRunner.requestDrain()
        _ = pump(until: { false }, timeout: 1)
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM jobs WHERE item_id = '\(itemID)'"), "1",
            "configuring and granting never moves waiting work by themselves")
        XCTAssertTrue(sender.requests.isEmpty)

        let jobConfiguration = root.jobConfiguration
        let moved = try run { try await jobConfiguration.repinWaitingWork(routeID: Self.routeID) }
        XCTAssertEqual(moved, 1)
        let link = try run { try await jobConfiguration.requeueRecord(forJob: waitingJobID) }
        XCTAssertNil(link?.fromProfileVersion)
        XCTAssertEqual(link?.toProfileVersion, profileVersion)

        root.jobRunner.requestDrain()
        XCTAssertTrue(
            pump(until: { !sender.requests.isEmpty }, timeout: 30), "the re-pinned job dispatches to the provider")
        XCTAssertEqual(sender.requests.first?.url.host, "api.anthropic.com")
        XCTAssertTrue(
            pump(until: {
                self.scalar("SELECT processing_state FROM items WHERE item_id = '\(itemID)'") == "processed"
            }, timeout: 30), "the provider reply is applied to the item")
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM jobs WHERE item_id = '\(itemID)' AND profile_version = '\(profileVersion)'"),
            "1")
        XCTAssertEqual(
            scalar("SELECT status FROM jobs WHERE job_id = '\(waitingJobID)'"), "failed",
            "the waiting job is left as it stopped")

        let again = try run { try await jobConfiguration.repinWaitingWork(routeID: Self.routeID) }
        XCTAssertEqual(again, 0, "re-pinning again moves nothing")
    }

    func testARouteWithoutAGrantMovesNothing() throws {
        let sender = RecordingProviderSender(reply: try anthropicReply())
        let root = try makeRoot(authenticated: true, sender: sender)
        defer { root.shutDown() }
        _ = try saveCapture(root)
        _ = try createDefaultProfile(profileID: "profile-a")
        let jobConfiguration = root.jobConfiguration

        let moved = try run { try await jobConfiguration.repinWaitingWork(routeID: Self.routeID) }

        XCTAssertEqual(moved, 0)
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM jobs WHERE profile_version IS NOT NULL"), "0",
            "no grant means no job is pinned")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM jobs"), "1", "only the capture's own unpinned job exists")
    }

    func testAJobQueuedBetweenAPreReadAndTheRevokeAppearsInThePostRevokeList() throws {
        let root = try makeRoot(authenticated: true, sender: RecordingProviderSender(reply: Data()))
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let profileVersion = try createProfile(profileID: "profile-a")
        let replacementVersion = try createProfile(profileID: "profile-b")
        func insertJob(_ jobID: String, revision: Int) -> String {
            """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES ('\(jobID)', 1, '\(itemID)', \
            'interpret', \(revision), '\(profileVersion)', '\(Self.requestVersion)', 'queued', 0, \
            '2026-10-08T09:30:0\(revision)Z')
            """
        }
        try execute([insertJob("job-before", revision: 1)])
        let jobConfiguration = root.jobConfiguration

        let preRead = try run { try await jobConfiguration.jobsPinned(toProfile: profileVersion) }
        XCTAssertEqual(preRead.jobs.map(\.jobID), ["job-before"])
        try execute([insertJob("job-in-window", revision: 2)])
        try run { try await jobConfiguration.revokeProfile(profileVersion) }
        _ = try run {
            try await jobConfiguration.requeue(
                jobID: "job-in-window", newJobID: "job-replacement", toProfileVersion: replacementVersion)
        }

        let retired = try run { try await jobConfiguration.jobsRetired(byRevocationOf: profileVersion) }
        XCTAssertEqual(retired.jobs.map(\.jobID), ["job-before", "job-in-window"])
        XCTAssertEqual(retired.total, 2)
        XCTAssertFalse(retired.truncated)
        XCTAssertNil(retired.jobs[0].requeuedTo)
        XCTAssertEqual(retired.jobs[1].requeuedTo, "job-replacement")
        XCTAssertTrue(retired.jobs.allSatisfy { $0.itemID == itemID })
    }

    func testACaptureOnlySessionCannotRepinAndNothingChanges() throws {
        let root = try makeRoot(authenticated: false, sender: RecordingProviderSender(reply: Data()))
        defer { root.shutDown() }
        _ = try saveCapture(root)
        let profileVersion = try createDefaultProfile(profileID: "profile-a")
        try grant(profileVersion: profileVersion)
        let jobConfiguration = root.jobConfiguration

        XCTAssertThrowsError(try run { try await jobConfiguration.repinWaitingWork(routeID: Self.routeID) }) { error in
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        XCTAssertThrowsError(try run { try await jobConfiguration.revokeProfile(profileVersion) }) { error in
            XCTAssertEqual(error as? JobConfigurationError, .notAuthenticated)
        }
        XCTAssertEqual(
            scalar("SELECT COUNT(*) FROM jobs WHERE profile_version IS NOT NULL"), "0",
            "a capture-only session pins nothing")
        XCTAssertEqual(scalar("SELECT COUNT(*) FROM provider_profiles WHERE revoked_at IS NOT NULL"), "0")
    }

    func testListingRequeueAndRevocationGoThroughTheRealCore() throws {
        let root = try makeRoot(authenticated: true, sender: RecordingProviderSender(reply: Data()))
        defer { root.shutDown() }
        let itemID = try saveCapture(root)
        let first = try createProfile(profileID: "profile-a")
        let second = try createProfile(profileID: "profile-b")
        try execute([
            """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES ('job-pinned-1', 1, '\(itemID)', \
            'interpret', 0, '\(first)', '\(Self.requestVersion)', 'queued', 0, '2026-10-08T09:30:02Z')
            """
        ])
        let jobConfiguration = root.jobConfiguration

        let listing = try run { try await jobConfiguration.jobsPinned(toProfile: first) }
        XCTAssertEqual(
            listing, JobConfigurationPinnedJobs(
                jobs: [JobConfigurationPinnedJob(jobID: "job-pinned-1", itemID: itemID, status: "queued")], total: 1,
                truncated: false))

        let replacement = try run {
            try await jobConfiguration.requeue(jobID: "job-pinned-1", newJobID: "job-pinned-2", toProfileVersion: second)
        }
        XCTAssertEqual(replacement.jobID, "job-pinned-2")
        XCTAssertEqual(replacement.itemID, itemID)
        XCTAssertEqual(replacement.profileVersion, second)
        let replayed = try run {
            try await jobConfiguration.requeue(jobID: "job-pinned-1", newJobID: "job-pinned-2", toProfileVersion: second)
        }
        XCTAssertEqual(replayed, replacement, "repeating the same user action answers the existing replacement")
        let link = try run { try await jobConfiguration.requeueRecord(forJob: "job-pinned-1") }
        XCTAssertEqual(link?.newJobID, "job-pinned-2")
        XCTAssertEqual(link?.fromProfileVersion, first)
        XCTAssertEqual(link?.toProfileVersion, second)
        let unlinked = try run { try await jobConfiguration.requeueRecord(forJob: "job-never-requeued") }
        XCTAssertNil(unlinked)
        let afterListing = try run { try await jobConfiguration.jobsPinned(toProfile: first) }
        XCTAssertTrue(afterListing.jobs.isEmpty)

        try run { try await jobConfiguration.revokeProfile(second) }
        XCTAssertNotEqual(scalar("SELECT revoked_at FROM provider_profiles WHERE profile_version = '\(second)'"), "")
        XCTAssertThrowsError(try run {
            try await jobConfiguration.requeue(jobID: "job-pinned-2", newJobID: "job-pinned-3", toProfileVersion: second)
        }) { error in
            XCTAssertEqual(error as? JobConfigurationError, .refused, "a revoked profile is not a target")
        }
        XCTAssertThrowsError(try run { try await jobConfiguration.revokeProfile("no-such-profile-version") }) { error in
            XCTAssertEqual(error as? JobConfigurationError, .notFound)
        }
        XCTAssertThrowsError(try run {
            try await jobConfiguration.requeue(jobID: "no-such-job", newJobID: "job-x", toProfileVersion: first)
        }) { error in
            XCTAssertEqual(error as? JobConfigurationError, .notFound)
        }
    }

    func testTheJobConfigurationRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == ServiceCompositionRoot.jobConfigurationClientName })
        XCTAssertEqual(mine.start, CoreJobConfigurationClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    // MARK: configuration through a second handle

    private func configurationCall(_ start: (CoreHandle, UInt64) throws -> Void) throws -> CoreEvent {
        let handle: CoreHandle
        if let existing = configurationHandle {
            handle = existing
        } else {
            handle = try CoreHandle(path: databasePath)
            try handle.setEventHandler { [unowned self] event in self.configurationEvents.append(event) }
            configurationHandle = handle
        }
        let operationID = nextConfigurationOperationID
        nextConfigurationOperationID += 1
        try start(handle, operationID)
        XCTAssertTrue(
            pump(until: { self.configurationEvents.contains { $0.operationID == operationID } }),
            "the configuration call completes")
        return try XCTUnwrap(configurationEvents.first { $0.operationID == operationID })
    }

    private func createProfile(profileID: String) throws -> String {
        let draft = ProviderProfileDraft(
            profileID: profileID, providerProtocol: "anthropic", model: "synthetic-model",
            credentialReference: Self.credentialReference, timeoutSeconds: 30)
        let event = try configurationCall { handle, operationID in
            try handle.startProviderProfiles(operationID: operationID, command: .createProfile(draft))
        }
        return try event.decode(ProviderProfileCreatedAnswer.self).profileVersion
    }

    private func createDefaultProfile(profileID: String) throws -> String {
        let profileVersion = try createProfile(profileID: profileID)
        let event = try configurationCall { handle, operationID in
            try handle.startProviderProfiles(
                operationID: operationID,
                command: .setDefault(capability: "text_interpretation", profileVersion: profileVersion))
        }
        _ = try event.outcome.get()
        return profileVersion
    }

    private func grant(profileVersion: String) throws {
        let event = try configurationCall { handle, operationID in
            try handle.startGrantRouteCapability(
                operationID: operationID, routeID: Self.routeID, capability: "text_interpretation",
                profileVersion: profileVersion)
        }
        _ = try event.outcome.get()
    }

    // MARK: helpers

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    private func makeRoot(authenticated: Bool, sender: ProviderRequestSender) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        if authenticated {
            session.authenticate(reason: "test") { _ in }
            XCTAssertEqual(session.scope, .authenticated)
        }
        return try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)))
    }

    private func saveCapture(_ root: ServiceCompositionRoot) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: Self.captureText, capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 30) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func run<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = ConfigurationResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump(until: { box.value != nil }), "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    private func execute(_ statements: [String]) throws {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw CompositionError.storageUnavailable }
        }
    }

    private func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(databasePath, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        XCTAssertEqual(sqlite3_prepare_v2(database, sql, -1, &statement, nil), SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    private func anthropicReply() throws -> Data {
        let phraseStart = 10
        let phraseEnd = phraseStart + "2099-01-15 11:00:00".count
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": Self.captureText.count]],
            "reminder_proposal": [
                "instant": "2099-01-15T11:00:00Z",
                "timezone_id": "UTC",
                "quality": "explicit",
                "source_span": ["start": phraseStart, "end": phraseEnd],
            ],
        ]
        let reply: [String: Any] = [
            "id": "msg_job_configuration_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_job_configuration_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }
}
