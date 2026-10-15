import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

final class AppServicesTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var created: [AppServices] = []

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("app-services-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        created = []
    }

    override func tearDownWithError() throws {
        created.forEach { $0.shutDown() }
        created = []
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func makeServices(
        handoff: CaptureEntryHandoff = CaptureEntryHandoff(notificationCenter: NotificationCenter()),
        notificationCenter: NotificationCenter = NotificationCenter(),
        voice: VoiceCaptureServices? = nil,
        protectedDataAvailable: Bool = true
    ) throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: handoff, voice: voice,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: notificationCenter,
            isProtectedDataAvailable: { protectedDataAvailable })
        created.append(services)
        return services
    }

    // MARK: real services

    func testProductionServicesResolveTheRealAdaptersAndTheLifecycleModule() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)

        let root = services.root
        XCTAssertEqual(root.adapterSet, .production)
        XCTAssertTrue(root.adapters.sender is ProviderHTTPTransport)
        XCTAssertTrue(root.adapters.notificationCenter is SystemNotificationCenter)
        XCTAssertTrue(root.adapters.reachability is SystemNetworkReachability)
        XCTAssertTrue(root.adapters.backgroundGrant is ApplicationBackgroundExecutionGrant)
        XCTAssertEqual(root.configuration.captureRouteID, AppConfiguration.universalRouteID)
        XCTAssertEqual(root.configuration.storageRootPath, storageRoot.path)
        XCTAssertNotNil(
            services.lifecycle.processingHealth, "the root attached the lifecycle module that owns launch work")
    }

    func testEveryLaunchStartsLockedSoNothingStoredIsReadable() throws {
        let services = try makeServices()
        XCTAssertEqual(services.root.readScope, .captureOnly)
        XCTAssertFalse(services.captureEntry.access.readAuthorized)
        XCTAssertTrue(services.captureEntry.access.protectedDataAvailable)
    }

    func testProtectedDataThatIsNotAvailableIsShownOnTheCaptureSurface() throws {
        let services = try makeServices(protectedDataAvailable: false)
        XCTAssertFalse(services.captureEntry.access.protectedDataAvailable)
        XCTAssertNotNil(services.captureEntry.protectedDataNotice)
    }

    // MARK: capture first

    func testTheAppOpensOnSilentCaptureWithNoManagementScreenShown() throws {
        let services = try makeServices()
        XCTAssertEqual(services.captureEntry.mode, .silent)
        XCTAssertEqual(services.navigation.path, [])
        XCTAssertNil(services.captureEntry.acknowledgment)
        XCTAssertTrue(services.captureEntry.isIdle)
    }

    func testAStoppedRootKeepsTheTypedTextAndSaysNothingWasSaved() throws {
        let services = try makeServices()
        services.root.shutDown()

        services.captureEntry.textModel.updateText("remind me to call the roofer")
        services.captureEntry.saveText()

        XCTAssertEqual(services.captureEntry.textModel.text, "remind me to call the roofer")
        XCTAssertEqual(services.captureEntry.acknowledgment?.save, .notSaved)
    }

    // MARK: entries

    func testAnAnnouncedEntryLandsOnCaptureAndLeavesAnyManagementScreen() throws {
        let center = NotificationCenter()
        let handoff = CaptureEntryHandoff(notificationCenter: center)
        let services = try makeServices(handoff: handoff, notificationCenter: center)
        services.navigation.path = [ManagementCatalog.settingsEntryID]
        services.sceneWillEnterForeground()

        handoff.registerAndAnnounce(source: .controlIntent, requestedMode: .voice)
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))

        XCTAssertEqual(services.captureEntry.mode, .voice)
        XCTAssertEqual(services.captureEntry.presentation?.source, .controlIntent)
        XCTAssertEqual(services.navigation.path, [])
    }

    func testAShortcutURLPresentsTheRequestedMode() throws {
        let services = try makeServices()
        services.sceneWillEnterForeground()

        services.receive(url: URL(string: "ohand-capture://capture?mode=voice")!)

        XCTAssertEqual(services.captureEntry.mode, .voice)
        XCTAssertEqual(services.captureEntry.presentation?.source, .shortcutURL)
    }

    func testAnEntryThatArrivesInTheBackgroundIsPresentedOnTheNextForeground() throws {
        let services = try makeServices()
        services.sceneDidEnterBackground()

        services.receive(url: URL(string: "ohand-capture://capture")!)
        XCTAssertNil(services.captureEntry.presentation)

        services.sceneWillEnterForeground()
        XCTAssertEqual(services.captureEntry.presentation?.source, .shortcutURL)
    }

    func testAnUnrelatedURLPresentsNothing() throws {
        let services = try makeServices()
        services.sceneWillEnterForeground()
        services.receive(url: URL(string: "https://example.com/capture")!)
        XCTAssertNil(services.captureEntry.presentation)
    }

    // MARK: launcher

    func testTheLauncherBuildsAndStartsTheServicesOnce() throws {
        var started: [Bool] = []
        var builds = 0
        let launcher = AppLauncher(
            makeServices: { [unowned self] in
                builds += 1
                return try self.makeServices()
            },
            startServices: { _, isActive in started.append(isActive) },
            isApplicationActive: { false })

        launcher.launch()
        launcher.launch()

        XCTAssertNotNil(launcher.services)
        XCTAssertEqual(builds, 1)
        XCTAssertEqual(started, [false])
    }

    func testAFailureToBuildLeavesNoServicesAndALaterRetrySucceeds() throws {
        struct Unavailable: Error {}
        var attempts = 0
        let launcher = AppLauncher(
            makeServices: { [unowned self] in
                attempts += 1
                if attempts == 1 { throw Unavailable() }
                return try self.makeServices()
            },
            startServices: { _, _ in },
            isApplicationActive: { false })

        launcher.launch()
        XCTAssertNil(launcher.services)

        launcher.launch()
        XCTAssertNotNil(launcher.services)
        XCTAssertEqual(attempts, 2)
    }

    func testARetryAfterTheAppIsActiveStillPresentsAPendingEntry() throws {
        let handoff = CaptureEntryHandoff(notificationCenter: NotificationCenter())
        handoff.register(source: .shortcutURL, requestedMode: .voice)
        let launcher = AppLauncher(
            makeServices: { [unowned self] in try self.makeServices(handoff: handoff) },
            startServices: { _, _ in },
            isApplicationActive: { true })

        launcher.launch()

        XCTAssertEqual(launcher.services?.captureEntry.mode, .voice)
    }

    func testAForegroundAfterAFailedBuildRetriesAndPresentsAPendingEntry() throws {
        struct Unavailable: Error {}
        let handoff = CaptureEntryHandoff(notificationCenter: NotificationCenter())
        handoff.register(source: .shortcutURL, requestedMode: .voice)
        var attempts = 0
        let launcher = AppLauncher(
            makeServices: { [unowned self] in
                attempts += 1
                if attempts == 1 { throw Unavailable() }
                return try self.makeServices(handoff: handoff)
            },
            startServices: { _, _ in },
            isApplicationActive: { false })

        launcher.launch()
        XCTAssertNil(launcher.services)

        launcher.sceneWillEnterForeground()

        XCTAssertNotNil(launcher.services, "the foreground retried the build")
        XCTAssertEqual(attempts, 2)
        XCTAssertEqual(launcher.services?.captureEntry.mode, .voice, "the foreground event the build missed was delivered")

        launcher.sceneWillEnterForeground()
        XCTAssertEqual(attempts, 2, "with services present a foreground does not rebuild")
    }

    func testTheProductionStorageRootIsInsideApplicationSupport() throws {
        let root = try AppConfiguration.productionStorageRoot()
        XCTAssertTrue(root.path.contains("Application Support"))
        XCTAssertEqual(root.lastPathComponent, "Store")
    }

    // MARK: release target

    func testTheAppBundleEmbedsNoProbeTarget() throws {
        let bundleURL = Bundle.main.bundleURL
        for directory in ["Frameworks", "PlugIns"] {
            let entries = (try? FileManager.default.contentsOfDirectory(
                atPath: bundleURL.appendingPathComponent(directory).path)) ?? []
            XCTAssertFalse(
                entries.contains { $0.localizedCaseInsensitiveContains("probe") },
                "\(directory) must not carry a probe: \(entries)")
        }
        XCTAssertEqual(Bundle.main.bundleIdentifier, "com.boldfield.ohand.app")
    }
}
