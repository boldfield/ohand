import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class TimeChangePassLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [(step: LifecycleStep, triggers: Set<LifecycleTrigger>)] = []

    var entries: [(step: LifecycleStep, triggers: Set<LifecycleTrigger>)] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(step: LifecycleStep, triggers: Set<LifecycleTrigger>) {
        lock.lock()
        recorded.append((step, triggers))
        lock.unlock()
    }
}

/// Stands in for the OS notification prompt: records how often it was asked and answers as scripted, optionally only
/// after a gate opens so concurrent reports can overlap.
private final class ScriptedPermissionAsker: NotificationPermissionAsking, @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    private var open: Bool
    private let outcome: NotificationPermissionAskOutcome

    init(outcome: NotificationPermissionAskOutcome, gated: Bool = false) {
        self.outcome = outcome
        open = !gated
    }

    var askCount: Int {
        lock.lock(); defer { lock.unlock() }
        return count
    }

    func openGate() {
        lock.lock(); open = true; lock.unlock()
    }

    func askIfNeverRequested() async -> NotificationPermissionAskOutcome {
        lock.lock(); count += 1; lock.unlock()
        while true {
            lock.lock(); let isOpen = open; lock.unlock()
            if isOpen { return outcome }
            try? await Task.sleep(nanoseconds: 2_000_000)
        }
    }
}

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
        baselineBuffers = settledLiveResultBufferCount()
        created = []
    }

    override func tearDownWithError() throws {
        created.forEach { $0.shutDown() }
        // A core read issued before the shutdown completes on its own queue; wait for it and for its buffer to be freed.
        let pending = created
        created = []
        let deadline = Date().addingTimeInterval(5)
        while pending.contains(where: { $0.homeRecents.readsInFlight > 0 })
            || CoreHandle.liveResultBufferCount != baselineBuffers, Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func makeServices(
        handoff: CaptureEntryHandoff = CaptureEntryHandoff(notificationCenter: NotificationCenter()),
        notificationCenter: NotificationCenter = NotificationCenter(),
        voice: VoiceCaptureServices? = nil,
        timeSources: TimeSources = .system,
        protectedDataAvailable: Bool = true,
        notificationPermission: NotificationPermissionAsking = ScriptedPermissionAsker(
            outcome: .alreadyDecided(.usable)),
        isForeground: @escaping () -> Bool = { true }
    ) throws -> AppServices {
        // The root gets injected adapters, not the production ones: those listen on `NotificationCenter.default`, where the
        // host application's own activation callbacks (and any other test's posts) would start lifecycle passes in the
        // middle of a test, and they watch the real network and use the real Keychain service. A private center keeps
        // every pass attributable to what the test did.
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            testing: AppConfiguration.production(storageRoot: storageRoot),
            adapters: .injectedForTests(
                sender: QuietUnreachableSender(),
                readSession: ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary()),
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false),
                lifecycleNotifications: NotificationCenter()),
            modules: [lifecycle])
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: handoff, voice: voice,
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: notificationCenter,
            timeSources: timeSources,
            notificationPermission: notificationPermission,
            isForeground: isForeground,
            isProtectedDataAvailable: { protectedDataAvailable })
        created.append(services)
        return services
    }

    // MARK: notification permission at the reminder boundary

    private func reminderReport(_ outlook: ReminderOutlook?) -> NotificationPermissionReport {
        let reminders = outlook.map { [ReminderStatus(facts: ReminderFixtures.resolved("item-1"), outlook: $0)] } ?? []
        return NotificationPermissionReport(
            permission: .notRequested, transition: .unchanged, trigger: .explicit,
            readiness: NotificationReadinessReport(permission: .notRequested, reminders: reminders),
            promptsGranted: 0, promptsDeferred: 0, promptsWithheld: 0, reconciliationFailure: nil, converged: true)
    }

    /// A fixed pause, only for proving that nothing happens (after a positive wait for what should have happened).
    private func settle() async {
        try? await Task.sleep(nanoseconds: 100_000_000)
    }

    func testAReminderThatCannotAlertAsksOnceInTheForegroundThenRunsAReminderPass() async throws {
        let asker = ScriptedPermissionAsker(outcome: .asked(.usable))
        let services = try makeServices(notificationPermission: asker)
        XCTAssertNil(services.root.lastReminderReport)

        services.handleReminderReport(reminderReport(.undeliverable(.permissionNotRequested)))

        let passed = await eventually { services.root.lastReminderReport != nil }
        XCTAssertTrue(passed, "a reminder pass follows the answer")
        XCTAssertEqual(asker.askCount, 1)
    }

    func testTheAskWaitsForTheForegroundAndHappensOnTheNextActivation() async throws {
        let asker = ScriptedPermissionAsker(outcome: .asked(.usable))
        var foreground = false
        let center = NotificationCenter()
        let services = try makeServices(
            notificationCenter: center, notificationPermission: asker, isForeground: { foreground })
        services.start(isActive: false)

        services.handleReminderReport(reminderReport(.undeliverable(.permissionNotRequested)))
        await settle()
        XCTAssertEqual(asker.askCount, 0, "nothing is asked while backgrounded")

        foreground = true
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let asked = await eventually { asker.askCount == 1 }
        XCTAssertTrue(asked, "the pending ask happens on activation")
        let passed = await eventually { services.root.lastReminderReport != nil }
        XCTAssertTrue(passed)

        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        await settle()
        XCTAssertEqual(asker.askCount, 1, "an answered ask is not pending any more")
    }

    func testReportsWithoutAReminderThatNeedsPermissionNeverAsk() async throws {
        let asker = ScriptedPermissionAsker(outcome: .asked(.usable))
        let services = try makeServices(notificationPermission: asker)
        services.start(isActive: false)

        services.handleReminderReport(reminderReport(nil))
        services.handleReminderReport(reminderReport(.undeliverable(.permissionDenied)))
        services.handleReminderReport(reminderReport(.scheduled))
        await settle()

        XCTAssertEqual(asker.askCount, 0, "launch and non-qualifying reports ask nothing")
    }

    func testConcurrentReportsShareOneAsk() async throws {
        let asker = ScriptedPermissionAsker(outcome: .asked(.denied), gated: true)
        let services = try makeServices(notificationPermission: asker)
        let report = reminderReport(.undeliverable(.permissionNotRequested))

        services.handleReminderReport(report)
        services.handleReminderReport(report)
        services.handleReminderReport(report)
        let asked = await eventually { asker.askCount >= 1 }
        XCTAssertTrue(asked)
        asker.openGate()
        let passed = await eventually { services.root.lastReminderReport != nil }
        XCTAssertTrue(passed, "the answer is followed by one reminder pass")

        XCTAssertEqual(asker.askCount, 1)
    }

    func testAPromptThatLeavesThePermissionUndecidedDoesNotLoopOrReconcile() async throws {
        let asker = ScriptedPermissionAsker(outcome: .asked(.notRequested))
        let center = NotificationCenter()
        let services = try makeServices(notificationCenter: center, notificationPermission: asker)
        services.start(isActive: false)
        let report = reminderReport(.undeliverable(.permissionNotRequested))

        services.handleReminderReport(report)
        let asked = await eventually { asker.askCount == 1 }
        XCTAssertTrue(asked)
        await settle()
        services.handleReminderReport(report)
        await settle()

        XCTAssertEqual(asker.askCount, 1, "one ask per foreground stay")
        XCTAssertNil(services.root.lastReminderReport, "no pass ran, so no report can ask again")

        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        services.handleReminderReport(report)
        let askedAgain = await eventually { asker.askCount == 2 }
        XCTAssertTrue(askedAgain, "the next foreground stay may ask again")
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

    func testTheProductionSearchScreenStaysLockedUntilTheSessionIsAuthenticated() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)

        let search = services.managementContext.retrieval
        search.query = "roofer"
        search.search()

        XCTAssertEqual(search.phase, .locked, "a capture-only session never reaches the core or shows a result")
        XCTAssertTrue(search.rows.isEmpty)
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

    // MARK: time changes

    func testAStartedAppReconcilesOnceWhenASystemTimeZoneCallbackReportsARealChange() async throws {
        let callbacks = NotificationCenter()
        let time = ManualTime(wall: Date(), zone: TimeZone(identifier: "America/New_York")!)
        let services = try makeServices(notificationCenter: callbacks, timeSources: time.sources)
        services.start(isActive: false)
        // This test checks the wiring from the callback to one `.systemTimeChange` pass. Starting connects the real
        // effectful steps (reset resume, the notification adapter, prompt eligibility), so recording actions replace them
        // afterwards and the pass does not depend on runner speed. `TimeChangeRealCoreTests` and
        // `LifecycleCoordinatorTests` cover the real steps.
        let passLog = TimeChangePassLog()
        for step in [LifecycleStep.reset, .reminders, .promptEligibility] {
            services.lifecycle.setAction({ triggers in
                passLog.record(step: step, triggers: triggers)
                return .completed
            }, for: step)
        }
        XCTAssertEqual(services.lifecycle.finishedPassCount, 0, "starting inactive runs no pass")

        // A callback that finds nothing changed requests nothing.
        let evaluationsBefore = services.timeChange.evaluationsPerformed
        callbacks.post(name: .NSSystemTimeZoneDidChange, object: nil)
        let heldNoChange = await timeChangeWaitUntil { time.sleeperCount == 1 }
        XCTAssertTrue(heldNoChange)
        time.releaseTimers()
        let compared = await timeChangeWaitUntil { services.timeChange.evaluationsPerformed > evaluationsBefore }
        XCTAssertTrue(compared, "the settled reading was compared before the absence of a pass is asserted")
        XCTAssertEqual(services.timeChange.reportedChangeCount, 0)
        XCTAssertEqual(services.lifecycle.finishedPassCount, 0, "no change, no pass")

        // The person lands in another zone and the system repeats the callback.
        time.travel(to: TimeZone(identifier: "America/Los_Angeles")!)
        for _ in 0..<3 { callbacks.post(name: .NSSystemTimeZoneDidChange, object: nil) }
        let held = await timeChangeWaitUntil { time.sleeperCount == 3 }
        XCTAssertTrue(held)
        time.releaseTimers()
        let reconciled = await timeChangeWaitUntil { services.lifecycle.finishedPassCount == 1 }
        XCTAssertTrue(reconciled)
        XCTAssertEqual(services.lifecycle.lastReport?.triggers, [.systemTimeChange])
        XCTAssertEqual(services.timeChange.reportedChangeCount, 1)
        XCTAssertEqual(
            passLog.entries.map(\.step.rawValue).sorted(), ["promptEligibility", "reminders", "reset"],
            "each recorded step ran once; the reminders step waits on the reset resume, so it runs too")
        XCTAssertTrue(passLog.entries.allSatisfy { $0.triggers == [.systemTimeChange] })
    }

    // MARK: entries

    func testAnAnnouncedEntryLandsOnCaptureAndLeavesAnyManagementScreen() throws {
        let center = NotificationCenter()
        let handoff = CaptureEntryHandoff(notificationCenter: center)
        let services = try makeServices(handoff: handoff, notificationCenter: center)
        services.navigation.path = [ManagementCatalog.settingsEntryID]
        services.sceneWillEnterForeground()

        handoff.registerAndAnnounce(source: .controlIntent, requestedMode: .voice)
        waitUntilPumping("the announced entry is presented and the management screen is left") {
            services.captureEntry.mode == .voice && services.captureEntry.presentation?.source == .controlIntent
                && services.navigation.path.isEmpty
        }

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

    @MainActor
    func testTheUnitTestHostBuiltNoProductionServices() throws {
        let hostLauncher = try XCTUnwrap(
            (UIApplication.shared.delegate as? AppDelegate)?.launcher, "the hosted app has its launcher")
        hostLauncher.launch()
        XCTAssertNil(hostLauncher.services, "the host never builds production services while hosting unit tests")
    }

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

    // MARK: quiet authentication

    private func makeQuietServices(
        boundary: FakeReadAuthenticationBoundary, lifecycleCenter: NotificationCenter
    ) throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-personal", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: QuietUnreachableSender(), readSession: ReadAuthenticationSession(boundary: boundary),
                notificationCenter: FakeNotificationCenter(), reachability: FakeReachability(reachable: false),
                lifecycleNotifications: lifecycleCenter),
            modules: [lifecycle])
        let services = AppServices(
            root: root, lifecycle: lifecycle,
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: lifecycleCenter)
        created.append(services)
        return services
    }

    func testAnActiveLaunchMakesOneQuietAttemptAndTheLauncherForegroundAddsNone() throws {
        let boundary = FakeReadAuthenticationBoundary()
        boundary.nextOutcome = .unavailable
        let center = NotificationCenter()
        let launcher = AppLauncher(
            makeServices: { [unowned self] in try self.makeQuietServices(boundary: boundary, lifecycleCenter: center) },
            isApplicationActive: { true })

        launcher.launch()

        XCTAssertEqual(boundary.quietCallCount, 1, "a synchronous unavailable outcome is not retried by the foreground")
        XCTAssertEqual(launcher.services?.root.readScope, .captureOnly)
        XCTAssertEqual(boundary.authenticateCallCount, 0)
    }

    func testAnInactiveLaunchWaitsForActivationAndSuccessAuthorizesReads() async throws {
        let boundary = FakeReadAuthenticationBoundary()
        let center = NotificationCenter()
        let services = try makeQuietServices(boundary: boundary, lifecycleCenter: center)
        services.start(isActive: false)
        XCTAssertEqual(boundary.quietCallCount, 0)

        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)

        XCTAssertEqual(boundary.quietCallCount, 1)
        XCTAssertEqual(services.root.readScope, .authenticated)
        let refreshed = await timeChangeWaitUntil { services.captureEntry.access.readAuthorized }
        XCTAssertTrue(refreshed, "the capture surface learns of the success")
    }

    func testShuttingDownRightAfterAuthenticatingLeavesNoHomeReadBehind() async throws {
        let boundary = FakeReadAuthenticationBoundary()
        let center = NotificationCenter()
        let services = try makeQuietServices(boundary: boundary, lifecycleCenter: center)
        services.start(isActive: false)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        XCTAssertEqual(services.root.readScope, .authenticated)

        services.shutDown()
        let freed = await timeChangeWaitUntil { CoreHandle.liveResultBufferCount == baselineBuffers }

        XCTAssertTrue(freed, "no home read outlives the shutdown")
        XCTAssertTrue(services.homeRecents.entries.isEmpty)
    }

    func testEachForegroundAfterABackgroundMakesOneAttemptAndTheRelockDoesNotCancelIt() throws {
        let boundary = FakeReadAuthenticationBoundary()
        boundary.nextOutcome = .denied
        let center = NotificationCenter()
        let services = try makeQuietServices(boundary: boundary, lifecycleCenter: center)
        services.start(isActive: true)
        XCTAssertEqual(boundary.quietCallCount, 1)

        center.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        boundary.defersCompletion = true
        center.post(name: UIApplication.willEnterForegroundNotification, object: nil)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)

        XCTAssertEqual(boundary.quietCallCount, 2, "one more attempt, and no second prompt while it is showing")
        XCTAssertEqual(boundary.invalidateCallCount, 0, "the foreground relock ran before the attempt began")
        boundary.completeDeferredAttempt(at: 0, with: .succeeded)
        XCTAssertEqual(services.root.readScope, .authenticated)
    }

    func testFailureAndUnavailabilityLeaveCaptureOnlyAndAreNotRetriedUntilTheNextForeground() throws {
        for outcome: ReadAuthenticationOutcome in [.denied, .unavailable, .cancelled] {
            let boundary = FakeReadAuthenticationBoundary()
            boundary.nextOutcome = outcome
            let center = NotificationCenter()
            let services = try makeQuietServices(boundary: boundary, lifecycleCenter: center)
            services.start(isActive: true)
            center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
            XCTAssertEqual(boundary.quietCallCount, 1, "\(outcome)")
            XCTAssertEqual(services.root.readScope, .captureOnly, "\(outcome)")
            XCTAssertNil(services.captureEntry.acknowledgment, "no error is shown for \(outcome)")
            services.shutDown()
            created.removeAll { $0 === services }
        }
    }

    func testTextCaptureSavesWhileAQuietAttemptIsStillShowing() async throws {
        let boundary = FakeReadAuthenticationBoundary()
        boundary.defersCompletion = true
        let services = try makeQuietServices(boundary: boundary, lifecycleCenter: NotificationCenter())
        services.start(isActive: true)
        XCTAssertEqual(boundary.quietCallCount, 1)
        XCTAssertEqual(boundary.deferredCompletions.count, 1, "the attempt is held open")

        services.captureEntry.textModel.updateText("remind me to call the roofer")
        services.captureEntry.saveText()

        let finished = await timeChangeWaitUntil {
            guard let save = services.captureEntry.acknowledgment?.save else { return false }
            return save != .saving
        }
        XCTAssertTrue(finished)
        guard case .saved = services.captureEntry.acknowledgment?.save else {
            return XCTFail("the capture is saved: \(String(describing: services.captureEntry.acknowledgment))")
        }
        XCTAssertEqual(services.root.readScope, .captureOnly)
        XCTAssertEqual(boundary.invalidateCallCount, 0)
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

private final class QuietUnreachableSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        throw URLError(.notConnectedToInternet)
    }
}
