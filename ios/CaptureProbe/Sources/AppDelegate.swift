import UIKit

@main
class CaptureProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool { return true }
    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
        configuration.delegateClass = CaptureProbeSceneDelegate.self
        return configuration
    }
}

class CaptureProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private let captureViewController = CaptureProbeViewController()
    private var hasEnteredForeground = false
    private var isForeground = false
    private var launchKind: IngressLaunchKind = .cold
    private var handoffObserver: NSObjectProtocol?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = captureViewController
        self.window = window
        window.makeKeyAndVisible()
        handoffObserver = NotificationCenter.default.addObserver(
            forName: IngressFlow.handoffRegisteredNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.handoffArrivedWhileForeground()
        }
    }

    // Fires for the cold launch and for every later return from the background (warm launch).
    func sceneWillEnterForeground(_ scene: UIScene) {
        isForeground = true
        launchKind = hasEnteredForeground ? .warm : .cold
        hasEnteredForeground = true
        presentEntry(source: .directLaunch)
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        isForeground = false
    }

    // A handoff that arrives while the app is backgrounded is consumed by the next foreground entry.
    private func handoffArrivedWhileForeground() {
        guard isForeground else { return }
        presentEntry(source: .controlIntent)
    }

    private func presentEntry(source: IngressSource) {
        let outcome = IngressFlow.live.enter(
            source: source,
            launchKind: launchKind,
            protectedDataAvailable: UIApplication.shared.isProtectedDataAvailable
        )
        captureViewController.render(outcome)
        IngressFlow.live.recordPresentation(outcome)
    }
}

class CaptureProbeViewController: UIViewController {
    private let statusLabel = UILabel()
    private let detailLabel = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 16
        container.alignment = .center
        container.distribution = .fillProportionally
        container.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = UILabel()
        titleLabel.text = "Capture Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "System control handoff and protected ingress probe.\nShows only the current entry."
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        statusLabel.text = "Waiting for entry"
        statusLabel.font = UIFont.systemFont(ofSize: 16, weight: .semibold)
        statusLabel.accessibilityIdentifier = "capture-status"
        container.addArrangedSubview(statusLabel)

        detailLabel.numberOfLines = 0
        detailLabel.textAlignment = .center
        detailLabel.font = UIFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        detailLabel.textColor = .secondaryLabel
        detailLabel.accessibilityIdentifier = "capture-detail"
        container.addArrangedSubview(detailLabel)

        view.addSubview(container)
        NSLayoutConstraint.activate([
            container.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            container.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            container.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -20),
        ])
    }

    func render(_ outcome: IngressOutcome) {
        loadViewIfNeeded()
        statusLabel.text = outcome.statusText
        detailLabel.text = outcome.displayLines.joined(separator: "\n")
    }
}
