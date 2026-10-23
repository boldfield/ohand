import SwiftUI
import UIKit
import XCTest
import OhAndCoreBridge
@testable import OhAndServices
@testable import OhAnd

/// Hosts the real settings root over fresh models and reads what each row shows without any other screen having been
/// opened first.
@MainActor
final class SettingsRootViewTests: XCTestCase {
    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var services: AppServices!
    private var window: UIWindow?
    private var rendered: [String: String] = [:]

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("settings-root-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            notificationHooks: NotificationHooks(installDelegate: { _ in }, reconcile: { _ in }),
            notificationCenter: NotificationCenter(),
            isProtectedDataAvailable: { true })
        rendered = [:]
    }

    override func tearDownWithError() throws {
        window?.isHidden = true
        window = nil
        services.shutDown()
        services = nil
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    private func host(
        resetSettings: ResetSettingsModel? = nil, opensResetOnAppear: Bool = false,
        resetRenderReporter: (([String: RetentionRenderedElement]) -> Void)? = nil
    ) {
        let current = services.managementContext
        let status = RetentionStatus(
            policy: RetentionPolicySetting(periodSeconds: 7 * 86_400, isDefault: true),
            storagePressure: .normal, unresolved: [])
        let retention = RetentionSettingsModel(
            controls: FakeRetentionControls(status: status), readScope: { .authenticated },
            exportSettings: current.retention.exportSettings, resetSettings: resetSettings)
        let providers = ProviderSettingsModel(
            controls: FakeProviderSettingsControls(listing: providerListing(versions: [], defaultVersion: nil)),
            credentials: FakeProviderSettingsCredentials(), sweep: FakeProviderSettingsSweep(),
            readScope: { .authenticated })
        let context = ManagementContext(
            voiceRecovery: current.voiceRecovery, retrieval: current.retrieval, retention: retention,
            providerSettings: providers, promptSettings: current.promptSettings,
            sourceDetail: current.sourceDetail, suggestion: current.suggestion,
            privacySettings: current.privacySettings)
        let root = NavigationStack {
            SettingsRootView(
                sections: ManagementCatalog.settingsSections, context: context,
                renderReporter: { [unowned self] in rendered = $0 },
                opensResetOnAppear: opensResetOnAppear, resetRenderReporter: resetRenderReporter)
        }
        let shown = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        shown.rootViewController = UIHostingController(rootView: root)
        shown.makeKeyAndVisible()
        window = shown
    }

    private func settle(until condition: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 10)
        repeat {
            window?.rootViewController?.view.layoutIfNeeded()
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
        } while !condition() && Date() < deadline
    }

    func testAFreshRootShowsEveryCurrentValueWithoutOpeningAnotherScreen() {
        host()
        settle { rendered["prompt-settings"] == "Daily prompt: Off" && rendered["audio-retention"] != nil
            && rendered["provider-settings"]?.hasSuffix(": On this phone") == true
            && rendered["voice-recovery"]?.hasSuffix(": None") == true }

        XCTAssertEqual(rendered["prompt-settings"], "Daily prompt: Off")
        XCTAssertEqual(rendered["voice-recovery"], "Unfinished recordings: None")
        XCTAssertEqual(rendered["provider-settings"], "Processing: On this phone")
        XCTAssertEqual(rendered["audio-retention"], "Audio: 7 days")
        XCTAssertEqual(rendered[SettingsRootView.exportEntryID], "Export")
    }

    func testEraseCardIsShownOnlyWhenAResetModelIsPresent() {
        host()
        settle { rendered["audio-retention"] != nil }
        XCTAssertNil(rendered[SettingsRootView.eraseIdentifier])

        window?.isHidden = true
        host(resetSettings: ResetSettingsModel(controls: FakeReset(), readScope: { .authenticated }))
        settle { rendered[SettingsRootView.eraseIdentifier] != nil }
        XCTAssertEqual(rendered[SettingsRootView.eraseIdentifier], "Erase everything")
    }

    /// The card's tap sets the same presentation state the root opens on appear; the real tap is covered end to end by
    /// the walkthrough UI test, which taps `management.settings.erase` and waits for the reset screen.
    func testTheEraseCardDestinationIsTheResetScreenOverTheContextsModel() {
        var resetRendered: [String: RetentionRenderedElement] = [:]
        host(resetSettings: ResetSettingsModel(controls: FakeReset(), readScope: { .authenticated }),
             opensResetOnAppear: true, resetRenderReporter: { resetRendered = $0 })
        settle { resetRendered[ResetSettingsView.explanationIdentifier] != nil }
        XCTAssertEqual(resetRendered[ResetSettingsView.explanationIdentifier]?.text, DeletionText.resetExplanation)
    }

    func testRowsFollowTheDesignOrderRegardlessOfCatalogOrder() {
        let ids = { (entries: [ManagementEntry]) in entries.map(\.id) }
        let catalog = ManagementCatalog.settingsSections
        XCTAssertEqual(ids(SettingsRootView.ordered(SettingsRootView.firstGroupEntryIDs, from: catalog)),
                       ["prompt-settings", "voice-recovery"])
        XCTAssertEqual(ids(SettingsRootView.ordered(SettingsRootView.secondGroupEntryIDs, from: catalog)),
                       ["provider-settings", "privacy-settings", "permissions-settings", "status-settings", "audio-retention"])
        XCTAssertEqual(SettingsRootView.secondGroupEntryIDs.last, SettingsRootView.exportEntryID)
    }

    /// The privacy screen is reachable from the app shell: the settings root shows its row, and the catalog entry opens
    /// the screen over the model `AppServices` assembled, which reads through the root's read gate (locked here, since
    /// the session is not authenticated).
    func testPrivacyRowOpensTheAssembledPrivacyScreen() throws {
        host()
        settle { rendered["privacy-settings"] != nil }
        XCTAssertEqual(rendered["privacy-settings"], "Privacy")

        let model = services.managementContext.privacySettings
        XCTAssertEqual(model.phase, .idle)
        let entry = try XCTUnwrap(ManagementCatalog.settingsSections.first { $0.id == "privacy-settings" })
        let shown = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
        shown.rootViewController = UIHostingController(rootView: NavigationStack { entry.makeView(services.managementContext) })
        shown.makeKeyAndVisible()
        window?.isHidden = true
        window = shown
        settle { model.phase == .locked }
        XCTAssertEqual(model.phase, .locked)
        XCTAssertTrue(model.routes.isEmpty)
    }

    /// The status screen is reachable from the app shell: the settings root lists its row, and `AppServices` assembles
    /// the model the catalog entry presents.
    func testStatusRowIsListedAndTheContextCarriesTheAssembledStatusModel() throws {
        host()
        settle { rendered["status-settings"] != nil }
        XCTAssertEqual(rendered["status-settings"], "Status")
        XCTAssertNotNil(services.managementContext.statusModel)
        XCTAssertNotNil(ManagementCatalog.settingsSections.first { $0.id == "status-settings" })
    }
}
