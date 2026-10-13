import Foundation
import SQLite3
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// Answers every provider request with one scripted reply and records the requests the core built.
private final class ScriptedProviderSender: ProviderRequestSender, @unchecked Sendable {
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

/// An optional capability joined through the seam: it owns a job type and an operation range and asks the core one
/// question so the test can see its events arrive.
private final class RecordingModule: CompositionModule {
    let identifier: String
    let firstOperationID: UInt64
    let jobCapabilities: [JobCapabilityHandler]
    private(set) var context: CompositionContext?
    private(set) var receivedEvents: [CoreEvent] = []
    private(set) var isShutDown = false

    init(identifier: String, firstOperationID: UInt64, jobTypes: [String] = []) {
        self.identifier = identifier
        self.firstOperationID = firstOperationID
        jobCapabilities = jobTypes.map { ScriptedCapability(jobType: $0, result: .settled) }
    }

    func attach(to context: CompositionContext) throws {
        self.context = context
    }

    func handle(_ event: CoreEvent) -> Bool {
        guard event.operationID == firstOperationID else { return false }
        receivedEvents.append(event)
        return true
    }

    func shutDown() {
        isShutDown = true
    }

    func askCore() throws {
        try context?.core.startItemStatus(operationID: firstOperationID, itemID: "no-such-item")
    }
}

final class CompositionRootTests: XCTestCase {
    private static let routeID = "route-composition"
    private static let profileVersion = "4b7a1bfe-88ff-4ba7-848e-a45069cf9081"
    private static let requestVersion = "5a1c0000-0000-4000-8000-0000000000a2"
    /// "about the quarterly estimate" is outside the offline grammar's closed content lexicon, so the offline fast path
    /// leaves the text to the provider and the job must reach the transport.
    private static let captureText = "Remind me 2099-01-15 11:00:00 to call the roofer about the quarterly estimate"

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("composition-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    // MARK: configuration

    func testConfigurationDecodesFromVersionedJSON() throws {
        let json = """
            {"version":1,"storage_root_path":"/tmp/x","capture_route_id":"route-a"}
            """
        let configuration = try CompositionConfiguration.decode(Data(json.utf8))
        XCTAssertEqual(configuration.storageRootPath, "/tmp/x")
        XCTAssertEqual(configuration.captureRouteID, "route-a")
        XCTAssertEqual(configuration.captureItemScope, "personal")
        XCTAssertEqual(configuration.credentialServiceName, CredentialService.defaultServiceName)
    }

    func testConfigurationOfAnotherVersionIsRefusedBeforeItsFieldsAreRead() {
        let json = #"{"version":2,"something_new":true}"#
        XCTAssertThrowsError(try CompositionConfiguration.decode(Data(json.utf8))) { error in
            XCTAssertEqual(error as? CompositionError, .unsupportedConfigurationVersion(2))
        }
    }

    func testInvalidConfigurationIsRefused() throws {
        var configuration = makeConfiguration()
        configuration.captureRouteID = ""
        XCTAssertThrowsError(try ServiceCompositionRoot(configuration: configuration)) { error in
            XCTAssertEqual(error as? CompositionError, .invalidConfiguration("capture_route_id"))
        }
        configuration = makeConfiguration()
        configuration.jobRetryDelaySeconds = 0
        XCTAssertThrowsError(try configuration.validate()) { error in
            XCTAssertEqual(error as? CompositionError, .invalidConfiguration("job_retry_delay_seconds"))
        }
    }

    // MARK: production construction

    func testProductionConstructionSelectsTheRealAdapters() throws {
        let root = try ServiceCompositionRoot(configuration: makeConfiguration())
        defer { root.shutDown() }
        XCTAssertEqual(root.adapterSet, .production)
        XCTAssertTrue(root.adapters.sender is ProviderHTTPTransport)
        XCTAssertTrue(root.adapters.notificationCenter is SystemNotificationCenter)
        XCTAssertTrue(root.adapters.reachability is SystemNetworkReachability)
        XCTAssertTrue(root.adapters.backgroundGrant is ApplicationBackgroundExecutionGrant)
        XCTAssertTrue(root.adapters.permissionObservations is UserDefaultsNotificationPermissionObservationStore)
        XCTAssertTrue(root.adapters.lifecycleNotifications === NotificationCenter.default)
        XCTAssertNil(root.adapters.recognizer, "production uses the on-device speech recognizer")
    }

    func testInjectedAdaptersAreMarkedAsTestOnly() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        XCTAssertEqual(root.adapterSet, .injectedForTests)
    }

    func testShutDownIsIdempotentAndReleasesTheStorage() throws {
        let first = try ServiceCompositionRoot(configuration: makeConfiguration())
        first.shutDown()
        first.shutDown()
        var outcome: CompositionSaveOutcome?
        first.saveText(captureID: UUID().uuidString.lowercased(), text: "late", capturedAt: Date()) { outcome = $0 }
        guard case .notStaged? = outcome else { return XCTFail("a stopped root stages nothing: \(String(describing: outcome))") }

        let second = try ServiceCompositionRoot(configuration: makeConfiguration())
        second.shutDown()
    }

