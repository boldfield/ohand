import Foundation
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

private final class SettingsTestProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("a prompt settings change sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}

/// Drives the settings model over the real prompt service, the real core and the production-shaped root, faking only
/// the notification centre and the provider transport.
@MainActor
final class PromptSettingsRealCoreTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("prompt-settings-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func promptIdentifiers(_ identifiers: [String]) -> [String] {
        identifiers.filter { $0.hasPrefix("prompt:") }
    }

    private func makeRoot(center: FakeNotificationCenter) throws -> ServiceCompositionRoot {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        let root = try ServiceCompositionRoot(
            testing: CompositionConfiguration(
                storageRootPath: storageRoot.path, captureRouteID: "route-prompt-settings", jobRetryDelaySeconds: 3600),
            environment: CompositionEnvironment(
                timeZone: { TimeZone(identifier: "UTC")! }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: SettingsTestProviderSender(), readSession: session, notificationCenter: center,
                reachability: FakeReachability(reachable: true), lifecycleNotifications: NotificationCenter()),
            modules: [coordinator])
        root.start(isActive: false)
        return root
    }

    func testDefaultOffThenOnInstallsBoundedPromptsAndOffCancelsThem() async throws {
        let center = FakeNotificationCenter()
        let root = try makeRoot(center: center)
        defer { root.shutDown() }
        let model = PromptSettingsModel(controller: root.promptSchedule)

        await model.load()
        XCTAssertFalse(model.isEnabled, "default is off")
        XCTAssertEqual(promptIdentifiers(center.addedRequests.map(\.identifier)), [])

        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled)
        XCTAssertEqual(model.notice, .saved)
        XCTAssertLessThanOrEqual(
            promptIdentifiers(center.pendingIdentifiers).count, PromptSettingsModel.horizonDays,
            "no catch-up burst beyond the horizon")

        await model.setEnabled(false)
        XCTAssertFalse(model.isEnabled)
        XCTAssertEqual(promptIdentifiers(center.pendingIdentifiers), [], "turning prompts off cancels what was installed")
        let relaunched = PromptSettingsModel(controller: root.promptSchedule)
        await relaunched.load()
        XCTAssertFalse(relaunched.isEnabled, "the cancellation persists")
    }

    func testADeniedPermissionSavesTheChoiceAndInstallsNothing() async throws {
        let center = FakeNotificationCenter()
        center.authorizationStatus = .denied
        let root = try makeRoot(center: center)
        defer { root.shutDown() }
        let model = PromptSettingsModel(controller: root.promptSchedule)
        await model.load()
        await model.setEnabled(true)
        XCTAssertTrue(model.isEnabled, "the choice persists")
        XCTAssertEqual(promptIdentifiers(center.addedRequests.map(\.identifier)), [], "nothing is installed")
        for request in center.addedRequests {
            XCTAssertFalse(request.identifier.hasPrefix("prompt:"))
        }
    }
}
