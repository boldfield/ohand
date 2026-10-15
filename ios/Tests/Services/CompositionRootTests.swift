import Foundation
import SQLite3
import UIKit
import UserNotifications
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
    /// The refusals the read gate gave when the module asked to admit an event, in arrival order.
    private(set) var admissionRefusals: [CoreFailure] = []
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
        do {
            try context?.privateReads.admit(operationID: event.operationID)
        } catch {
            admissionRefusals.append((error as? CoreFailure) ?? CoreFailure.handleClosed)
        }
        receivedEvents.append(event)
        return true
    }

    func shutDown() {
        isShutDown = true
    }

    func askCore() throws {
        try context?.privateReads.startItemStatus(operationID: firstOperationID, itemID: "no-such-item")
    }
}

/// Owns the processing health client, which claims every identifier of its interval, and is registered before others.
private final class HealthOwningModule: CompositionModule {
    let identifier = "health.owner"
    let firstOperationID: UInt64 = 1 << 42
    private(set) var health: ProcessingHealthService?

    func attach(to context: CompositionContext) throws {
        health = try context.clients.processingHealth()
    }

    func handle(_ event: CoreEvent) -> Bool {
        health?.handle(event) ?? false
    }

    func shutDown() {}
}

/// Takes the fixed-range core clients it names when it attaches.
private final class ClientTakingModule: CompositionModule {
    let identifier: String
    let firstOperationID: UInt64
    private let take: (CompositionCoreClients) throws -> Void
    private(set) var isShutDown = false

    init(identifier: String, firstOperationID: UInt64, take: @escaping (CompositionCoreClients) throws -> Void) {
        self.identifier = identifier
        self.firstOperationID = firstOperationID
        self.take = take
    }

    func attach(to context: CompositionContext) throws {
        try take(context.clients)
    }

    func handle(_ event: CoreEvent) -> Bool { false }

    func shutDown() {
        isShutDown = true
    }
}

/// Takes shadow review through the typed client factory, the only way a module reaches the provider transport.
private final class ShadowReviewTakingModule: CompositionModule {
    let identifier = "shadow.review.taker"
    let firstOperationID: UInt64 = 1 << 44
    let configuration: ShadowReviewConfiguration
    private(set) var service: ShadowReviewService?

    init(configuration: ShadowReviewConfiguration) {
        self.configuration = configuration
    }

    func attach(to context: CompositionContext) throws {
        service = try context.clients.shadowReview(configuration: configuration)
    }

    func handle(_ event: CoreEvent) -> Bool {
        service?.handle(event) ?? false
    }

    func shutDown() {}
}

/// Retrieves through the gated search and list verbs only, as an `app/` module would: it holds no core handle.
private final class RetrievalModule: CompositionModule {
    let identifier = "retrieval.reader"
    let firstOperationID: UInt64 = 1 << 44
    private(set) var context: CompositionContext?
    private var nextOffset: UInt64 = 0
    /// Every admitted answer, decoded, and every refusal, in arrival order.
    private(set) var outcomes: [Result<PrivateRetrievalAnswer, CoreFailure>] = []

    func attach(to context: CompositionContext) throws {
        self.context = context
    }

    func handle(_ event: CoreEvent) -> Bool {
        guard event.operationID >= firstOperationID, event.operationID < firstOperationID + nextOffset else {
            return false
        }
        do {
            try context?.privateReads.admit(operationID: event.operationID)
            outcomes.append(.success(try event.decode(PrivateRetrievalAnswer.self)))
        } catch {
            outcomes.append(.failure((error as? CoreFailure) ?? CoreFailure.handleClosed))
        }
        return true
    }

    func shutDown() {}

    func search(_ query: String, filters: PrivateRetrievalFilters = PrivateRetrievalFilters()) throws {
        try context?.privateReads.startSearch(operationID: allocateOperationID(), query: query, filters: filters)
    }

    func list(filters: PrivateRetrievalFilters = PrivateRetrievalFilters()) throws {
        try context?.privateReads.startList(operationID: allocateOperationID(), filters: filters)
    }

    private func allocateOperationID() -> UInt64 {
        defer { nextOffset += 1 }
        return firstOperationID + nextOffset
    }
}

private struct TransportAttachRefused: Error {}

