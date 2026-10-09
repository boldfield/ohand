import UIKit

/// Covers (and uncovers) everything the app shows. Called on the main thread.
protocol RedactionPresenter: AnyObject {
    func showRedaction()
    func hideRedaction()
}

/// Puts an opaque window above every connected scene so the system's app-switcher snapshot, taken as the app resigns
/// active, contains no stored content. The cover carries no text from the app. A simulator shows the cover but only a
/// device shows the real snapshot; the physical check is device evidence (device evidence task T05, `docs/validation/m1-device-results.md`).
final class WindowRedactionPresenter: RedactionPresenter {
    private let windowScenes: () -> [UIWindowScene]
    private var coverWindows: [UIWindow] = []

    init(windowScenes: @escaping () -> [UIWindowScene] = {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    }) {
        self.windowScenes = windowScenes
    }

    var coverWindowCount: Int { coverWindows.count }

    func showRedaction() {
        guard coverWindows.isEmpty else { return }
        for scene in windowScenes() {
            let window = UIWindow(windowScene: scene)
            window.windowLevel = UIWindow.Level.alert + 1
            let controller = UIViewController()
            controller.view.backgroundColor = .systemBackground
            controller.view.accessibilityIgnoresInvertColors = true
            window.rootViewController = controller
            window.isHidden = false
            coverWindows.append(window)
        }
    }

    func hideRedaction() {
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
