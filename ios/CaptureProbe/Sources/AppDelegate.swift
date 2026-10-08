import UIKit
import Foundation

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

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = CaptureProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class CaptureProbeViewController: UIViewController {
    private var ingressLabel: UILabel!
    private var statusLabel: UILabel!

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
        descriptionLabel.text = "System control handoff and entry validation\nCapture entry point and production surface flow"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        statusLabel = UILabel()
        statusLabel.text = "Creating ingress record…"
        statusLabel.font = UIFont.systemFont(ofSize: 12, weight: .light)
        statusLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(statusLabel)

        ingressLabel = UILabel()
        ingressLabel.text = "—"
        ingressLabel.font = UIFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        ingressLabel.numberOfLines = 0
        ingressLabel.textAlignment = .center
        container.addArrangedSubview(ingressLabel)

        let scrollView = UIScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.addSubview(container)
        view.addSubview(scrollView)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            container.centerXAnchor.constraint(equalTo: scrollView.centerXAnchor),
            container.widthAnchor.constraint(lessThanOrEqualTo: scrollView.widthAnchor, constant: -40),
            container.topAnchor.constraint(greaterThanOrEqualTo: scrollView.topAnchor, constant: 20),
            container.bottomAnchor.constraint(lessThanOrEqualTo: scrollView.bottomAnchor, constant: -20)
        ])

        createIngressRecord()
    }

    private func createIngressRecord() {
        let captureId = UUID().uuidString
        let now = ISO8601DateFormatter().string(from: Date())
        let isLocked = !UIApplication.shared.isProtectedDataAvailable

        DispatchQueue.main.async {
            self.ingressLabel.text = """
            Capture ID: \(captureId)
            Time: \(now)
            Locked: \(isLocked)
            """
            self.statusLabel.text = "Ingress record created"
        }

        DispatchQueue.global().async {
            print("Probe ingress: ID=\(captureId) locked=\(isLocked)")
        }
    }
}
