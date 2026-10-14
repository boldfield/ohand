import Combine
import Foundation
import OhAndServices
import UIKit
import UserNotifications

/// How the app attaches the root's notification handling to the system: the delegate to install on the notification
/// center, and the pass that replays callbacks stored by an earlier launch. Tests record both instead of touching the
/// system.
struct NotificationHooks {
    var installDelegate: (UNUserNotificationCenterDelegate) -> Void
    var reconcile: (ServiceCompositionRoot) -> Void

    static let live = NotificationHooks(
        installDelegate: { UNUserNotificationCenter.current().delegate = $0 },
        reconcile: { root in Task { await root.reconcileNotificationActions() } })
}

/// The app's resolved production services and the models built on them. It owns the composition root and its
/// lifecycle coordinator, and gives the capture surface and the management navigation their seams over the root. It
/// does not rebuild the root's wiring: launch and foreground reconciliation (ingress recovery, the job runner,
/// deletion, reminders, retention, prompt eligibility, health) runs inside the `LifecycleCoordinator` module the root
/// starts.
///
/// Confined to the main queue.
final class AppServices {
    let root: ServiceCompositionRoot
    let lifecycle: LifecycleCoordinator
    let captureEntry: CaptureEntryModel
    let navigation: ManagementNavigationModel
    let entrySession: CaptureEntrySession
    let voice: VoiceCaptureServices
    let managementContext: ManagementContext
    private let notificationHooks: NotificationHooks

    /// Builds the production root over the app's own storage, with the lifecycle coordinator attached.
    static func production(
        handoff: CaptureEntryHandoff = .live,
        storageRoot: URL? = nil
    ) throws -> AppServices {
        let resolvedStorageRoot = try storageRoot ?? AppConfiguration.productionStorageRoot()
        let lifecycle = LifecycleCoordinator()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: resolvedStorageRoot), modules: [lifecycle])
        return AppServices(root: root, lifecycle: lifecycle, handoff: handoff)
    }

    init(
        root: ServiceCompositionRoot,
        lifecycle: LifecycleCoordinator,
        handoff: CaptureEntryHandoff,
        voice: VoiceCaptureServices? = nil,
        notificationHooks: NotificationHooks = .live,
        notificationCenter: NotificationCenter = .default,
        isProtectedDataAvailable: @escaping () -> Bool = { UIApplication.shared.isProtectedDataAvailable }
    ) {
        self.root = root
        self.lifecycle = lifecycle
        self.notificationHooks = notificationHooks
        let voice = voice ?? VoiceCaptureServices(root: root)
        self.voice = voice
        managementContext = ManagementContext(voiceRecovery: voice.recoveryModel)

        let access: () -> CaptureEntryAccess = { [weak root] in
            CaptureEntryAccess(
                protectedDataAvailable: isProtectedDataAvailable(),
                readAuthorized: (root?.readScope ?? .captureOnly) == .authenticated)
        }
        let defaultMode: () -> CaptureEntryMode = { .silent }
        let captureEntry = CaptureEntryModel(
            textIngress: RootTextCaptureIngress(root: root),
            voice: voice.recorder,
            progressReader: RootCaptureProgressReader(root: root),
            defaultMode: defaultMode(),
            access: access)
        self.captureEntry = captureEntry
        navigation = ManagementNavigationModel(
            readScope: { [weak root] in root?.readScope ?? .captureOnly },
            authenticate: { [weak root] reason, completion in
                guard let root else {
                    completion(.unavailable)
                    return
                }
                root.authenticateReads(reason: reason, completion: completion)
            },
            readAccessDidChange: { [weak captureEntry] in captureEntry?.refreshAccess() })
        entrySession = CaptureEntrySession(
            handoff: handoff, defaultMode: defaultMode, access: access, notificationCenter: notificationCenter)
        entrySession.observeHandoffs { [weak self] presentation in self?.present(presentation) }
    }

    /// Installs the notification delegate and the open-item route before the root starts, so no delivered, opened or
    /// action callback is missed, then replays the callbacks an earlier launch stored.
    func start(isActive: Bool) {
        root.setOpenItemHandler { [weak self] itemID in
            DispatchQueue.main.async { self?.navigation.openItem(itemID: itemID) }
        }
        notificationHooks.installDelegate(root.notificationCenterDelegate)
        root.start(isActive: isActive)
        notificationHooks.reconcile(root)
    }

    func shutDown() {
        root.shutDown()
    }

    // MARK: Scene events

    func sceneWillEnterForeground() {
        if let presentation = entrySession.willEnterForeground() {
            present(presentation)
        }
    }

    func sceneDidEnterBackground() {
        entrySession.didEnterBackground()
    }

    func receive(url: URL) {
        if let presentation = entrySession.receive(url: url) {
            present(presentation)
        }
    }

    /// A capture entry always lands on the capture surface, never on a management screen.
    private func present(_ presentation: CaptureEntryPresentation) {
        navigation.returnToCapture()
        captureEntry.present(presentation)
    }
}

/// Builds the services once the process launches and keeps them for the process. A failure leaves `services` empty and
/// the scene showing a retry, never a capture surface that cannot save.
final class AppLauncher: ObservableObject {
    @Published private(set) var services: AppServices?

    private let makeServices: () throws -> AppServices
    private let startServices: (AppServices, Bool) -> Void
    private let isApplicationActive: () -> Bool

    init(
        makeServices: @escaping () throws -> AppServices = { try AppServices.production() },
        startServices: @escaping (AppServices, Bool) -> Void = { services, isActive in
            services.start(isActive: isActive)
        },
        isApplicationActive: @escaping () -> Bool = { UIApplication.shared.applicationState == .active }
    ) {
        self.makeServices = makeServices
        self.startServices = startServices
        self.isApplicationActive = isApplicationActive
    }

    /// Builds and starts the services if they do not exist yet. Call on the main queue.
    func launch() {
        launch(deliveringForeground: false)
    }

    /// The scene is entering the foreground. With services this forwards the event; without them (an earlier build
    /// failed, for example because the process started before the first unlock) it tries to build them again.
    func sceneWillEnterForeground() {
        if let services {
            services.sceneWillEnterForeground()
        } else {
            launch(deliveringForeground: true)
        }
    }

    private func launch(deliveringForeground: Bool) {
        guard services == nil, let built = try? makeServices() else { return }
        services = built
        let isActive = isApplicationActive()
        startServices(built, isActive)
        // A build that came after the scene's foreground event missed that event.
        if isActive || deliveringForeground {
            built.sceneWillEnterForeground()
        }
    }
}
