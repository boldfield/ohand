import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

/// A provider transport for tests that never expect a provider request.
private final class PromptTestProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("a prompt pass sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}

/// A reminder pass that reads the persisted schedule when it starts, can be held before it finishes, and then keeps what
/// it read as the installed state, the way a pass that began before a later choice would.
private final class ScheduleMirroringReconciler: ReminderReconciling, @unchecked Sendable {
    let gate = SuspensionGate()
    var reportsConverged: Bool {
        get { lock.lock(); defer { lock.unlock() }; return storedReportsConverged }
        set { lock.lock(); defer { lock.unlock() }; storedReportsConverged = newValue }
    }
    var installedScheduleIsEnabled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return installedEnabled
    }

    private let core: CorePromptScheduleClient
    private let lock = NSLock()
    private var gatesNextPass = false
    private var installedEnabled = false
    private var storedReportsConverged = true

    init(core: CorePromptScheduleClient) {
        self.core = core
    }

    func gateNextPass() {
        lock.lock()
        gatesNextPass = true
        lock.unlock()
    }

    func reconcile(_ request: ReminderReconciliationRequest) async throws -> ReminderReconciliationOutcome {
        let enabledWhenThePassBegan = try await core.read().enabled
        lock.lock()
        let held = gatesNextPass
        gatesNextPass = false
        lock.unlock()
        if held { await gate.wait() }
        lock.lock()
        installedEnabled = enabledWhenThePassBegan
        let converged = storedReportsConverged
        lock.unlock()
        return ReminderReconciliationOutcome(
            reminders: [], promptsGranted: 0, promptsDeferred: 0, converged: converged)
    }

    func storedFacts() async throws -> [ReminderFacts] { [] }
}