    // MARK: extension seam

    func testModuleJoinsWithoutEditingTheRoot() throws {
        let module = RecordingModule(
            identifier: "optional.capability", firstOperationID: 1 << 58, jobTypes: ["optional_capability_job"])
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        XCTAssertNotNil(module.context, "attach receives the open core")
        try module.askCore()
        XCTAssertTrue(pump(until: { !module.receivedEvents.isEmpty }), "the module receives the events of its range")
        root.shutDown()
        XCTAssertTrue(module.isShutDown)
    }

    func testModuleOperationRangesThatCollideAreRefused() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let cases: [(String, [CompositionModule])] = [
            ("below the importer range", [RecordingModule(identifier: "a", firstOperationID: 1 << 10)]),
            ("the status reader range", [RecordingModule(identifier: "a", firstOperationID: 1 << 57)]),
            ("the job runner range", [RecordingModule(identifier: "a", firstOperationID: 1 << 62)]),
            ("the reminder range", [RecordingModule(identifier: "a", firstOperationID: 1 << 56)]),
            (
                "another module's range",
                [
                    RecordingModule(identifier: "a", firstOperationID: 1 << 58),
                    RecordingModule(identifier: "b", firstOperationID: 1 << 58),
                ]
            ),
        ]
        for (label, modules) in cases {
            XCTAssertThrowsError(try makeTestRoot(sender: sender, modules: modules), label) { error in
                guard case CompositionError.operationRangeConflict = error else {
                    return XCTFail("\(label): \(error)")
                }
            }
        }
    }

