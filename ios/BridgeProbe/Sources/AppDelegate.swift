import UIKit

@main
class BridgeProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        return true
    }

    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(
            name: "Default Configuration",
            sessionRole: connectingSceneSession.role
        )
        configuration.delegateClass = BridgeProbeSceneDelegate.self
        return configuration
    }
}

class BridgeProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = BridgeProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class BridgeProbeViewController: UIViewController {
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
        titleLabel.text = "Bridge Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "Rust-Swift boundary validation\nRound-trip capture-shaped value test"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        let statusLabel = UILabel()
        statusLabel.accessibilityIdentifier = "boundary-status"
        statusLabel.numberOfLines = 0
        statusLabel.textAlignment = .center
        do {
            statusLabel.text = try BoundarySelfCheck.run()
        } catch {
            fatalError("Rust boundary self-check failed: \(error)")
        }
        statusLabel.font = UIFont.systemFont(ofSize: 12, weight: .light)
        statusLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(statusLabel)

        view.addSubview(container)
        NSLayoutConstraint.activate([
            container.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            container.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            container.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -20)
        ])
    }
}
