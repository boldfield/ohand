import XCTest
import OhAndCoreBridge
@testable import OhAndServices
import UserNotifications
@testable import OhAnd

/// The app's production wiring over a fresh store, with only the microphone, the audio engine and the system
/// notification center replaced. Everything else (the core, ingress, the recording save path, recovery) is real.
final class AppRealServicesTests: XCTestCase {
    private final class RecordingHooks {
        private(set) var events: [String] = []
        private(set) var installedDelegate: UNUserNotificationCenterDelegate?

        var hooks: NotificationHooks {
            NotificationHooks(
                installDelegate: { [unowned self] delegate in
                    events.append("install")
                    installedDelegate = delegate
                },
                reconcile: { [unowned self] _ in events.append("reconcile") })
        }
    }

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0
    private var created: [AppServices] = []
    private let engine = SyntheticVoiceEngine()
    private let permission = FakeMicrophonePermission()
    private let poller = ManualPoller()
    private let hooks = RecordingHooks()

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("app-real-services-\(UUID().uuidString)", isDirectory: true)
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

    private func makeServices() throws -> AppServices {
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: storageRoot), modules: [lifecycle])
        var environment = VoiceRecordingEnvironment.live(protectRecordingFile: { _ in })
        environment.isForeground = { true }
        let manualPoller = poller
        environment.schedulePoll = { interval, handler in manualPoller.schedule(interval: interval, handler: handler) }
        let voice = VoiceCaptureServices(
            root: root, engine: engine, permission: permission, environment: environment,
            notificationCenter: NotificationCenter())
        let services = AppServices(
            root: root, lifecycle: lifecycle, handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()),
            voice: voice, notificationHooks: hooks.hooks, notificationCenter: NotificationCenter(),
            isProtectedDataAvailable: { true })
        created.append(services)
        return services
    }

    private func wait(timeout: TimeInterval = 30, until condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
    }

    // MARK: saving through the real core

    func testATextCaptureOnAFreshStoreIsSavedByTheRealCore() throws {
        let services = try AppServices.production(
            handoff: CaptureEntryHandoff(notificationCenter: NotificationCenter()), storageRoot: storageRoot)
        created.append(services)

        services.captureEntry.textModel.updateText("remind me to water the plants")
        services.captureEntry.saveText()
        wait { services.captureEntry.acknowledgment?.savedItemID != nil }

        let itemID = try XCTUnwrap(services.captureEntry.acknowledgment?.savedItemID, "the core confirmed the import")
        XCTAssertFalse(itemID.isEmpty)
        XCTAssertEqual(services.captureEntry.textModel.text, "", "the field clears only after a confirmed save")
    }

    func testARecordingIsSavedByTheRealCoreThroughTheRootHandoff() throws {
        let services = try makeServices()

        services.captureEntry.startRecording()
        wait { if case .recording = services.captureEntry.recorderState { return true } else { return false } }
        guard case .recording = services.captureEntry.recorderState else {
            return XCTFail("the recorder did not start: \(services.captureEntry.recorderState)")
        }
        XCTAssertEqual(engine.startCount, 1, "the real recorder, not an unavailable stand-in, drove the engine")

        services.captureEntry.stopRecording()
        wait { services.captureEntry.acknowledgment?.savedItemID != nil }

        let acknowledgment = try XCTUnwrap(services.captureEntry.acknowledgment)
        XCTAssertNotNil(acknowledgment.savedItemID, "saved only on the core's confirmation: \(acknowledgment.save)")
        XCTAssertEqual(acknowledgment.source, .audio)
    }

    func testACancelledRecordingStaysReachableThroughTheRecoveryModel() throws {
        let services = try makeServices()

        services.captureEntry.startRecording()
        wait { if case .recording = services.captureEntry.recorderState { return true } else { return false } }
        services.captureEntry.cancelRecording()
        wait { services.captureEntry.acknowledgment?.save == .cancelled }
        XCTAssertEqual(services.captureEntry.acknowledgment?.save, .cancelled)

        services.voice.recoveryModel.refresh()
        wait { !services.voice.recoveryModel.rows.isEmpty }

        XCTAssertEqual(services.voice.recoveryModel.rows.count, 1, "the cancelled audio is offered, never silently dropped")
    }

    // MARK: notification wiring

    func testStartInstallsTheDelegateBeforeTheRootStartsAndThenReconciles() throws {
        let services = try makeServices()
        XCTAssertEqual(hooks.events, [], "building the services touches no system object")

        services.start(isActive: false)

        XCTAssertEqual(hooks.events, ["install", "reconcile"])
        XCTAssertTrue(
            hooks.installedDelegate === services.root.notificationCenterDelegate,
            "the root's own delegate is the one installed")
    }

    func testALockedSessionDropsARequestedItem() throws {
        let services = try makeServices()
        services.navigation.path = [ManagementRoute.item("item-1").pathComponent]
        XCTAssertEqual(services.root.readScope, .captureOnly)

        services.navigation.revalidate()

        XCTAssertNil(services.navigation.requestedItemID, "a locked session keeps no item open")
    }
}
