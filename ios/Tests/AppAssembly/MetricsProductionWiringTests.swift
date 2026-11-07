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

    /// Saves a reminder through the root itself, not the capture entry model, so the save records no timing.
    private func saveReminder(_ services: AppServices) {
        let box = AnswerBox<CompositionSaveOutcome>()
        services.root.saveText(
            captureID: "capture-metrics-sink", text: "remind me 2099-01-01 09:00:00 to call mom", capturedAt: Date()
        ) { box.set($0) }
        waitUntilPumping("the reminder was saved", timeout: 30) { box.value != nil }
        guard case .saved? = box.value else { return XCTFail("the reminder was not saved: \(String(describing: box.value))") }
    }

    /// The reconciler keeps its sink only inside a native-effects closure, so its identity is shown by behaviour: once
    /// the metrics client is shut down the core refuses every record, and a refused record is counted only by the
    /// `MetricsService` instance that submitted it. A reconciler given any other sink, even a second service over the
    /// same client, would leave the root's count at zero.
    func testTheReminderReconcilerRecordsThroughTheRootsOneMetricsService() throws {
        center.authorizationStatus = .denied
        let services = try makeServices()
        authenticate(services)
        services.start(isActive: true)
        let service = services.root.metricsService
        services.root.metricsClient.shutDown()
        XCTAssertEqual(service.droppedRecordCount, 0)

        saveReminder(services)
        let root = services.root
        eventually("the reconciler's refused scheduling failure is counted by the root's metrics service") {
            _ = try? await root.reconcileReminders()
            return service.droppedRecordCount > 0
        }
    }

    /// The control for the test above: the same save and reminder pass with permission granted records no scheduling
    /// failure, and nothing else in that flow is counted, so the count above can only come from the reconciler.
    func testTheSameFlowWithPermissionGrantedLeavesTheRootsDropCountAtZero() throws {
        center.authorizationStatus = .authorized
        let services = try makeServices()
        authenticate(services)
        services.start(isActive: true)
        let service = services.root.metricsService
        services.root.metricsClient.shutDown()

        saveReminder(services)
        let root = services.root
        let center = self.center
        eventually("the reminder is scheduled") {
            _ = try? await root.reconcileReminders()
            return !center.addedRequests.isEmpty
        }
        XCTAssertEqual(service.droppedRecordCount, 0, "only a scheduling failure is recorded in this flow")
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
        let exported = try XCTUnwrap(run { try? await control.prepareExport() } ?? nil, "the export was prepared")
        defer { control.cancelExport(exportID: exported.id) }
        let text = try String(contentsOf: exported.file, encoding: .utf8)
        XCTAssertTrue(text.contains("trigger_to_ready"))
    }

    func testASchedulingFailureOfTheReminderReconcilerAppearsInTheSameExport() throws {
        center.authorizationStatus = .denied
        let services = try makeServices()
        authenticate(services)
        services.start(isActive: true)
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

    /// The exported file of a real session holds no capture text and no identifier: the capture is saved through the
    /// production assembly, every sink records, and the file is read back from the share-sheet export.
    func testTheExportedFileOfARealSessionHoldsNoCaptureTextAndNoIdentifier() throws {
        center.authorizationStatus = .denied
        let services = try makeServices()
        authenticate(services)
        services.start(isActive: true)

        let service = services.root.metricsService
        let root = services.root
        let captureText = "remind me 2099-01-01 09:00:00 to call mom"
        services.captureEntry.textModel.updateText(captureText)
        services.captureEntry.saveText()
        eventually("the withheld schedule is counted as a scheduling failure") {
            _ = try? await root.reconcileReminders()
            guard let export = try? await service.exportReport() else { return false }
            return export.report.failures.contains { $0.series == "scheduling_failure" }
        }
        services.captureEntry.present(
            CaptureEntryPresentation(
                entryID: "entry-privacy-1", source: .controlIntent, mode: .silent, launchKind: .cold,
                access: CaptureEntryAccess(protectedDataAvailable: true, readAuthorized: true),
                triggeredAtUptimeNanoseconds: DispatchTime.now().uptimeNanoseconds))
        services.captureEntry.surfaceDidBecomeReady()
        eventually("the trigger-to-ready figure is recorded") {
            guard let export = try? await service.exportReport() else { return false }
            return export.report.timings.contains { $0.series == "trigger_to_ready" }
        }

        let control = services.root.metricsReport
        let exported = try XCTUnwrap(run { try? await control.prepareExport() } ?? nil, "the export was prepared")
        defer { control.cancelExport(exportID: exported.id) }
        let data = try Data(contentsOf: exported.file)
        let text = try XCTUnwrap(String(data: data, encoding: .utf8))

        for fragment in ["call mom", "remind me", "2099", "entry-privacy-1", "entry-", "item-", "job-"] {
            XCTAssertFalse(text.contains(fragment), "the file must not hold \(fragment)")
        }
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(
            Set(object.keys),
            ["schema", "generated_at", "recording_enabled", "sample_limit_per_series", "build", "timings", "failures"])
        let build = try XCTUnwrap(object["build"] as? [String: Any])
        XCTAssertEqual(Set(build.keys), ["version", "build_number", "revision", "dirty"])
        XCTAssertEqual(build["version"] as? String, CoreMetricsBuild.current()?.version)
        let machineLabels: Set<String> = [
            "trigger_to_ready", "end_of_input_to_save", "cold", "warm", "locked_handoff", "voice", "text",
            "processing_failure", "scheduling_failure", "timeout", "unavailable", "rate_limited", "unauthorized",
            "invalid_output", "capability_unavailable", "unsupported_language", "interrupted", "retries_exhausted",
            "other", "permission_denied", "capacity_exceeded", "unschedulable", "native_failure",
        ]
        for key in ["timings", "failures"] {
            for entry in object[key] as? [[String: Any]] ?? [] {
                let labels = [entry["series"], entry["dimension"]].compactMap { $0 as? String }
                XCTAssertEqual(labels.count, 2)
                XCTAssertTrue(Set(labels).isSubset(of: machineLabels), "\(labels) are machine labels")
            }
        }
    }
}