final class CompositionRootTests: XCTestCase {
    private static let routeID = "route-composition"
    /// The core requires a proposal's zone to equal the capture's, so the reply reads the identifier the root stamps.
    private static let captureTimeZone = TimeZone(identifier: "UTC")!
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
        XCTAssertEqual(root.readSession.scope, .captureOnly, "production reads start locked")
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
            identifier: "optional.capability", firstOperationID: 1 << 44, jobTypes: ["optional_capability_job"])
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        XCTAssertNotNil(module.context, "attach receives the open core")
        try module.askCore()
        XCTAssertTrue(pump(until: { !module.receivedEvents.isEmpty }), "the module receives the events of its range")
        root.shutDown()
        XCTAssertTrue(module.isShutDown)
    }

    func testModuleContextCarriesOnlyGatedAndCoreMediatedCapabilities() throws {
        let module = RecordingModule(identifier: "boundary.probe", firstOperationID: 1 << 44)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        defer { root.shutDown() }
        let context = try XCTUnwrap(module.context)
        let fields = Mirror(reflecting: context).children.map { child -> (String, Any) in (child.label ?? "", child.value) }
        XCTAssertEqual(
            fields.map(\.0), ["clients", "privateReads", "now"],
            "a new context field must not widen the seam past gated reads and core-mediated clients")
        for (label, value) in fields {
            XCTAssertFalse(value is CoreHandle, "\(label) hands a module the ungated core handle")
            XCTAssertFalse(value is ProviderRequestSender, "\(label) hands a module the raw provider transport")
            XCTAssertFalse(value is URL, "\(label) hands a module a path that can reach the core's stores")
        }
    }

    func testModuleReadIsRefusedWhileTheSessionIsLocked() throws {
        let (session, _) = makeSession(authenticated: false)
        let module = RecordingModule(identifier: "locked.reader", firstOperationID: 1 << 44)
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, modules: [module])
        defer { root.shutDown() }
        XCTAssertThrowsError(try module.askCore()) { error in
            let failure = error as? CoreFailure
            XCTAssertEqual(failure?.errorClass, .unauthorized)
            XCTAssertEqual(failure?.code, "read_not_authenticated")
        }
        XCTAssertTrue(module.receivedEvents.isEmpty, "a refused read never reaches the core")
    }

    func testModuleAnswerArrivingAfterARelockIsRefusedAdmission() throws {
        let (session, _) = makeSession(authenticated: true)
        let module = RecordingModule(identifier: "relocked.reader", firstOperationID: 1 << 44)
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, modules: [module])
        defer { root.shutDown() }
        try module.askCore()
        root.relockReads()
        XCTAssertTrue(pump(until: { !module.receivedEvents.isEmpty }))
        XCTAssertEqual(module.admissionRefusals.first?.code, "read_relocked")
    }

    func testModuleAnswerIsAdmittedWhileTheSessionStaysAuthenticated() throws {
        let module = RecordingModule(identifier: "authenticated.reader", firstOperationID: 1 << 44)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        defer { root.shutDown() }
        try module.askCore()
        XCTAssertTrue(pump(until: { !module.receivedEvents.isEmpty }))
        XCTAssertTrue(module.admissionRefusals.isEmpty)
    }

    func testModuleSearchAndListAreRefusedBeforeAuthenticationAndNeverReachTheCore() throws {
        let (session, _) = makeSession(authenticated: false)
        let module = RetrievalModule()
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, modules: [module])
        defer { root.shutDown() }
        _ = try saveSyntheticCapture(root)

        for ask in [{ try module.search("synthetic") }, { try module.list() }] {
            XCTAssertThrowsError(try ask()) { error in
                let failure = error as? CoreFailure
                XCTAssertEqual(failure?.errorClass, .unauthorized)
                XCTAssertEqual(failure?.code, "read_not_authenticated")
            }
        }
        _ = pump(until: { !module.outcomes.isEmpty }, timeout: 0.3)
        XCTAssertTrue(module.outcomes.isEmpty, "a refused retrieval never reaches the core")
    }

    func testModuleSearchAndListReturnASeededCaptureAfterAuthentication() throws {
        let (session, boundary) = makeSession(authenticated: false)
        let module = RetrievalModule()
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, modules: [module])
        defer { root.shutDown() }
        let itemID = try saveSyntheticCapture(root)
        XCTAssertThrowsError(try module.search("synthetic"))

        boundary.nextOutcome = .succeeded
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        try module.search("synthetic")
        try module.list()
        XCTAssertTrue(pump(until: { module.outcomes.count == 2 }))
        for outcome in module.outcomes {
            guard case .success(let answer) = outcome else { return XCTFail("authenticated retrieval failed: \(outcome)") }
            XCTAssertEqual(answer.totalAccessible, 1)
            XCTAssertEqual(answer.hits.map(\.itemID), [itemID])
            XCTAssertEqual(answer.hits.first?.text, "synthetic capture")
            XCTAssertEqual(answer.hits.first?.scope, "personal")
        }
    }

    func testModuleRetrievalAnswerArrivingAfterARelockIsDiscarded() throws {
        let (session, _) = makeSession(authenticated: true)
        let module = RetrievalModule()
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, modules: [module])
        defer { root.shutDown() }
        _ = try saveSyntheticCapture(root)

        try module.search("synthetic")
        session.relock(.explicit)
        XCTAssertTrue(pump(until: { !module.outcomes.isEmpty }))
        guard case .failure(let failure)? = module.outcomes.first else {
            return XCTFail("a relocked answer must not be delivered: \(module.outcomes)")
        }
        XCTAssertEqual(failure.errorClass, .unauthorized)
        XCTAssertEqual(failure.code, "read_relocked")
    }

    func testModuleRetrievalExcludesScopesItDoesNotAskFor() throws {
        let module = RetrievalModule()
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        defer { root.shutDown() }
        _ = try saveSyntheticCapture(root)

        let workOnly = PrivateRetrievalFilters(scopes: [.work])
        try module.search("synthetic", filters: workOnly)
        try module.list(filters: workOnly)
        XCTAssertTrue(pump(until: { module.outcomes.count == 2 }))
        for outcome in module.outcomes {
            guard case .success(let answer) = outcome else { return XCTFail("retrieval failed: \(outcome)") }
            XCTAssertEqual(answer.totalAccessible, 0, "no count from an excluded scope")
            XCTAssertTrue(answer.hits.isEmpty, "no text from an excluded scope")
        }

        try module.search("synthetic", filters: PrivateRetrievalFilters(scopes: [.personal]))
        XCTAssertTrue(pump(until: { module.outcomes.count == 3 }))
        guard case .success(let personal)? = module.outcomes.last else { return XCTFail("retrieval failed") }
        XCTAssertEqual(personal.hits.map(\.text), ["synthetic capture"])
    }

    func testModuleJustBelowTheProcessingHealthIntervalReceivesItsEvents() throws {
        let module = RecordingModule(
            identifier: "highest.accepted",
            firstOperationID: ProcessingHealthService.firstOperationID - ServiceCompositionRoot.operationRangeWidth)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        defer { root.shutDown() }
        try module.askCore()
        XCTAssertTrue(
            pump(until: { !module.receivedEvents.isEmpty }),
            "an accepted module at the top of the allowed range is not shadowed by the job runner")
    }

    func testModuleAboveAProcessingHealthOwnerReceivesItsResponse() throws {
        let healthOwner = HealthOwningModule()
        let upper = RecordingModule(
            identifier: "upper.module",
            firstOperationID: ProcessingHealthService.firstOperationID - ServiceCompositionRoot.operationRangeWidth)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [healthOwner, upper])
        defer { root.shutDown() }
        let health = try XCTUnwrap(healthOwner.health)
        health.refresh()
        try upper.askCore()
        XCTAssertTrue(
            pump(until: { !upper.receivedEvents.isEmpty && health.latestOutcome != nil }),
            "the health client, routed first, answers its own read without swallowing the later module's answer")
        XCTAssertTrue(upper.admissionRefusals.isEmpty)
    }

    func testEveryFixedRangeCoreClientCanBeTakenOnce() throws {
        let module = ClientTakingModule(identifier: "all.clients", firstOperationID: 1 << 44) { clients in
            _ = try clients.audioRetention()
            _ = try clients.deletionCleanup()
            _ = try clients.processingHealth()
            _ = try clients.shadowReview()
        }
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: [module])
        root.shutDown()
        XCTAssertTrue(module.isShutDown)
    }

    func testAFixedRangeCoreClientTakenTwiceIsRefusedAndReleasesEverything() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let takers: [(String, (CompositionCoreClients) throws -> Void)] = [
            (ServiceCompositionRoot.audioRetentionClientName, { _ = try $0.audioRetention() }),
            (ServiceCompositionRoot.deletionCleanupClientName, { _ = try $0.deletionCleanup() }),
            (ServiceCompositionRoot.processingHealthClientName, { _ = try $0.processingHealth() }),
            (ServiceCompositionRoot.shadowReviewClientName, { _ = try $0.shadowReview() }),
        ]
        for (name, take) in takers {
            let first = ClientTakingModule(identifier: "first", firstOperationID: 1 << 44, take: take)
            let second = ClientTakingModule(identifier: "second", firstOperationID: 1 << 45, take: take)
            XCTAssertThrowsError(try makeTestRoot(sender: sender, modules: [first, second]), name) {
                XCTAssertEqual($0 as? CompositionError, .coreClientAlreadyTaken(name))
            }
            XCTAssertTrue(first.isShutDown, "\(name): the module that attached is shut down when assembly fails")
        }
        let again = try makeTestRoot(sender: sender)
        again.shutDown()
    }

    func testModuleOperationRangesThatCollideAreRefused() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let cases: [(String, [CompositionModule])] = [
            ("below the importer range", [RecordingModule(identifier: "a", firstOperationID: 1 << 10)]),
            ("the status reader range", [RecordingModule(identifier: "a", firstOperationID: 1 << 57)]),
            ("the job runner range", [RecordingModule(identifier: "a", firstOperationID: 1 << 62)]),
            ("above the job runner start", [RecordingModule(identifier: "a", firstOperationID: 1 << 63)]),
            ("the highest identifier", [RecordingModule(identifier: "a", firstOperationID: UInt64.max)]),
            ("the reminder range", [RecordingModule(identifier: "a", firstOperationID: 1 << 56)]),
            ("the audio retention client range", [RecordingModule(identifier: "a", firstOperationID: 1 << 58)]),
            ("the deletion cleanup client range", [RecordingModule(identifier: "a", firstOperationID: 1 << 52)]),
            ("the shadow review client range", [RecordingModule(identifier: "a", firstOperationID: 1 << 48)]),
            (
                "ending inside the shadow review client range",
                [RecordingModule(identifier: "a", firstOperationID: (1 << 48) - (1 << 39))]
            ),
            ("the notification action range", [RecordingModule(identifier: "a", firstOperationID: 1 << 60)]),
            (
                "inside the processing health interval",
                [
                    RecordingModule(
                        identifier: "a",
                        firstOperationID: ProcessingHealthService.firstOperationID
                            + ServiceCompositionRoot.operationRangeWidth)
                ]
            ),
            (
                "straddling the job runner start",
                [
                    RecordingModule(
                        identifier: "a",
                        firstOperationID: JobRunnerService.firstOperationID - ServiceCompositionRoot.operationRangeWidth)
                ]
            ),
            (
                "overlapping the end of another module's range",
                [
                    RecordingModule(identifier: "a", firstOperationID: 1 << 44),
                    RecordingModule(identifier: "b", firstOperationID: (1 << 44) + (1 << 39)),
                ]
            ),
            (
                "another module's range",
                [
                    RecordingModule(identifier: "a", firstOperationID: 1 << 44),
                    RecordingModule(identifier: "b", firstOperationID: 1 << 44),
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
            RecordingModule(identifier: "same", firstOperationID: 1 << 44),
            RecordingModule(identifier: "same", firstOperationID: 1 << 45),
        ]
        XCTAssertThrowsError(try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), modules: modules)) {
            XCTAssertEqual($0 as? CompositionError, .duplicateModule("same"))
        }
    }

    func testJobTypeConflictFailsCleanlyAndReleasesEverything() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let module = RecordingModule(
            identifier: "clash", firstOperationID: 1 << 44, jobTypes: [TranscriptionJobHandler.transcriptionJobType])
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

    // MARK: provider transport

    func testModuleShadowReviewDispatchesThroughTheTransportTheRootAttached() throws {
        try prepareShadowReviewStore()
        let sender = ScriptedProviderSender(reply: try shadowReviewReply())
        let module = ShadowReviewTakingModule(configuration: shadowReviewConfiguration())
        let root = try makeTestRoot(sender: sender, modules: [module])
        defer { root.shutDown() }
        let service = try XCTUnwrap(module.service)

        let jobID = try selectCase(service)
        var run: ShadowReviewRunResult?
        service.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }), "the review completes")

        guard case .completed(let record, nil)? = run else {
            return XCTFail("expected a completed review, got \(String(describing: run))")
        }
        XCTAssertEqual(record.outcome, .reviewed)
        XCTAssertEqual(record.verdict, .agreement)
        XCTAssertEqual(sender.requests.count, 1, "the one review request reached the sender the root was given")
        XCTAssertEqual(sender.requests.first?.authorization.capability, .review)
        XCTAssertEqual(sender.requests.first?.url.host, "api.anthropic.com")
    }

    func testRootAttachesTheTransportOnceToItsOwnCoreWithTheAdapterSender() throws {
        try prepareShadowReviewStore()
        let sender = ScriptedProviderSender(reply: try shadowReviewReply())
        var attachments: [(core: CoreHandle, sender: ProviderRequestSender)] = []
        let root = try makeTestRoot(
            sender: sender,
            attachProviderTransport: { core, sender in
                attachments.append((core, sender))
                return try ProviderExchangeCoordinator.attach(to: core, sender: sender)
            })
        defer { root.shutDown() }
        XCTAssertEqual(attachments.count, 1)
        XCTAssertTrue(attachments.first?.core === root.coreHandle, "attached to the root's own core handle")
        XCTAssertTrue(attachments.first?.sender === sender, "attached with the sender the adapters carry")
    }

    func testFailedTransportAttachUndoesAssemblyAndTheCoreRefusesDispatchWithoutATransport() throws {
        try prepareShadowReviewStore()
        let sender = ScriptedProviderSender(reply: try shadowReviewReply())
        let module = ShadowReviewTakingModule(configuration: shadowReviewConfiguration())
        var attempts = 0
        XCTAssertThrowsError(
            try makeTestRoot(
                sender: sender,
                attachProviderTransport: { _, _ in
                    attempts += 1
                    throw TransportAttachRefused()
                },
                modules: [module])
        ) { XCTAssertTrue($0 is TransportAttachRefused) }
        XCTAssertEqual(attempts, 1)
        XCTAssertNil(module.service, "no module attaches over a core without a transport")

        let bareCore = try CoreHandle(path: databasePath)
        defer { bareCore.close() }
        let bareService = ShadowReviewService(core: bareCore, configuration: shadowReviewConfiguration())
        try bareCore.setEventHandler { _ = bareService.handle($0) }
        let jobID = try selectCase(bareService)
        var run: ShadowReviewRunResult?
        bareService.review(jobID: jobID) { run = $0 }
        XCTAssertTrue(pump(until: { run != nil }))
        XCTAssertEqual(run, .failed(code: "transport_not_registered"))
        XCTAssertTrue(sender.requests.isEmpty, "a dispatch without a transport sends nothing")

        let reassembled = try makeTestRoot(sender: sender)
        reassembled.shutDown()
    }

    // MARK: private read gate

    func testStatusReadIsRefusedWhileTheSessionIsLockedAndNeverReachesTheCore() throws {
        let (session, _) = makeSession(authenticated: false)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), readSession: session)
        defer { root.shutDown() }

        var result: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: "any-item") { result = $0 }
        guard case .failure(let failure)? = result else {
            return XCTFail("a locked read fails at once: \(String(describing: result))")
        }
        XCTAssertEqual(failure.errorClass, .unauthorized)
        XCTAssertEqual(failure.code, "read_not_authenticated")
    }

    func testStatusAnswerArrivingAfterARelockIsDroppedUndelivered() throws {
        try prepareStore()
        let (session, _) = makeSession(authenticated: true)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), readSession: session)
        defer { root.shutDown() }

        var result: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: "any-item") { result = $0 }
        session.relock(.explicit)
        XCTAssertTrue(pump(until: { result != nil }), "the read still completes")
        guard case .failure(let failure)? = result else {
            return XCTFail("a relocked answer is not delivered: \(String(describing: result))")
        }
        XCTAssertEqual(failure.errorClass, .unauthorized)
        XCTAssertEqual(failure.code, "read_relocked")
    }

    func testStatusReadSucceedsOnlyAfterTheSessionAuthenticates() throws {
        let (session, boundary) = makeSession(authenticated: false)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), readSession: session)
        defer { root.shutDown() }

        var refused: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: "any-item") { refused = $0 }
        guard case .failure(let refusal)? = refused else {
            return XCTFail("a locked read fails at once: \(String(describing: refused))")
        }
        XCTAssertEqual(refusal.code, "read_not_authenticated")

        boundary.nextOutcome = .succeeded
        XCTAssertEqual(authenticateAndWait(session), .authenticated)
        var admitted: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: "any-item") { admitted = $0 }
        XCTAssertTrue(pump(until: { admitted != nil }))
        if case .failure(let failure)? = admitted {
            XCTAssertNotEqual(failure.errorClass, .unauthorized, "an authenticated read reaches the core")
        }
    }

    // MARK: public read authentication lifecycle

    func testReadAuthenticationLifecycleThroughThePublicRootSurface() throws {
        let (session, boundary) = makeSession(authenticated: false)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), readSession: session)
        defer { root.shutDown() }
        XCTAssertEqual(root.readScope, .captureOnly)

        boundary.nextOutcome = .denied
        XCTAssertEqual(authenticateReadsAndWait(root), .denied)
        XCTAssertEqual(root.readScope, .captureOnly)

        boundary.nextOutcome = .succeeded
        XCTAssertEqual(authenticateReadsAndWait(root), .authenticated)
        XCTAssertEqual(root.readScope, .authenticated)

        root.relockReads()
        XCTAssertEqual(root.readScope, .captureOnly)
        var result: Result<ItemStatusReport, CoreFailure>?
        root.itemStatus(itemID: "any-item") { result = $0 }
        guard case .failure(let failure)? = result else {
            return XCTFail("a relocked read fails at once: \(String(describing: result))")
        }
        XCTAssertEqual(failure.code, "read_not_authenticated")
    }

    func testBackgroundingTheAppRelocksReads() throws {
        let lifecycle = NotificationCenter()
        let (session, _) = makeSession(authenticated: true)
        let root = try makeTestRoot(
            sender: ScriptedProviderSender(reply: Data()), readSession: session, lifecycleNotifications: lifecycle)
        defer { root.shutDown() }
        root.start(isActive: false)
        XCTAssertEqual(root.readScope, .authenticated)
        lifecycle.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        XCTAssertEqual(root.readScope, .captureOnly)
        XCTAssertEqual(session.lastRelockReason, .enteredBackground)
    }

    // MARK: capture route and notification actions

    private static let reminderID = "4a2b8c1e-5d4a-4e0b-9a77-000000000001"

    func testFreshStoreSavesThroughAProductionConfiguredRootOnAProvisionedLocalOnlyRoute() throws {
        let root = try ServiceCompositionRoot(configuration: makeConfiguration())
        defer { root.shutDown() }
        let itemID = try saveSyntheticCapture(root)
        XCTAssertEqual(scalar("SELECT item_id FROM items"), itemID, "the item exists")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "1")
        XCTAssertEqual(scalar("SELECT processing_destinations FROM routes WHERE route_id = '\(Self.routeID)'"), "[]")
        XCTAssertEqual(scalar("SELECT scope FROM routes WHERE route_id = '\(Self.routeID)'"), "personal")
        XCTAssertEqual(
            scalar("SELECT CAST(COUNT(*) AS TEXT) FROM route_authorizations"), "0",
            "provisioning the route authorizes no upload")
    }

    // MARK: voice ingress seam

    private static let recordingCaptureID = "voice-root-1"
    private static let recordingBytes = Data("synthetic recording".utf8)

    func testRecordingHandedOffIsSavedAsAnAudioCaptureWithItsItemAndNothingIsUploaded() throws {
        let sender = ScriptedProviderSender(reply: Data())
        let root = try makeTestRoot(sender: sender)
        defer { root.shutDown() }
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        XCTAssertEqual(
            root.inProgressAudioDirectory.standardizedFileURL, layout.directory(for: .ingressInProgressAudio).standardizedFileURL)
        let recordingURL = root.inProgressAudioDirectory.appendingPathComponent("\(Self.recordingCaptureID).wav")
        try Self.recordingBytes.write(to: recordingURL)

        var outcome: CompositionRecordingOutcome?
        root.saveRecording(makeHandoff()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }), "the handoff completes")
        guard case .saved(let captureID, let itemID, let savedAt, let alreadySaved)? = outcome else {
            return XCTFail("expected a confirmed save, got \(String(describing: outcome))")
        }
        XCTAssertEqual(captureID, Self.recordingCaptureID)
        XCTAssertFalse(savedAt.isEmpty)
        XCTAssertFalse(alreadySaved)

        XCTAssertEqual(scalar("SELECT item_id FROM items WHERE capture_id = '\(Self.recordingCaptureID)'"), itemID)
        XCTAssertEqual(
            scalar("SELECT audio_reference FROM captures WHERE capture_id = '\(Self.recordingCaptureID)'"),
            "\(StoreProtectionPolicy.policy(for: .finalizedAudio).directoryName)/\(Self.recordingCaptureID).wav")
        XCTAssertEqual(
            scalar("SELECT COALESCE(text, 'none') FROM captures WHERE capture_id = '\(Self.recordingCaptureID)'"), "none")
        XCTAssertFalse(FileManager.default.fileExists(atPath: recordingURL.path), "the audio was moved, not copied")
        XCTAssertEqual(
            try Data(contentsOf: layout.directory(for: .finalizedAudio).appendingPathComponent("\(Self.recordingCaptureID).wav")),
            Self.recordingBytes)
        XCTAssertTrue(sender.requests.isEmpty, "nothing was sent to a provider")
        XCTAssertEqual(scalar("SELECT processing_destinations FROM routes WHERE route_id = '\(Self.routeID)'"), "[]")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM route_authorizations"), "0")

        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "1")
    }

    func testHandoffOfAMissingRecordingIsReportedUnavailableAndNeverSaved() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        var outcome: CompositionRecordingOutcome?
        root.saveRecording(makeHandoff()) { outcome = $0 }
        XCTAssertTrue(pump(until: { outcome != nil }))
        XCTAssertEqual(outcome, .audioUnavailable(captureID: Self.recordingCaptureID))
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "0")
    }

    func testHandoffAfterShutDownIsNotStaged() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        root.shutDown()
        var outcome: CompositionRecordingOutcome?
        root.saveRecording(makeHandoff()) { outcome = $0 }
        XCTAssertEqual(outcome, .notStaged(captureID: Self.recordingCaptureID))
        var completed = false
        var report: CompositionRecoveryReport?
        root.recoverRecordings {
            report = $0
            completed = true
        }
        XCTAssertTrue(completed)
        XCTAssertNil(report, "recovery on a shut-down root reports nil")
    }

    func testHandoffInterruptedBeforeImportIsRecoveredOnTheNextAssembly() throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        XCTAssertTrue(ProtectedStorageService(layout: layout).prepare().isFullyConfigured)
        let inProgressURL = layout.directory(for: .ingressInProgressAudio)
            .appendingPathComponent("\(Self.recordingCaptureID).wav")
        try Self.recordingBytes.write(to: inProgressURL)
        let record = IngressRecord(
            captureID: Self.recordingCaptureID, text: nil,
            audio: IngressAudioHandoff(
                inProgressFileName: "\(Self.recordingCaptureID).wav",
                finalizedFileName: "\(Self.recordingCaptureID).wav"),
            context: IngressCaptureContext(
                captureInstant: "2026-10-08T09:30:00Z", timezoneID: "UTC", utcOffsetMinutes: 0, locale: "en_US",
                calendar: "gregorian", itemScope: makeConfiguration().captureItemScope, routeID: Self.routeID,
                entryLocked: false, createdAt: "2026-10-08T09:30:00Z", sessionTopic: nil))
        _ = try IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem()).stage(record)

        let sender = ScriptedProviderSender(reply: Data())
        let root = try makeTestRoot(sender: sender)
        defer { root.shutDown() }
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "0", "nothing imported before recovery")

        var completed = false
        var report: CompositionRecoveryReport?
        root.recoverRecordings {
            report = $0
            completed = true
        }
        XCTAssertTrue(pump(until: { completed }), "recovery completes")
        let found = try XCTUnwrap(report)
        XCTAssertEqual(found.confirmedCaptureIDs, [Self.recordingCaptureID])
        XCTAssertTrue(found.pending.isEmpty)
        XCTAssertTrue(found.deletedItemCaptureIDs.isEmpty)
        XCTAssertEqual(found.unclaimedInProgressFileNames, [])
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items WHERE capture_id = '\(Self.recordingCaptureID)'"), "1")
        XCTAssertFalse(FileManager.default.fileExists(atPath: inProgressURL.path))
        XCTAssertTrue(sender.requests.isEmpty, "nothing was sent to a provider")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM route_authorizations"), "0")
    }

    func testUnclaimedInProgressFileIsReportedNotImported() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        let orphanURL = root.inProgressAudioDirectory.appendingPathComponent("orphan-recording.wav")
        try Self.recordingBytes.write(to: orphanURL)

        var completed = false
        var report: CompositionRecoveryReport?
        root.recoverRecordings {
            report = $0
            completed = true
        }
        XCTAssertTrue(pump(until: { completed }))
        let found = try XCTUnwrap(report)
        XCTAssertEqual(found.unclaimedInProgressFileNames, ["orphan-recording.wav"])
        XCTAssertTrue(found.confirmedCaptureIDs.isEmpty)
        XCTAssertTrue(found.pending.isEmpty)
        XCTAssertEqual(try Data(contentsOf: orphanURL), Self.recordingBytes, "the file is left in place")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "0", "nothing was imported")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM captures"), "0")
    }

    func testUnclaimedNamesAreNilWhenAStagedRecordCannotBeRead() throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        XCTAssertTrue(ProtectedStorageService(layout: layout).prepare().isFullyConfigured)
        try Data("not json".utf8).write(
            to: layout.directory(for: .ingressStagingRecords).appendingPathComponent("voice-corrupt-1.json"))
        try Self.recordingBytes.write(
            to: layout.directory(for: .ingressInProgressAudio).appendingPathComponent("maybe-owned.wav"))

        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        var completed = false
        var report: CompositionRecoveryReport?
        root.recoverRecordings {
            report = $0
            completed = true
        }
        XCTAssertTrue(pump(until: { completed }))
        let found = try XCTUnwrap(report)
        XCTAssertNil(found.unclaimedInProgressFileNames, "unknown ownership is nil, never an empty list")
        XCTAssertEqual(found.pending.map(\.captureID), ["voice-corrupt-1"])
        XCTAssertEqual(found.pending.first?.reason, .stagingRecordUnreadable)
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM items"), "0")
    }

    private func makeHandoff() -> CompositionRecordingHandoff {
        CompositionRecordingHandoff(
            captureID: Self.recordingCaptureID, inProgressFileName: "\(Self.recordingCaptureID).wav",
            finalizedFileName: "\(Self.recordingCaptureID).wav", startedAt: Date(timeIntervalSince1970: 1_790_000_000))
    }

    func testRouteScopeMismatchFailsAssemblyWithADistinctErrorAndReleasesEverything() throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        _ = ProtectedStorageService(layout: layout).prepare()
        try CoreHandle(path: layout.databaseURL.path).close()
        try execute([
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            ('\(Self.routeID)', 'synthetic', 'work', '[]', '2026-10-08T09:00:00Z')
            """
        ])
        XCTAssertThrowsError(try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))) { error in
            XCTAssertEqual(error as? CompositionError, .captureRouteUnavailable("route_scope_mismatch"))
        }
        XCTAssertEqual(scalar("SELECT scope FROM routes WHERE route_id = '\(Self.routeID)'"), "work", "nothing changed")
    }

    func testProvisioningKeepsARouteTheUserAlreadyConfigured() throws {
        try prepareStore()
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        XCTAssertEqual(
            scalar("SELECT processing_destinations FROM routes WHERE route_id = '\(Self.routeID)'"),
            #"["https://api.anthropic.com"]"#)
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM route_authorizations"), "1")
    }

    func testCompleteResponseReachesTheCoreOnceAndADuplicateDoesNotMutateAgain() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        let itemID = try saveSyntheticCapture(root)
        try seedReminder(itemID: itemID)
        let baselineRevision = try XCTUnwrap(Int(itemRevision(itemID)))
        XCTAssertEqual(itemLifecycle(itemID), "active")

        deliver(NotificationActionIdentifier.complete, to: root, itemID: itemID)
        reconcileActions(root)
        XCTAssertEqual(itemLifecycle(itemID), "completed")
        XCTAssertEqual(itemRevision(itemID), String(baselineRevision + 1), "one revisioned completion event")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM events WHERE event_type = 'completion'"), "1")

        deliver(NotificationActionIdentifier.complete, to: root, itemID: itemID)
        reconcileActions(root)
        XCTAssertEqual(itemRevision(itemID), String(baselineRevision + 1), "a duplicate does not mutate again")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM events WHERE event_type = 'completion'"), "1")
        XCTAssertEqual(try actionJournal.pending(), [])
    }

    func testResponseReceivedWhileUnauthenticatedWaitsInTheJournalAndRunsAtReconcileAfterAuthentication() throws {
        let (session, _) = makeSession(authenticated: false)
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()), readSession: session)
        defer { root.shutDown() }
        let itemID = try saveSyntheticCapture(root)
        try seedReminder(itemID: itemID)

        deliver(NotificationActionIdentifier.complete, to: root, itemID: itemID)
        reconcileActions(root)
        XCTAssertEqual(itemLifecycle(itemID), "active", "no mutation while the session is capture-only")
        XCTAssertEqual(try actionJournal.pending().count, 1, "the response waits in the journal")

        XCTAssertEqual(authenticateReadsAndWait(root), .authenticated)
        reconcileActions(root)
        XCTAssertEqual(itemLifecycle(itemID), "completed")
        XCTAssertEqual(scalar("SELECT CAST(COUNT(*) AS TEXT) FROM events WHERE event_type = 'completion'"), "1")
        XCTAssertEqual(try actionJournal.pending(), [])
    }

    func testTapCallsTheOpenItemHandlerWithTheCoreResolvedIdentifier() throws {
        let root = try makeTestRoot(sender: ScriptedProviderSender(reply: Data()))
        defer { root.shutDown() }
        let itemID = try saveSyntheticCapture(root)
        try seedReminder(itemID: itemID)
        let opened = OpenedItemsBox()
        root.setOpenItemHandler { opened.add($0) }

        deliver(UNNotificationDefaultActionIdentifier, to: root, itemID: itemID)
        reconcileActions(root)
        XCTAssertEqual(opened.all, [itemID])
        XCTAssertEqual(itemLifecycle(itemID), "active", "opening changes nothing")
    }

    func testEventsAlsoReachTheEnvironmentHandlerTheCallerSupplied() throws {
        let received = ReceivedEventsBox()
        let session = makeSession(authenticated: true).0
        let root = try ServiceCompositionRoot(
            testing: makeConfiguration(),
            environment: CompositionEnvironment(notificationEvents: { received.add($0) }),
            adapters: .injectedForTests(
                sender: ScriptedProviderSender(reply: Data()), readSession: session,
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: true)))
        defer { root.shutDown() }
        deliver(NotificationActionIdentifier.acknowledge, to: root, itemID: UUID().uuidString.lowercased())
        XCTAssertEqual(received.count, 1)
        XCTAssertNotNil(root.notificationCenterDelegate as? NotificationEventReceiver)
    }

    private final class OpenedItemsBox: @unchecked Sendable {
        private let lock = NSLock()
        private var items: [String] = []

        func add(_ item: String) {
            lock.lock()
            items.append(item)
            lock.unlock()
        }

        var all: [String] {
            lock.lock()
            defer { lock.unlock() }
            return items
        }
    }

    private final class ReceivedEventsBox: @unchecked Sendable {
        private let lock = NSLock()
        private var events: [NotificationEvent] = []

        func add(_ event: NotificationEvent) {
            lock.lock()
            events.append(event)
            lock.unlock()
        }

        var count: Int {
            lock.lock()
            defer { lock.unlock() }
            return events.count
        }
    }

    private final class FinishedFlag: @unchecked Sendable {
        private let lock = NSLock()
        private var finished = false

        func finish() {
            lock.lock()
            finished = true
            lock.unlock()
        }

        var isFinished: Bool {
            lock.lock()
            defer { lock.unlock() }
            return finished
        }
    }

    private var notificationID: String {
        NotificationIdentifier(reminderID: Self.reminderID, scheduleGeneration: 2)!.rawValue
    }

    private var actionJournal: FileNotificationActionJournal {
        FileNotificationActionJournal(
            directory: ProtectedStorageLayout(rootDirectory: storageRoot).directory(for: .temporaryFiles)
                .appendingPathComponent(NotificationActionClients.journalDirectoryName, isDirectory: true))
    }

    private func saveSyntheticCapture(_ root: ServiceCompositionRoot) throws -> String {
        var outcome: CompositionSaveOutcome?
        root.saveText(captureID: UUID().uuidString.lowercased(), text: "synthetic capture", capturedAt: Date()) {
            outcome = $0
        }
        XCTAssertTrue(pump(until: { outcome != nil }), "the save completes")
        guard case .saved(_, let itemID, _)? = outcome else {
            XCTFail("expected a confirmed save, got \(String(describing: outcome))")
            throw CoreFailure.handleClosed
        }
        return itemID
    }

    /// Adds the reminder the notification belongs to: schedule generation 2 is current and generation 1 was superseded.
    private func seedReminder(itemID: String) throws {
        var statements = [
            "INSERT INTO reminders (reminder_id, item_id, request_state, schedule_state, delivery_state, "
                + "acknowledgment_state, resolved_instant, timezone_id, schedule_generation, created_at, updated_at) "
                + "VALUES ('\(Self.reminderID)', '\(itemID)', 'resolved', 'scheduled', 'unknown', 'not_acknowledged', "
                + "'2090-01-01T09:00:00Z', 'UTC', 2, '2026-10-08T09:31:00Z', '2026-10-08T09:32:00Z')"
        ]
        for generation in 1...2 {
            let identifier = "\(Self.reminderID)#\(generation)"
            statements.append(
                "INSERT INTO reminder_operations (operation_id, reminder_id, operation_type, operation_state, "
                    + "effect_identity, external_id, created_at) VALUES ('schedule:\(identifier)', "
                    + "'\(Self.reminderID)', 'schedule', 'acknowledged', 'schedule:\(identifier)', '\(identifier)', "
                    + "'2026-10-08T09:32:00Z')")
        }
        try execute(statements)
    }

    /// Feeds one OS response through the same ingestor the installed delegate uses.
    private func deliver(_ actionIdentifier: String, to root: ServiceCompositionRoot, itemID: String) {
        root.notificationIngestor.ingestResponse(
            requestIdentifier: notificationID, actionIdentifier: actionIdentifier,
            userInfo: [NotificationContent.targetKey: itemID])
    }

    private func reconcileActions(_ root: ServiceCompositionRoot) {
        let finished = FinishedFlag()
        Task {
            await root.reconcileNotificationActions()
            finished.finish()
        }
        XCTAssertTrue(pump(until: { finished.isFinished }), "the reconcile pass finished")
    }

    private func itemLifecycle(_ itemID: String) -> String {
        scalar("SELECT lifecycle_state FROM items WHERE item_id = '\(itemID)'")
    }

    private func itemRevision(_ itemID: String) -> String {
        scalar("SELECT CAST(revision AS TEXT) FROM items WHERE item_id = '\(itemID)'")
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

    // MARK: helpers

    private func authenticateReadsAndWait(_ root: ServiceCompositionRoot) -> CompositionAuthenticationOutcome? {
        var outcome: CompositionAuthenticationOutcome?
        root.authenticateReads(reason: "test") { outcome = $0 }
        _ = pump(until: { outcome != nil }, timeout: 5)
        return outcome
    }

    /// A session over a fake boundary, already authenticated when `authenticated` is set.
    private func makeSession(authenticated: Bool) -> (ReadAuthenticationSession, FakeReadAuthenticationBoundary) {
        let boundary = FakeReadAuthenticationBoundary()
        let session = ReadAuthenticationSession(boundary: boundary)
        if authenticated {
            XCTAssertEqual(authenticateAndWait(session), .authenticated)
        }
        return (session, boundary)
    }

    private func makeConfiguration() -> CompositionConfiguration {
        CompositionConfiguration(
            storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600)
    }

    private func makeTestRoot(
        sender: ProviderRequestSender,
        notifications: NotificationCenterProviding = FakeNotificationCenter(),
        readSession: ReadAuthenticationSession? = nil,
        lifecycleNotifications: NotificationCenter = NotificationCenter(),
        attachProviderTransport: @escaping (CoreHandle, ProviderRequestSender) throws -> ProviderExchangeCoordinator = {
            try ProviderExchangeCoordinator.attach(to: $0, sender: $1)
        },
        modules: [CompositionModule] = []
    ) throws -> ServiceCompositionRoot {
        let session = readSession ?? makeSession(authenticated: true).0
        return try ServiceCompositionRoot(
            testing: makeConfiguration(),
            environment: CompositionEnvironment(
                timeZone: { Self.captureTimeZone }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender, attachProviderTransport: attachProviderTransport, readSession: session, notificationCenter: notifications,
                reachability: FakeReachability(reachable: true), lifecycleNotifications: lifecycleNotifications),
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

    private func shadowReviewConfiguration() -> ShadowReviewConfiguration {
        ShadowReviewConfiguration(
            reviewProfileVersion: ShadowReviewScenario.profileVersion,
            policy: ShadowReviewPolicy(
                isEnabled: true, samplePerMille: 1000, windowSeconds: 3600, maxRequestsPerWindow: 4,
                maxAttemptsPerSample: 2))
    }

    /// Seeds the root's store with a finished interpretation, a route with a review grant and a review profile.
    private func prepareShadowReviewStore() throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        _ = ProtectedStorageService(layout: layout).prepare()
        _ = try ShadowReviewSeed.makeStore(
            in: layout.directory(for: .database),
            scenario: ShadowReviewScenario(credentialReference: "synthetic-credential"))
    }

    private func selectCase(_ service: ShadowReviewService) throws -> String {
        var selection: ShadowReviewSelectionResult?
        service.offer(
            itemID: ShadowReviewScenario.itemID, sourceRevision: 0, requestVersion: ShadowReviewScenario.requestVersion
        ) { selection = $0 }
        XCTAssertTrue(pump(until: { selection != nil }), "the offer is answered")
        guard case .selected(let record)? = selection else {
            XCTFail("expected the case to be selected, got \(String(describing: selection))")
            throw XCTSkip("no selected case")
        }
        return record.jobID
    }

    private func shadowReviewReply() throws -> Data {
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": 4]],
        ]
        let reply: [String: Any] = [
            "id": "msg_composition_review_01",
            "type": "message",
            "role": "assistant",
            "model": ShadowReviewScenario.reviewModel,
            "content": [["type": "tool_use", "id": "toolu_composition_review_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
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
                "timezone_id": Self.captureTimeZone.identifier,
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