/// Drives the daily prompt schedule through a production-shaped root: the real Rust core, the real lifecycle
/// coordinator and reminder pass, and a fake only at the operating system's edges (the notification centre and the
/// provider transport).
final class PromptScheduleRealCoreTests: XCTestCase {
    private static let routeID = "route-prompts"
    private static let timeZone = TimeZone(identifier: "UTC")!

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("prompts-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    // MARK: Tests

    func testPromptsStayOffUntilConfigured() async throws {
        let center = FakeNotificationCenter()
        let (root, coordinator) = try makeStartedRoot(center: center)
        defer { root.shutDown() }

        let launch = await coordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        let foreground = await coordinator.reconcile(.foreground)
        XCTAssertEqual(foreground.outcome(of: .promptEligibility), .completed)

        XCTAssertEqual(promptIdentifiers(center.addedRequests), [], "nothing is installed before the user chooses a time")
        let state = try await root.promptSchedule.read()
        XCTAssertFalse(state.enabled)
        XCTAssertNil(state.time)
        XCTAssertNil(state.next)
    }

    func testConfigureThenLaunchInstallsOnePromptPerEligibleLocalDate() async throws {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .denied
        let (root, coordinator) = try makeStartedRoot(center: center)
        defer { root.shutDown() }

        let configured = try await root.promptSchedule.configure(.daily(hour: 9, minute: 0))
        XCTAssertTrue(configured.enabled)
        XCTAssertEqual(configured.time, PromptScheduleOutcome.Time(hour: 9, minute: 0))
        let next = try XCTUnwrap(configured.next)
        XCTAssertEqual(promptIdentifiers(center.addedRequests), [], "a denied permission withholds every prompt")

        let withheld = await coordinator.reconcile(.launch)
        XCTAssertEqual(withheld.outcome(of: .promptEligibility), .deferred("prompts_not_permitted"))
        XCTAssertEqual(promptIdentifiers(center.addedRequests), [])

        center.authorizationStatus = .authorized
        let launch = await coordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)

        let installed = promptIdentifiers(center.addedRequests)
        XCTAssertEqual(Set(installed).count, installed.count, "each local date is requested once")
        XCTAssertEqual(installed.count, 3, "the planning horizon is three occurrences")
        XCTAssertEqual(installed.sorted().first, "prompt:\(next.localDate)", "the first is the next eligible local date")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), installed.sorted())
        for request in center.addedRequests where request.identifier.hasPrefix("prompt:") {
            XCTAssertEqual(request.content.title, PromptNotificationWording.title)
            XCTAssertEqual(request.content.body, PromptNotificationWording.body)
        }
    }

    func testAForegroundPassDoesNotDuplicateTheInstalledPrompts() async throws {
        let center = FakeNotificationCenter()
        let (root, coordinator) = try makeStartedRoot(center: center)
        defer { root.shutDown() }
        _ = try await root.promptSchedule.configure(.daily(hour: 9, minute: 0))

        let launch = await coordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        let installed = promptIdentifiers(center.addedRequests)
        XCTAssertEqual(installed.count, 3)

        let foreground = await coordinator.reconcile(.foreground)
        XCTAssertEqual(foreground.outcome(of: .promptEligibility), .completed)
        let timeChange = await coordinator.reconcile(.systemTimeChange)
        XCTAssertEqual(timeChange.outcome(of: .promptEligibility), .completed)

        XCTAssertEqual(promptIdentifiers(center.addedRequests), installed, "no prompt was requested a second time")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), installed.sorted())
        XCTAssertEqual(center.removedIdentifiers, [], "nothing was cancelled")
    }

    func testTurningPromptsOffCancelsTheInstalledPrompts() async throws {
        let center = FakeNotificationCenter()
        let (root, coordinator) = try makeStartedRoot(center: center)
        defer { root.shutDown() }
        _ = try await root.promptSchedule.configure(.daily(hour: 9, minute: 0))
        _ = await coordinator.reconcile(.launch)
        let installed = promptIdentifiers(center.addedRequests)
        XCTAssertEqual(installed.count, 3)

        let reportBeforeOff = try XCTUnwrap(root.lastReminderReport)
        let off = try await root.promptSchedule.configure(.off)
        let reportOfOff = try XCTUnwrap(root.lastReminderReport)
        XCTAssertNotEqual(reportOfOff, reportBeforeOff, "the root records the pass that cancelled the prompts")
        XCTAssertEqual(reportOfOff.trigger, .explicit)
        XCTAssertFalse(off.enabled)
        XCTAssertNil(off.next)
        XCTAssertEqual(center.removedIdentifiers.sorted(), installed.sorted(), "every installed prompt was cancelled")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), [])

        let foreground = await coordinator.reconcile(.foreground)
        XCTAssertEqual(foreground.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(promptIdentifiers(center.addedRequests), installed, "a later pass installs nothing while off")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), [])
    }

    func testARelaunchKeepsTheLedgerAndRequestsNoHeldDateAgain() async throws {
        let center = FakeNotificationCenter()
        let (first, firstCoordinator) = try makeStartedRoot(center: center)
        defer { first.shutDown() }
        _ = try await first.promptSchedule.configure(.daily(hour: 9, minute: 0))
        _ = await firstCoordinator.reconcile(.launch)
        let installed = promptIdentifiers(center.addedRequests)
        XCTAssertEqual(installed.count, 3)
        let before = try await first.promptSchedule.read()
        first.shutDown()

        let (second, secondCoordinator) = try makeStartedRoot(center: center)
        defer { second.shutDown() }
        let after = try await second.promptSchedule.read()
        XCTAssertTrue(after.enabled, "the schedule survived the relaunch")
        XCTAssertEqual(after.time, before.time)
        XCTAssertEqual(after.next, before.next)

        let launch = await secondCoordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(promptIdentifiers(center.addedRequests), installed, "no second prompt for a held date")
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), installed.sorted())
        XCTAssertEqual(center.removedIdentifiers, [])
    }

    func testAProvisionalPermissionFollowsTheExistingRulesAndInstallsQuietly() async throws {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .provisional
        let (root, coordinator) = try makeStartedRoot(center: center)
        defer { root.shutDown() }
        _ = try await root.promptSchedule.configure(.daily(hour: 9, minute: 0))

        let launch = await coordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(promptIdentifiers(center.addedRequests).count, 3)
    }

    func testTheTimeChangeEntryRefreshesWithoutChangingTheChoice() async throws {
        let center = FakeNotificationCenter()
        let (root, _) = try makeStartedRoot(center: center)
        defer { root.shutDown() }
        let configured = try await root.promptSchedule.configure(.daily(hour: 7, minute: 30))

        let installed = promptIdentifiers(center.addedRequests)
        XCTAssertEqual(installed.count, 3)

        let refreshed = try await root.promptSchedule.timeChanged()
        XCTAssertTrue(refreshed.enabled)
        XCTAssertEqual(refreshed.time, configured.time)
        XCTAssertEqual(promptIdentifiers(center.addedRequests), installed, "the entry itself installs nothing more")
    }

    func testConfigureAfterJoiningAStalePassStillAppliesTheNewChoice() async throws {
        let (core, client) = try openCore()
        defer { client.shutDown(); core.close() }
        let reconciliation = ScheduleMirroringReconciler(core: client)
        let reconciler = NotificationPermissionReconciler(
            center: FakeNotificationCenter(), reconciliation: reconciliation,
            observations: InMemoryNotificationPermissionObservationStore())
        let service = PromptScheduleService(
            core: client, timeZone: { Self.timeZone }, install: { try await reconciler.refresh(trigger: $0) })

        _ = try await service.configure(.daily(hour: 9, minute: 0))
        XCTAssertTrue(reconciliation.installedScheduleIsEnabled)

        reconciliation.gateNextPass()
        let stalePass = Task { try await reconciler.refresh(trigger: .foreground) }
        try await waitUntil("the stale pass has read the enabled schedule") { reconciliation.gate.arrivals >= 1 }

        let turningOff = Task { try await service.configure(.off) }
        try await waitUntil("the choice to turn prompts off is persisted") {
            let persisted = try await client.read()
            return !persisted.enabled
        }
        reconciliation.gate.release()

        _ = try await stalePass.value
        let off = try await turningOff.value
        XCTAssertFalse(off.enabled)
        XCTAssertFalse(
            reconciliation.installedScheduleIsEnabled,
            "configure ran a pass that began after the choice was persisted, not only the stale one it joined")
    }

    func testConfigureReportsAPassThatDidNotConverge() async throws {
        let (core, client) = try openCore()
        defer { client.shutDown(); core.close() }
        let reconciliation = ScheduleMirroringReconciler(core: client)
        reconciliation.reportsConverged = false
        let reconciler = NotificationPermissionReconciler(
            center: FakeNotificationCenter(), reconciliation: reconciliation,
            observations: InMemoryNotificationPermissionObservationStore())
        let service = PromptScheduleService(
            core: client, timeZone: { Self.timeZone }, install: { try await reconciler.refresh(trigger: $0) })

        do {
            _ = try await service.configure(.daily(hour: 9, minute: 0))
            XCTFail("configure reported success although its pass did not converge")
        } catch PromptScheduleConfigurationError.installIncomplete(let outcome, let report) {
            XCTAssertTrue(outcome.enabled, "the choice itself was persisted")
            XCTAssertFalse(report.converged)
        }
        let persisted = try await client.read()
        XCTAssertTrue(persisted.enabled)
    }

    // MARK: Helpers

    private func openCore() throws -> (CoreHandle, CorePromptScheduleClient) {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        XCTAssertTrue(ProtectedStorageService(layout: layout).prepare().isFullyConfigured)
        let core = try CoreHandle(path: layout.databaseURL.path)
        let client = CorePromptScheduleClient(core: core)
        try core.setEventHandler { _ = client.handle($0) }
        return (core, client)
    }

    private func waitUntil(
        _ description: String, timeoutSeconds: TimeInterval = 20, _ condition: () async throws -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeoutSeconds)
        while true {
            if try await condition() { return }
            if Date() > deadline {
                XCTFail("timed out waiting for \(description)")
                throw CancellationError()
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }

    private func promptIdentifiers(_ identifiers: [String]) -> [String] {
        identifiers.filter { $0.hasPrefix("prompt:") }
    }

    private func promptIdentifiers(_ requests: [NotificationCenterRequest]) -> [String] {
        promptIdentifiers(requests.map(\.identifier))
    }

    private func makeStartedRoot(
        center: FakeNotificationCenter
    ) throws -> (ServiceCompositionRoot, LifecycleCoordinator) {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { Self.timeZone }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: PromptTestProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true), lifecycleNotifications: NotificationCenter()),
            modules: [coordinator])
        root.start(isActive: false)
        return (root, coordinator)
    }
}
