import Combine
import Foundation
import OhAndServices
import UIKit

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
        notificationCenter: NotificationCenter = .default,
        isProtectedDataAvailable: @escaping () -> Bool = { UIApplication.shared.isProtectedDataAvailable }
    ) {
        self.root = root
        self.lifecycle = lifecycle

        let access: () -> CaptureEntryAccess = { [weak root] in
            CaptureEntryAccess(
                protectedDataAvailable: isProtectedDataAvailable(),
                readAuthorized: (root?.readScope ?? .captureOnly) == .authenticated)
        }
        // Voice capture has no save path in the root yet, so silent capture is the honest default.
        let defaultMode: () -> CaptureEntryMode = { .silent }
        let captureEntry = CaptureEntryModel(
            textIngress: RootTextCaptureIngress(root: root),
            voice: VoiceRecorderUnavailable(),
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

    func start(isActive: Bool) {
        root.start(isActive: isActive)
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
        guard services == nil, let built = try? makeServices() else { return }
        services = built
        let isActive = isApplicationActive()
        startServices(built, isActive)
        // A retry after the scene already came to the foreground missed that event.
        if isActive {
            built.sceneWillEnterForeground()
        }
    }
}
