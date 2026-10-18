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
    /// Turns the system's clock and time-zone callbacks into one `.systemTimeChange` lifecycle request per real change.
    let timeChange: TimeChangeCoordinator
    let captureEntry: CaptureEntryModel
    let navigation: ManagementNavigationModel
    let entrySession: CaptureEntrySession
    let voice: VoiceCaptureServices
    let managementContext: ManagementContext
    private let notificationHooks: NotificationHooks
    private let lifecycleCenter: NotificationCenter
    private var lifecycleTokens: [NSObjectProtocol] = []
    /// Whether this foreground stay has already made its one quiet attempt, so a launch that is also a foreground, or a
    /// repeated activation, never prompts twice even when an attempt finishes synchronously. Reset on background.
    private var quietAttemptSpent = false

    /// Builds the production root over the app's own storage, with the lifecycle coordinator attached.
    static func production(
        handoff: CaptureEntryHandoff = .live,
        storageRoot: URL? = nil
    ) throws -> AppServices {
        let resolvedStorageRoot = try storageRoot ?? AppConfiguration.productionStorageRoot()
        let lifecycle = LifecycleCoordinator()
        let retrieval = RetrievalSearchModule()
        let root = try ServiceCompositionRoot(
            configuration: AppConfiguration.production(storageRoot: resolvedStorageRoot),
            modules: [lifecycle, retrieval])
        return AppServices(root: root, lifecycle: lifecycle, handoff: handoff, retrieval: retrieval)
    }

    init(
        root: ServiceCompositionRoot,
        lifecycle: LifecycleCoordinator,
        handoff: CaptureEntryHandoff,
        voice: VoiceCaptureServices? = nil,
        retrieval: RetrievalSearching = RetrievalSearchModule(),
        notificationHooks: NotificationHooks = .live,
        notificationCenter: NotificationCenter = .default,
        timeSources: TimeSources = .system,
        isProtectedDataAvailable: @escaping () -> Bool = { UIApplication.shared.isProtectedDataAvailable }
    ) {
        self.root = root
        self.lifecycle = lifecycle
        self.notificationHooks = notificationHooks
        lifecycleCenter = notificationCenter
        timeChange = TimeChangeCoordinator(
            lifecycle: lifecycle, notificationCenter: notificationCenter, sources: timeSources)
        let voice = voice ?? VoiceCaptureServices(root: root)
        self.voice = voice

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
        let navigation = ManagementNavigationModel(
            readScope: { [weak root] in root?.readScope ?? .captureOnly },
            authenticate: { [weak root] reason, completion in
                guard let root else {
                    completion(.unavailable)
                    return
                }
                root.authenticateReads(reason: reason, completion: completion)
            },
            readAccessDidChange: { [weak captureEntry] in captureEntry?.refreshAccess() })
        self.navigation = navigation
        managementContext = ManagementContext(
            voiceRecovery: voice.recoveryModel,
            retrieval: .assembled(searcher: retrieval, root: root, navigation: navigation),
            retention: .assembled(root: root),
            providerSettings: .assembled(root: root),
            promptSettings: PromptSettingsModel(controller: root.promptSchedule),
            suggestion: SuggestionScreenModel(responder: root.suggestionResponses))
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
        root.setOpenSuggestionHandler { [weak self] event in
            DispatchQueue.main.async { self?.routePromptTap(event) }
        }
        notificationHooks.installDelegate(root.notificationCenterDelegate)
        root.start(isActive: isActive)
        timeChange.start()
        notificationHooks.reconcile(root)
        observeActivation()
        requestQuietAuthentication(isActive: isActive)
    }

    private func observeActivation() {
        guard lifecycleTokens.isEmpty else { return }
        lifecycleTokens = [
            lifecycleCenter.addObserver(
                forName: UIApplication.didBecomeActiveNotification, object: nil, queue: nil
            ) { [weak self] _ in self?.requestQuietAuthentication(isActive: true) },
            lifecycleCenter.addObserver(
                forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: nil
            ) { [weak self] _ in self?.quietAttemptSpent = false },
        ]
    }

    /// One quiet Face ID attempt per foreground stay, at launch when active and otherwise on `didBecomeActive`. That
    /// notification follows `willEnterForeground`, where the root relocks, so the relock cannot cancel the attempt. The
    /// attempt never blocks capture; a failure shows nothing and is not retried until the next foreground.
    private func requestQuietAuthentication(isActive: Bool) {
        guard isActive, !quietAttemptSpent else { return }
        quietAttemptSpent = true
        root.requestQuietReadAuthentication(reason: Self.quietAuthenticationReason) { [weak self] _ in
            self?.captureEntry.refreshAccess()
        }
    }

    private static let quietAuthenticationReason = "Show your recent captures."

    /// Presents the suggestion screen for a tapped prompt. The prompt identifier is kept first; the navigation model
    /// shows the screen only once the session is authenticated, and the screen reads nothing before that.
    func routePromptTap(_ event: PromptNotificationEvent) {
        managementContext.suggestion.receive(promptID: event.identifier.rawValue)
        navigation.openSuggestion()
    }

    func shutDown() {
        lifecycleTokens.forEach { lifecycleCenter.removeObserver($0) }
        lifecycleTokens = []
        timeChange.stop()
        root.shutDown()
    }

    /// Returns once none of the work that `shutDown()` cancelled is still running: lifecycle steps and reminder passes
    /// that ignore cancellation. After it, nothing calls into the core.
    func waitForShutDownWork() async {
        await lifecycle.waitForRunningActions()
        await root.waitForReminderPasses()
    }

    // MARK: Scene events

    func sceneWillEnterForeground() {
        // Callbacks do not arrive while the app is suspended, so compare the clocks on return.
        timeChange.checkNow()
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
