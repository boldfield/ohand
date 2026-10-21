import UIKit

/// Covers (and uncovers) everything the app shows. Called on the main thread.
public protocol RedactionPresenter: AnyObject {
    func showRedaction()
    func hideRedaction()
}

/// Puts an opaque window above every connected scene so the system's app-switcher snapshot, taken as the app resigns
/// active, contains no stored content. The cover carries no text from the app. A simulator shows the cover but only a
/// device shows the real snapshot; the physical check is device evidence (device evidence task T05, `docs/validation/m1-device-results.md`).
public final class WindowRedactionPresenter: RedactionPresenter {
    private let windowScenes: () -> [UIWindowScene]
    private var coverWindows: [UIWindow] = []

    var coverWindowCount: Int { coverWindows.count }

    private let background: UIColor
    private let markImage: UIImage?

    /// Defaults to the shipped launch screen pair (`LaunchBackground`, `LaunchMark`), so the cover and the launch
    /// screen are the same brand surface in both themes.
    public convenience init(
        windowScenes: @escaping () -> [UIWindowScene] = {
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        }
    ) {
        self.init(
            windowScenes: windowScenes,
            background: UIColor(named: "LaunchBackground") ?? .systemBackground,
            markImage: UIImage(named: "LaunchMark"))
    }

    public init(windowScenes: @escaping () -> [UIWindowScene], background: UIColor, markImage: UIImage?) {
        self.windowScenes = windowScenes
        self.background = background
        self.markImage = markImage
    }

    /// The cover window's content view: the background with the mark centred. A fixed image, never stored history.
    func makeCoverView() -> UIView {
        let view = UIView()
        view.backgroundColor = background
        view.accessibilityIgnoresInvertColors = true
        if let markImage = markImage {
            let mark = UIImageView(image: markImage)
            mark.isAccessibilityElement = false
            mark.contentMode = .center
            mark.frame = view.bounds
            mark.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            view.addSubview(mark)
        }
        return view
    }

    var coverViews: [UIView] { coverWindows.compactMap { $0.rootViewController?.view } }

    public func showRedaction() {
        guard coverWindows.isEmpty else { return }
        for scene in windowScenes() {
            let window = UIWindow(windowScene: scene)
            window.windowLevel = UIWindow.Level.alert + 1
            let controller = UIViewController()
            controller.view = makeCoverView()
            window.rootViewController = controller
            window.isHidden = false
            coverWindows.append(window)
        }
    }

    public func hideRedaction() {
        for window in coverWindows {
            window.isHidden = true
            window.rootViewController = nil
        }
        coverWindows.removeAll()
    }
}

/// Connects app lifecycle notifications to the session and the redaction cover.
///
/// - Resigning active (app switcher, Control Center, the system authentication sheet) covers the screen. This does not
///   relock: the authentication sheet itself resigns active, and relocking then would make authentication impossible.
/// - Entering the background, returning to the foreground and protected data becoming unavailable relock. Relocking on
///   both background transitions means a missed notification on one side still relocks.
/// - Becoming active uncovers the screen. Access is whatever the session says, which is capture-only after any
///   background trip.
final class SessionLifecycleCoordinator {
    private let session: ReadAuthenticationSession
    private let presenter: RedactionPresenter
    private let notificationCenter: NotificationCenter
    private var observers: [NSObjectProtocol] = []

    init(
        session: ReadAuthenticationSession,
        presenter: RedactionPresenter,
        notificationCenter: NotificationCenter = .default
    ) {
        self.session = session
        self.presenter = presenter
        self.notificationCenter = notificationCenter
    }

    deinit { stop() }

    func start() {
        guard observers.isEmpty else { return }
        observe(UIApplication.willResignActiveNotification) { [weak self] in
            self?.presenter.showRedaction()
        }
        observe(UIApplication.didEnterBackgroundNotification) { [weak self] in
            self?.session.relock(.enteredBackground)
            self?.presenter.showRedaction()
        }
        observe(UIApplication.willEnterForegroundNotification) { [weak self] in
            self?.session.relock(.willEnterForeground)
        }
        observe(UIApplication.protectedDataWillBecomeUnavailableNotification) { [weak self] in
            self?.session.relock(.protectedDataUnavailable)
        }
        observe(UIApplication.didBecomeActiveNotification) { [weak self] in
            self?.presenter.hideRedaction()
        }
    }

    func stop() {
        observers.forEach { notificationCenter.removeObserver($0) }
        observers.removeAll()
    }

    private func observe(_ name: Notification.Name, _ action: @escaping () -> Void) {
        observers.append(notificationCenter.addObserver(forName: name, object: nil, queue: nil) { _ in action() })
    }
}

/// The read lock as the scene cover sees it. The session itself stays inside the services module.
public protocol ReadLockState: AnyObject {
    var readsAreUnlocked: Bool { get }
    /// Calls `handler` whenever the read scope changes, on any thread. Returns the registration; release it to stop.
    func observeReadScopeChanges(_ handler: @escaping () -> Void) -> AnyObject
}

private final class ReadScopeRegistration {
    private let session: ReadAuthenticationSession
    private let token: SessionScopeObserverToken

    init(session: ReadAuthenticationSession, handler: @escaping () -> Void) {
        self.session = session
        token = session.observeScope { _ in handler() }
    }

    deinit { session.removeScopeObserver(token) }
}

extension ServiceCompositionRoot: ReadLockState {
    public var readsAreUnlocked: Bool { readSession.scope == .authenticated }

    public func observeReadScopeChanges(_ handler: @escaping () -> Void) -> AnyObject {
        ReadScopeRegistration(session: readSession, handler: handler)
    }
}

/// Covers a scene while it is not active and reads are unlocked, and only the cover: the relock stays with the app's
/// own lifecycle handling, so nothing relocks twice. While locked nothing stored is on screen, so the app's own Face ID
/// prompt (which resigns the scene) never shows the cover. The cover is raised when the scene will resign active or
/// enters the background, before the system takes its snapshot, or when reads unlock while the scene is inactive. The
/// lock state decides only whether it is raised: once raised, only the scene becoming active removes it, so a relock
/// (which runs after `sceneDidEnterBackground` and before the snapshot), a scope change or a URL never does. Every
/// capture entry (URL, Control, shortcut, App Intent) makes the scene active, which removes the cover and shows the
/// capture surface. A scene that becomes active without resigning never shows it. Called on the main thread.
public final class SceneCoverController {
    private let presenter: RedactionPresenter
    private let readLock: ReadLockState
    private var registration: AnyObject?
    private var isInactive = false
    public private(set) var isCovered = false

    public init(presenter: RedactionPresenter, readLock: ReadLockState) {
        self.presenter = presenter
        self.readLock = readLock
        registration = readLock.observeReadScopeChanges { [weak self] in
            if Thread.isMainThread {
                self?.raiseIfNeeded()
            } else {
                DispatchQueue.main.async { self?.raiseIfNeeded() }
            }
        }
    }

    public func sceneWillResignActive() {
        isInactive = true
        raiseIfNeeded()
    }

    public func sceneDidEnterBackground() {
        isInactive = true
        raiseIfNeeded()
    }

    public func sceneDidBecomeActive() {
        isInactive = false
        guard isCovered else { return }
        isCovered = false
        presenter.hideRedaction()
    }

    private func raiseIfNeeded() {
        guard isInactive, !isCovered, readLock.readsAreUnlocked else { return }
        isCovered = true
        presenter.showRedaction()
    }
}
