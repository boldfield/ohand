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
    private let session = IngressSession(flow: IngressFlow.live)

    private var protectedDataAvailable: Bool { UIApplication.shared.isProtectedDataAvailable }

    func scene(
        _ scene: UIScene,
        willConnectTo sceneSession: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        captureViewController.onOpenManagement = { url, completion in
            UIApplication.shared.open(url, options: [:], completionHandler: completion)
        }
        window.rootViewController = captureViewController
        self.window = window
        window.makeKeyAndVisible()
        session.observeHandoffs(
            protectedDataAvailable: { UIApplication.shared.isProtectedDataAvailable },
            onOutcome: { [weak self] outcome in self?.present(outcome) }
        )
        // Cold launch from the shortcut URL: the scene is not foreground yet, so this only registers the handoff
        // and the foreground callback commits it.
        receive(urls: connectionOptions.urlContexts.map(\.url))
    }

    func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
        receive(urls: URLContexts.map(\.url))
    }

    func sceneWillEnterForeground(_ scene: UIScene) {
        present(session.willEnterForeground(protectedDataAvailable: protectedDataAvailable))
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        session.didEnterBackground()
    }

    private func receive(urls: [URL]) {
        for url in urls {
            if let outcome = session.receive(url: url, protectedDataAvailable: protectedDataAvailable) {
                present(outcome)
            }
        }
    }

    private func present(_ outcome: IngressOutcome?) {
        captureViewController.render(outcome)
        IngressFlow.live.recordPresentation(outcome)
    }
}

class CaptureProbeViewController: UIViewController {
    /// Opens a handoff URL and reports whether the system accepted it. Set by the scene delegate.
    var onOpenManagement: ((URL, @escaping (Bool) -> Void) -> Void)?

    private let statusLabel = UILabel()
    private let detailLabel = UILabel()
    private let managementButton = UIButton(type: .system)
    private let managementStatusLabel = UILabel()
    private var managementURL: URL?

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
        titleLabel.font = UIFontMetrics(forTextStyle: .title1).scaledFont(for: UIFont.systemFont(ofSize: 24, weight: .bold))
        titleLabel.adjustsFontForContentSizeCategory = true
        titleLabel.numberOfLines = 0
        titleLabel.textAlignment = .center
        titleLabel.accessibilityTraits = .header
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "System control handoff and protected ingress probe.\nShows only the current entry."
        descriptionLabel.font = UIFont.preferredFont(forTextStyle: .footnote)
        descriptionLabel.adjustsFontForContentSizeCategory = true
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        statusLabel.text = "Waiting for entry"
        statusLabel.font = UIFont.preferredFont(forTextStyle: .headline)
        statusLabel.adjustsFontForContentSizeCategory = true
        statusLabel.numberOfLines = 0
        statusLabel.textAlignment = .center
        statusLabel.accessibilityIdentifier = "capture-status"
        container.addArrangedSubview(statusLabel)

        detailLabel.numberOfLines = 0
        detailLabel.textAlignment = .center
        detailLabel.font = UIFontMetrics(forTextStyle: .caption1)
            .scaledFont(for: UIFont.monospacedSystemFont(ofSize: 11, weight: .regular))
        detailLabel.adjustsFontForContentSizeCategory = true
        detailLabel.textColor = .secondaryLabel
        detailLabel.accessibilityIdentifier = "capture-detail"
        container.addArrangedSubview(detailLabel)

        var buttonConfiguration = UIButton.Configuration.filled()
        buttonConfiguration.title = "Review in management app"
        buttonConfiguration.titleLineBreakMode = .byWordWrapping
        buttonConfiguration.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { attributes in
            var scaled = attributes
            scaled.font = UIFont.preferredFont(forTextStyle: .headline)
            return scaled
        }
        managementButton.configuration = buttonConfiguration
        managementButton.accessibilityIdentifier = "open-management"
        managementButton.accessibilityHint = "Opens the management app. Only this entry's identifier is sent."
        managementButton.addTarget(self, action: #selector(openManagement), for: .touchUpInside)
        managementButton.isEnabled = false
        container.addArrangedSubview(managementButton)

        managementStatusLabel.font = UIFont.preferredFont(forTextStyle: .footnote)
        managementStatusLabel.adjustsFontForContentSizeCategory = true
        managementStatusLabel.numberOfLines = 0
        managementStatusLabel.textAlignment = .center
        managementStatusLabel.textColor = .secondaryLabel
        managementStatusLabel.accessibilityIdentifier = "management-status"
        container.addArrangedSubview(managementStatusLabel)

        // Scrolls so every control stays reachable at the largest accessibility text sizes.
        let scrollView = UIScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scrollView)
        scrollView.addSubview(container)
        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            container.topAnchor.constraint(equalTo: scrollView.contentLayoutGuide.topAnchor, constant: 40),
            container.bottomAnchor.constraint(equalTo: scrollView.contentLayoutGuide.bottomAnchor, constant: -24),
            container.leadingAnchor.constraint(equalTo: scrollView.contentLayoutGuide.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(equalTo: scrollView.contentLayoutGuide.trailingAnchor, constant: -20),
            container.widthAnchor.constraint(equalTo: scrollView.frameLayoutGuide.widthAnchor, constant: -40),
        ])
    }

    /// Renders the current entry, or the idle screen when there is none. The management button is enabled only
    /// for an entry that is already saved; the save never depends on it.
    func render(_ outcome: IngressOutcome?) {
        loadViewIfNeeded()
        statusLabel.text = outcome?.statusText ?? IngressOutcome.idleStatusText
        detailLabel.text = (outcome?.displayLines ?? IngressOutcome.idleDisplayLines).joined(separator: "\n")
        managementURL = outcome?.managementHandoffURL
        managementButton.isEnabled = managementURL != nil
        managementStatusLabel.text = ""
    }

    @objc private func openManagement() {
        guard let url = managementURL, let onOpenManagement = onOpenManagement else { return }
        managementStatusLabel.text = "Opening management app…"
        onOpenManagement(url) { [weak self] opened in
            DispatchQueue.main.async {
                self?.managementStatusLabel.text = opened
                    ? "Sent this entry's identifier to the management app."
                    : "Management app unavailable. The entry is saved."
            }
        }
    }
}
