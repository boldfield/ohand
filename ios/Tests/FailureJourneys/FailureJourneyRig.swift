import SQLite3
import UIKit
import UserNotifications
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// A provider transport whose reachability the test flips: it fails as the real transport does while the provider is
/// down and answers with a scripted reply once it is back. Every attempt is counted.
final class FailureJourneyProviderSender: ProviderRequestSender, @unchecked Sendable {
    enum Mode {
        case failing(ProviderTransportError)
        case replying(Data)
    }

    private let lock = NSLock()
    private var mode: Mode
    private var attempts = 0

    init(mode: Mode) {
        self.mode = mode
    }

    var attemptCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return attempts
    }

    func set(_ newMode: Mode) {
        lock.lock()
        mode = newMode
        lock.unlock()
    }

    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        lock.lock()
        attempts += 1
        let current = mode
        lock.unlock()
        switch current {
        case .failing(let error):
            throw error
        case .replying(let body):
            return ProviderHTTPResponse(status: 200, headers: ["Content-Type": "application/json"], body: body)
        }
    }
}

/// The notification permission the status screen sees, set by the test to match the fake notification center.
final class FailureJourneyPermissions: PermissionsReading, @unchecked Sendable {
    private let lock = NSLock()
    private var entry = PermissionEntry(kind: .notifications, status: .granted, change: .unchanged)

    func set(status: PermissionStatus, change: PermissionChange) {
        lock.lock()
        entry = PermissionEntry(kind: .notifications, status: status, change: change)
        lock.unlock()
    }

    func refresh() async -> [PermissionEntry] {
        lock.lock()
        defer { lock.unlock() }
        return [entry]
    }
}

private final class FailureSearchBox: @unchecked Sendable {
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

private final class FailureResultBox<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?

    var value: Result<Value, Error>? {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    func set(_ result: Result<Value, Error>) {
        lock.lock()
        stored = result
        lock.unlock()
    }
}

/// Shared rig for the failure journeys: `AppServices` over the real composition root, Rust core and an on-disk store,
/// with only the provider transport, the system notification center, the Face ID prompt and the microphone replaced by
/// deterministic stand-ins. Journeys that need a provider configure one through the root's own services.
@MainActor
class FailureJourneyTestCase: XCTestCase {
    static let routeID = "route-failure-journeys"
    /// Outside the offline grammar's closed content lexicon, so only a provider can interpret it.
    static let providerCaptureText = "Remind me 2099-01-15 11:00:00 to call the roofer about the quarterly estimate"
    static let syntheticKey = "synthetic-provider-key-do-not-leak-0002"

    var storageRoot: URL!
    var baselineHandles = 0
    var baselineBuffers = 0
    var services: AppServices!
    var sender: FailureJourneyProviderSender!
    var center: FakeNotificationCenter!
    var retrieval: RetrievalSearchModule!
    var lifecycle: LifecycleCoordinator!
    var captureEntry: CaptureEntryModel { services.captureEntry }
    let permissions = FailureJourneyPermissions()
    private let engine = SyntheticVoiceEngine()
    private let microphone = FakeMicrophonePermission()
    private let poller = ManualPoller()
    /// Kept across a restart: the credentials a person entered live in the Keychain, which outlives the process.
    private var credentials: CredentialService?
    private(set) var recognizer: FailureJourneyRecognizer?
    private(set) var grant: FakeBackgroundGrant?
    private var appNotifications = NotificationCenter()

    /// The controlled clock every production instant derives from.
    let fixedNow = Date(timeIntervalSince1970: 1_768_478_400)  // 2026-01-15T12:00:00Z

    var utcCalendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()

    /// Brings the rig up with the provider transport in `mode`. Tests call this first so the mode is theirs to pick.
    func startRig(
        providerMode: FailureJourneyProviderSender.Mode, withRecognizer: Bool = false, withBackgroundGrant: Bool = false
    ) throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("failure-journeys-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        try bringUp(providerMode: providerMode, withRecognizer: withRecognizer, withBackgroundGrant: withBackgroundGrant)
    }

    /// Ends the running process the way a clean exit does, then launches a new one over the same storage with fresh
    /// in-memory state: a new core handle, job runner, read session and notification center. The recognizer and
    /// background grant are new too, so a test scripts the second run independently of the first.
    func restartRig(
        providerMode: FailureJourneyProviderSender.Mode, withRecognizer: Bool = false, withBackgroundGrant: Bool = false
    ) throws {
        stopRig()
        try launchRig(providerMode: providerMode, withRecognizer: withRecognizer, withBackgroundGrant: withBackgroundGrant)
    }

    /// Ends the running process without waiting for its drain, leaving the storage as it was. A test may change the stored rows before it launches
    /// the next run, to leave them as a process that was killed mid-job would.
    func stopRig() {
        services.shutDown()
        services = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "the run closed every core handle")
    }

    func launchRig(
        providerMode: FailureJourneyProviderSender.Mode, withRecognizer: Bool = false, withBackgroundGrant: Bool = false
    ) throws {
        try bringUp(providerMode: providerMode, withRecognizer: withRecognizer, withBackgroundGrant: withBackgroundGrant)
    }

