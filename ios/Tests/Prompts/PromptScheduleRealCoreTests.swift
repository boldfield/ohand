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

        let off = try await root.promptSchedule.configure(.off)
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

    // MARK: Helpers

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
