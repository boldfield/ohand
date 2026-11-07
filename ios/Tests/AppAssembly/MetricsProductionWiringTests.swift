import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// The production assembly over a fresh store: the capture entry model, the job runner and the reminder reconciler
/// record to one metrics service, and what they record reaches the export. Only the platform adapters are replaced.
final class MetricsProductionWiringTests: XCTestCase {
    private final class UnreachableProviderSender: ProviderRequestSender, @unchecked Sendable {
        func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
            throw URLError(.notConnectedToInternet)
        }
    }

    private var storageRoot: URL!
    private var coreLeakBaseline = CoreLeakBaseline.empty
    private var created: [AppServices] = []
    private let center = FakeNotificationCenter()

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("metrics-wiring-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        coreLeakBaseline = CoreLeakBaseline.capture()
        created = []
    }

    override func tearDownWithError() throws {
        created.forEach { $0.shutDown() }
        created = []
        coreLeakBaseline.check()
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func makeServices() throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnreachableProviderSender(),
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: center, reachability: FakeReachability(reachable: false)),
            modules: [lifecycle])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let voice = VoiceCaptureServices(
            root: root, engine: SyntheticVoiceEngine(), permission: FakeMicrophonePermission(),
            environment: environment, notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: NotificationCenter(), isProtectedDataAvailable: { true })
        created.append(services)
        return services
    }

    private func authenticate(_ services: AppServices) {
        services.root.authenticateReads(reason: "test") { _ in }
        XCTAssertEqual(services.root.readScope, .authenticated)
    }

    /// The sink a type keeps in its `metrics` property, found by reflection because the property is private.
    private func sink(of object: Any) -> AnyObject? {
        for child in Mirror(reflecting: object).children where child.label == "metrics" {
            let inner = Mirror(reflecting: child.value)
            if inner.displayStyle == .optional { return inner.children.first?.value as? AnyObject }
            return child.value as AnyObject
        }
        return nil
    }

    private final class AnswerBox<Value>: @unchecked Sendable {
        private let lock = NSLock()
        private var stored: Value?
        var value: Value? {
            lock.lock()
            defer { lock.unlock() }
            return stored
        }
        func set(_ newValue: Value) {
            lock.lock()
            stored = newValue
            lock.unlock()
        }
    }

    /// Runs an async operation from a synchronous test, turning the run loop until it answers: the root's own work
    /// needs the main run loop, so the test must not block it.
    private func run<Value>(_ operation: @escaping @Sendable () async -> Value) -> Value? {
        let box = AnswerBox<Value>()
        Task { box.set(await operation()) }
        waitUntilPumping("the call completed", timeout: 30) { box.value != nil }
        return box.value
    }

    private func eventually(_ condition: String, _ holds: @escaping @Sendable () async -> Bool) {
        let deadline = Date().addingTimeInterval(30)
        while Date() < deadline {
            if run(holds) == true { return }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.05))
        }
        XCTFail("timed out waiting for: \(condition)")
    }

    func testTheRootOffersOneSinkAndTheJobRunnerKeepsIt() throws {
        let services = try makeServices()
        let service = services.root.metricsService
        XCTAssertTrue(services.root.metrics as AnyObject === service)
        let runnerSink = try XCTUnwrap(sink(of: services.root.jobRunner), "the job runner was given a metrics sink")
        XCTAssertTrue(runnerSink === service, "the job runner records to the root's one metrics service")
        let entrySink = try XCTUnwrap(sink(of: services.captureEntry), "the capture entry model was given a metrics sink")
        XCTAssertTrue(entrySink === service, "the capture entry model records to the root's one metrics service")
    }

    func testATriggerToReadyRecordMadeThroughTheCaptureEntryModelAppearsInTheExport() throws {
        let services = try makeServices()
        authenticate(services)
        services.captureEntry.present(
            CaptureEntryPresentation(
                entryID: "entry-1", source: .controlIntent, mode: .silent, launchKind: .cold,
                access: CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true),
                triggeredAtUptimeNanoseconds: DispatchTime.now().uptimeNanoseconds))
        services.captureEntry.surfaceDidBecomeReady()

        let service = services.root.metricsService
        eventually("the trigger-to-ready figure is in the export") {
            guard let export = try? await service.exportReport() else { return false }
            return export.report.timings.contains { $0.series == "trigger_to_ready" && $0.dimension == "cold" }
        }
        let control = services.root.metricsReport
        let exported = try XCTUnwrap(run { try? await control.prepareExport() }, "the export was prepared")
        defer { control.cancelExport(exportID: exported.id) }
        let text = try String(contentsOf: exported.file, encoding: .utf8)
        XCTAssertTrue(text.contains("trigger_to_ready"))
    }

    func testASchedulingFailureOfTheReminderReconcilerAppearsInTheSameExport() throws {
        center.authorizationStatus = .denied
        let services = try makeServices()
        authenticate(services)
        services.captureEntry.textModel.updateText("remind me 2099-01-01 09:00:00 to call mom")
        services.captureEntry.saveText()

        let service = services.root.metricsService
        let root = services.root
        eventually("a reminder pass counts the withheld schedule") {
            _ = try? await root.reconcileReminders()
            guard let export = try? await service.exportReport() else { return false }
            return export.report.failures.contains { $0.series == "scheduling_failure" && $0.dimension == "permission_denied" }
        }
    }
}