    private func bringUp(
        providerMode: FailureJourneyProviderSender.Mode, withRecognizer: Bool, withBackgroundGrant: Bool
    ) throws {
        sender = FailureJourneyProviderSender(mode: providerMode)
        recognizer = withRecognizer ? FailureJourneyRecognizer() : nil
        grant = withBackgroundGrant ? FakeBackgroundGrant() : nil
        center = FakeNotificationCenter()
        retrieval = RetrievalSearchModule(calendar: utcCalendar)
        lifecycle = LifecycleCoordinator()
        let lifecycleCenter = NotificationCenter()
        appNotifications = lifecycleCenter
        let credentials =
            self.credentials
            ?? CredentialService(
                serviceName: "com.boldfield.ohand.tests.failure-journeys.\(UUID().uuidString)", keychain: FakeKeychain())
        self.credentials = credentials
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                now: { [fixedNow] in fixedNow },
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: sender,
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: true),
                backgroundGrant: grant, lifecycleNotifications: lifecycleCenter, recognizer: recognizer,
                credentials: credentials),
            modules: [lifecycle, retrieval])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let manualPoller = poller
        environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: microphone, environment: environment,
            notificationCenter: NotificationCenter())
        services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, retrieval: retrieval,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter, captureNow: { [fixedNow] in fixedNow },
            isProtectedDataAvailable: { true })
        services.start(isActive: true)
    }

    override func tearDownWithError() throws {
        if let services {
            settleBackgroundWork()
            services.shutDown()
            self.services = nil
            XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
            XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        }
        if let storageRoot { try? FileManager.default.removeItem(at: storageRoot) }
        try super.tearDownWithError()
    }

    // MARK: the status screen

    /// The status model production assembles, with only the notification permission reader replaced so a test can
    /// deny and restore it.
    func makeStatusModel() -> StatusModel {
        StatusModel(
            health: LifecycleHealthReader(lifecycle: lifecycle), permissions: permissions,
            retrier: services.root.jobRunner, scheduling: RootReminderSchedulingReader(root: services.root),
            openSystemSettings: {})
    }

    func refresh(_ status: StatusModel) {
        runMain { await status.refresh() }
    }

    func retry(_ status: StatusModel) {
        runMain { await status.retry() }
    }

    func fact(_ kind: StatusFactKind, in status: StatusModel) throws -> StatusFact {
        try XCTUnwrap(status.facts.first { $0.kind == kind }, "\(kind) fact")
    }

    // MARK: provider configuration

    /// A default profile for text interpretation, granted to the capture route, so the next capture pins a provider job.
    func configureProvider() throws {
        let root = services.root
        let reference = try root.providerCredentials.addCredential(Data(Self.syntheticKey.utf8))
        let configuration = root.providerConfiguration
        let created = try call {
            try await configuration.createProfile(
                ProviderProfileDraft(
                    profileID: "profile-failure", providerProtocol: "anthropic", model: "synthetic-model",
                    credentialReference: reference, timeoutSeconds: 30))
        }
        try call {
            try await configuration.setDefault(capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        let disposition = try call {
            try await configuration.grant(
                routeID: Self.routeID, capability: "text_interpretation", profileVersion: created.profileVersion)
        }
        XCTAssertEqual(disposition, .granted)
    }

    /// The provider's answer to `providerCaptureText`: an action with an explicit reminder.
    func providerReply() throws -> Data {
        let text = Self.providerCaptureText
        let phraseStart = 10
        let phraseEnd = phraseStart + "2099-01-15 11:00:00".count
        let proposal: [String: Any] = [
            "operation": ["kind": "annotate"],
            "item_type": "action",
            "source_spans": [["start": 0, "end": text.count]],
            "reminder_proposal": [
                "instant": "2099-01-15T11:00:00Z",
                "timezone_id": "UTC",
                "quality": "explicit",
                "source_span": ["start": phraseStart, "end": phraseEnd],
            ],
        ]
        let reply: [String: Any] = [
            "id": "msg_failure_journey_01",
            "type": "message",
            "role": "assistant",
            "model": "synthetic-model",
            "content": [["type": "tool_use", "id": "toolu_failure_journey_01", "name": "interpret", "input": proposal]],
            "stop_reason": "tool_use",
            "usage": ["input_tokens": 42, "output_tokens": 17],
        ]
        return try JSONSerialization.data(withJSONObject: reply)
    }

    // MARK: driving the app

    func unlock() {
        services.root.authenticateReads(reason: "failure journeys") { _ in }
        XCTAssertTrue(pump { self.services.root.readScope == .authenticated }, "the read session authenticated")
    }

    /// Saves through the capture surface the app shows and returns the item the core confirmed.
    func captureSilently(_ text: String) throws -> String {
        let previousItemID = captureEntry.acknowledgment?.savedItemID
        captureEntry.textModel.updateText(text)
        captureEntry.saveText()
        XCTAssertTrue(
            pump(timeout: 10) {
                guard let savedItemID = self.captureEntry.acknowledgment?.savedItemID else { return false }
                return savedItemID != previousItemID
            }, "the core confirmed the save")
        return try XCTUnwrap(captureEntry.acknowledgment?.savedItemID)
    }

    /// The app goes to the background, as the system announces it to the running app.
    func enterBackground() {
        appNotifications.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        let delivered = Date().addingTimeInterval(0.3)
        _ = pump(timeout: 1) { Date() >= delivered }
    }

    /// Records and stops one voice capture through the controller the app's capture surface uses, and returns the item
    /// the core confirmed. The synthetic engine writes real audio; only the speech recognizer is a stand-in.
    func recordVoiceCapture() throws -> String {
        let controller = services.voice.controller
        var started: Result<VoiceRecordingStarted, VoiceStartFailure>?
        controller.start { started = $0 }
        _ = try XCTUnwrap(started, "the microphone was granted, so start completes at once").get()
        controller.stop()
        XCTAssertTrue(pump(timeout: 10) { controller.lastOutcome != nil }, "the recording ended")
        guard case .saved(_, let acknowledgment)? = controller.lastOutcome else {
            XCTFail("expected a saved voice capture, got \(String(describing: controller.lastOutcome))")
            throw CoreFailure.handleClosed
        }
        return acknowledgment.itemID
    }

    func search(_ request: RetrievalSearchRequest) -> Result<RetrievalPage, RetrievalSearchFailure> {
        let box = FailureSearchBox()
        retrieval.search(request) { box.set($0) }
        XCTAssertTrue(pump { box.value != nil }, "the search never completed")
        return box.value ?? .failure(.unavailable)
    }

    /// Runs an async call to completion from a synchronous test, turning the main run loop meanwhile.
    func call<Value>(_ operation: @escaping @Sendable () async throws -> Value) throws -> Value {
        let box = FailureResultBox<Value>()
        Task {
            do { box.set(.success(try await operation())) } catch { box.set(.failure(error)) }
        }
        XCTAssertTrue(pump { box.value != nil }, "the call never completed")
        return try XCTUnwrap(box.value).get()
    }

    func runMain(_ operation: @escaping @MainActor () async -> Void) {
        let finished = expectation(description: "main-actor call")
        Task { @MainActor in
            await operation()
            finished.fulfill()
        }
        wait(for: [finished], timeout: 30)
    }

    func settleBackgroundWork() {
        _ = pump(timeout: 30) { !self.services.root.jobRunner.isDraining }
        let settled = Date().addingTimeInterval(1.5)
        _ = pump(timeout: 2) { Date() >= settled }
    }

    @discardableResult
    func pump(timeout: TimeInterval = 30, until condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    // MARK: persisted state

    func scalar(_ sql: String) -> String {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        guard sqlite3_open(path, &database) == SQLITE_OK else { return "" }
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK else { return "" }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0) else { return "" }
        return String(cString: text)
    }

    /// Runs SQL against the store file, which the core also has open; writes wait on the busy timeout.
    func execute(_ statement: String) {
        var database: OpaquePointer?
        let path = ProtectedStorageLayout(rootDirectory: storageRoot).databaseURL.path
        XCTAssertEqual(sqlite3_open(path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        var message: UnsafeMutablePointer<CChar>?
        let code = sqlite3_exec(database, statement, nil, nil, &message)
        let detail = message.map { String(cString: $0) } ?? ""
        sqlite3_free(message)
        XCTAssertEqual(code, SQLITE_OK, "statement failed: \(detail)")
    }

    func itemColumn(_ column: String, of itemID: String) -> String {
        scalar("SELECT \(column) FROM items WHERE item_id = '\(itemID)'")
    }

    func savedText(of itemID: String) -> String {
        scalar(
            "SELECT captures.text FROM captures JOIN items ON items.capture_id = captures.capture_id "
                + "WHERE items.item_id = '\(itemID)'")
    }

    func jobColumn(_ column: String, of itemID: String) -> String {
        scalar("SELECT \(column) FROM jobs WHERE item_id = '\(itemID)' ORDER BY created_at DESC LIMIT 1")
    }
}

/// An on-device recognizer the test controls. Each call waits until the test answers it, and gives up as cancelled
/// when the job runner cancels the work, as the speech framework's task does.
final class FailureJourneyRecognizer: OnDeviceRecognizing, @unchecked Sendable {
    let recognizerName = "failure-journey-recognizer/1.0"
    private let lock = NSLock()
    private var scripted: OnDeviceRecognitionOutcome?
    private var calls = 0
    private var cancellations = 0

    var callCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return calls
    }

    var cancellationCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return cancellations
    }

    /// Answers the call in flight, and every later one, with `outcome`.
    func answer(with outcome: OnDeviceRecognitionOutcome) {
        lock.lock()
        scripted = outcome
        lock.unlock()
    }

    func recognize(audioURL: URL, localeIdentifier: String) async -> OnDeviceRecognitionOutcome {
        lock.lock()
        calls += 1
        lock.unlock()
        while true {
            lock.lock()
            let ready = scripted
            lock.unlock()
            if let ready { return ready }
            if Task.isCancelled {
                lock.lock()
                cancellations += 1
                lock.unlock()
                return .cancelled
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
    }
}