    func testDuplicateModuleIdentifiersAreRefused() throws {
        let modules: [CompositionModule] = [
            RecordingModule(identifier: "same", firstOperationID: 1 << 58),
            RecordingModule(identifier: "same", firstOperationID: 1 << 59),
        ]
        XCTAssertThrowsError(try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: modules)) {
            XCTAssertEqual($0 as? CompositionError, .duplicateModule("same"))
        }
    }

    func testJobTypeConflictFailsCleanlyAndReleasesEverything() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let module = RecordingModule(
            identifier: "clash", firstOperationID: 1 << 58, jobTypes: [TranscriptionJobHandler.transcriptionJobType])
        XCTAssertThrowsError(try makeTestRoot(sender: sender, modules: [module])) {
            XCTAssertEqual($0 as? CompositionError, .jobTypeConflict(TranscriptionJobHandler.transcriptionJobType))
        }
        let again = try makeTestRoot(sender: sender)
        again.shutDown()
    }

    // MARK: save, dispatch, read, schedule through the real core

    func testSaveDispatchReadAndScheduleThroughTheRealCore() throws {
        try prepareStore()
        let sender = ScriptedProviderSender(reply: try anthropicReply())
        let notifications = FakeNotificationCenter()
        let root = try makeTestRoot(sender: sender, notifications: notifications)

        let captureID = UUID().uuidString.lowercased()
        var saved: CompositionSaveOutcome?
        let capturedAt = Date()
        root.saveText(captureID: captureID, text: Self.captureText, capturedAt: capturedAt) { saved = $0 }
        XCTAssertTrue(pump(until: { saved != nil }), "the save completes")
        guard case .saved(let savedCaptureID, let itemID, let alreadySaved)? = saved else {
            return XCTFail("expected a confirmed save, got \(String(describing: saved))")
        }
        XCTAssertEqual(savedCaptureID, captureID)
        XCTAssertFalse(alreadySaved)

        let beforeProcessing = try XCTUnwrap(readStatus(root, itemID: itemID))
        XCTAssertEqual(beforeProcessing.saveState, "saved_local")
        XCTAssertEqual(beforeProcessing.processingState, "unprocessed")
        XCTAssertNil(beforeProcessing.reminderRequestState)

        try insertInterpretationJob(itemID: itemID)
        root.start(isActive: true)

        XCTAssertTrue(
            pump(until: { !sender.requests.isEmpty }, timeout: 20),
            "the job reaches the provider transport: \(jobDiagnostics(root, sender: sender))")
        XCTAssertEqual(sender.requests.first?.url.host, "api.anthropic.com")

        var processed: ItemStatusReport?
        XCTAssertTrue(
            pump(until: {
                processed = self.readStatus(root, itemID: itemID)
                return processed?.reminderRequestState != nil
            }, timeout: 30),
            "the provider reply is applied to the item: \(jobDiagnostics(root, sender: sender))")
        XCTAssertEqual(processed?.processingState, "processed")
        XCTAssertEqual(processed?.reminderRequestState, "resolved")

        var scheduled: ItemStatusReport?
        XCTAssertTrue(
            pump(until: {
                scheduled = self.readStatus(root, itemID: itemID)
                return scheduled?.reminderScheduleState == "scheduled"
            }, timeout: 30),
            "the reminder reaches the scheduler")
        let request = try XCTUnwrap(notifications.addedRequests.first, "the notification center received the request")
        XCTAssertEqual(request.dueInstant.timeIntervalSince1970, 4_072_158_000, accuracy: 1)
        XCTAssertFalse(
            String(describing: request.content).contains("roofer"),
            "the scheduled notification carries no capture text")

        root.shutDown()

        let reopened = try makeTestRoot(sender: sender, notifications: notifications)
        defer { reopened.shutDown() }
        let durable = try XCTUnwrap(readStatus(reopened, itemID: itemID))
        XCTAssertEqual(durable.processingState, "processed")
        XCTAssertEqual(durable.reminderScheduleState, "scheduled")

        var repeated: CompositionSaveOutcome?
        reopened.saveText(captureID: captureID, text: Self.captureText, capturedAt: capturedAt) { repeated = $0 }
        XCTAssertTrue(pump(until: { repeated != nil }))
        if case .saved(_, let repeatedItemID, let again)? = repeated {
            XCTAssertEqual(repeatedItemID, itemID)
            XCTAssertTrue(again, "saving the same capture again converges on the same item")
        } else {
            XCTFail("expected the repeated save to converge, got \(String(describing: repeated))")
        }
    }

    // MARK: helpers

    private func makeConfiguration() -> CompositionConfiguration {
        CompositionConfiguration(
            storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600)
    }

    private func makeTestRoot(
        sender: ProviderRequestSender,
        notifications: NotificationCenterProviding = FakeNotificationCenter(),
        modules: [CompositionModule] = []
    ) throws -> ServiceCompositionRoot {
        try ServiceCompositionRoot(
            testing: makeConfiguration(),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, notificationCenter: notifications,
                reachability: FakeReachability(reachable: true)),
            modules: modules)
    }

    private func pump(until condition: () -> Bool, timeout: TimeInterval = 15) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    private func readStatus(_ root: ServiceCompositionRoot, itemID: String) -> ItemStatusReport? {
        var result: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: itemID) { result = $0 }
        _ = pump(until: { result != nil })
        return try? result?.get()
    }

    private func jobDiagnostics(_ root: ServiceCompositionRoot, sender: ScriptedProviderSender) -> String {
        let job = JobStoreInspector.job(at: databasePath, id: "job-composition-1")
        return "requests=\(sender.requests.count) drains started=\(root.jobRunner.startedDrainCount) "
            + "finished=\(root.jobRunner.finishedDrainCount) outcome=\(String(describing: root.jobRunner.lastOutcome)) "
            + "job=\(String(describing: job))"
    }

    private var databasePath: String {
        ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
    }

    /// Creates the stores and the schema with the real core, then adds the rows an installed provider profile and a
    /// route would have: nothing in the app creates them yet.
    private func prepareStore() throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        _ = ProtectedStorageService(layout: layout).prepare()
        try CoreHandle(path: layout.databaseURL.path).close()
        let destination = #"["https://api.anthropic.com"]"#
        let retryPolicy = #"{"max_attempts":3,"initial_backoff_ms":500,"max_backoff_ms":30000}"#
        let capabilities =
            #"{"text_interpretation":{"capability":"text_interpretation","support":"supported","# +
            #""evidence":"composition-root-tests","reason":null,"input_size_limit":500,"# +
            #""structured_output":"json_schema"}}"#
        try execute([
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            ('\(Self.routeID)', 'synthetic', 'personal', '\(destination)', '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) VALUES \
            ('\(Self.routeID)', 'text_interpretation', '\(destination)', '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO provider_profiles (profile_version, profile_id, provider_type, endpoint, model, \
            credential_ref, timeout_seconds, retry_policy, authorized_destinations, capabilities, created_at, \
            revoked_at) VALUES ('\(Self.profileVersion)', 'synthetic-profile', 'anthropic', NULL, 'synthetic-model', \
            'synthetic-credential', 30, '\(retryPolicy)', '\(destination)', '\(capabilities)', \
            '2026-10-08T09:00:00Z', NULL)
            """,
        ])
    }

    private func insertInterpretationJob(itemID: String) throws {
        try execute([
            """
            INSERT INTO jobs (job_id, job_schema_version, item_id, job_type, source_revision, profile_version, \
            request_version, status, attempt_count, created_at) VALUES ('job-composition-1', 1, '\(itemID)', \
            'interpret', 0, '\(Self.profileVersion)', '\(Self.requestVersion)', 'queued', 0, '2026-10-08T09:30:02Z')
            """
        ])
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
            "id": "msg_composition_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_composition_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }
}
