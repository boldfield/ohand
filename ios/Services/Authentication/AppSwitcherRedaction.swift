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

    public init(windowScenes: @escaping () -> [UIWindowScene] = {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    }) {
        self.windowScenes = windowScenes
    }

    var coverWindowCount: Int { coverWindows.count }

    /// The design's `brand` indigo in light theme and the dark launch ground in dark theme.
    static let brandBackground = UIColor { traits in
        traits.userInterfaceStyle == .dark
            ? UIColor(red: 0x0A / 255, green: 0x0B / 255, blue: 0x12 / 255, alpha: 1)
            : UIColor(red: 0x1E / 255, green: 0x27 / 255, blue: 0x61 / 255, alpha: 1)
    }

    /// The italic ampersand mark. A fixed glyph, never stored history.
    private static func makeMark() -> UIView {
        let label = UILabel()
        let baseFont = UIFont(descriptor: UIFontDescriptor.preferredFontDescriptor(withTextStyle: .largeTitle)
            .withDesign(.serif) ?? UIFontDescriptor(), size: 120)
        let italic = baseFont.fontDescriptor.withSymbolicTraits(.traitItalic)
            .map { UIFont(descriptor: $0, size: 120) } ?? baseFont
        label.font = italic
        label.text = "&"
        label.textColor = .white
        label.isAccessibilityElement = false
        label.sizeToFit()
        label.autoresizingMask = [.flexibleLeftMargin, .flexibleRightMargin, .flexibleTopMargin, .flexibleBottomMargin]
        label.translatesAutoresizingMaskIntoConstraints = true
        return label
    }

    public func showRedaction() {
        guard coverWindows.isEmpty else { return }
        for scene in windowScenes() {
            let window = UIWindow(windowScene: scene)
            window.windowLevel = UIWindow.Level.alert + 1
            let controller = UIViewController()
            controller.view.backgroundColor = Self.brandBackground
            controller.view.addSubview(Self.makeMark())
            controller.view.accessibilityIgnoresInvertColors = true
            window.rootViewController = controller
            controller.view.subviews.first.map { mark in
                mark.center = CGPoint(x: scene.coordinateSpace.bounds.midX, y: scene.coordinateSpace.bounds.midY)
            }
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

/// Covers a scene while it is inactive, and only the cover: the relock stays with the app's own lifecycle handling, so
/// nothing relocks twice. The cover is raised when the scene will resign active, before the system takes its snapshot,
/// and removed when the scene becomes active. A scene that becomes active without resigning never shows it.
public final class SceneCoverController {
    private let presenter: RedactionPresenter
    public private(set) var isCovered = false

    public init(presenter: RedactionPresenter) {
        self.presenter = presenter
    }

    public func sceneWillResignActive() {
        guard !isCovered else { return }
        isCovered = true
        presenter.showRedaction()
    }

    public func sceneDidBecomeActive() {
        removeCover()
    }

    /// A capture entry (Control, shortcut or URL) shows the capture surface, not the cover.
    public func captureEntryArrived() {
        removeCover()
    }

    private func removeCover() {
        guard isCovered else { return }
        isCovered = false
        presenter.hideRedaction()
    }
}
