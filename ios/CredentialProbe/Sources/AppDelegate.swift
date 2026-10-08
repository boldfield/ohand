import UIKit
import Security

@main
class CredentialProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        if ProcessInfo.processInfo.arguments.contains(KeychainSelfTest.launchArgument) {
            DispatchQueue.main.async { KeychainSelfTest.runAndExit() }
        }
        return true
    }
    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        let configuration = UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
        configuration.delegateClass = CredentialProbeSceneDelegate.self
        return configuration
    }
}

class CredentialProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = CredentialProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }

    func sceneDidEnterBackground(_ scene: UIScene) {
        LockedRetrievalProbe.shared.beginIfArmed()
    }
}

class CredentialProbeViewController: UIViewController {
    private let scrollView = UIScrollView()
    private let contentView = UIView()
    private var resultLabels: [UILabel] = []
    private let statusLabel = UILabel()
    private let lockedLogLabel = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        setupScrollView()
        setupUI()
        showReadyState()
    }

    private func setupScrollView() {
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        contentView.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(scrollView)
        scrollView.addSubview(contentView)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.topAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.bottomAnchor),

            contentView.topAnchor.constraint(equalTo: scrollView.topAnchor),
            contentView.leadingAnchor.constraint(equalTo: scrollView.leadingAnchor),
            contentView.trailingAnchor.constraint(equalTo: scrollView.trailingAnchor),
            contentView.bottomAnchor.constraint(equalTo: scrollView.bottomAnchor),
            contentView.widthAnchor.constraint(equalTo: scrollView.widthAnchor),
        ])
    }

    private func makeButton(title: String, action: Selector) -> UIButton {
        let button = UIButton(type: .system)
        button.setTitle(title, for: .normal)
        button.addTarget(self, action: action, for: .touchUpInside)
        return button
    }

    private func setupUI() {
        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 16
        container.alignment = .fill
        container.distribution = .fill
        container.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = UILabel()
        titleLabel.text = "Credential Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "Keychain accessibility and lock behavior with synthetic credentials only"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        for _ in KeychainTester.AccessibilityClass.allCases {
            let resultLabel = UILabel()
            resultLabel.font = UIFont.monospacedSystemFont(ofSize: 12, weight: .regular)
            resultLabel.numberOfLines = 0
            resultLabel.textColor = .secondaryLabel
            container.addArrangedSubview(resultLabel)
            resultLabels.append(resultLabel)
        }

        statusLabel.font = UIFont.systemFont(ofSize: 12, weight: .regular)
        statusLabel.numberOfLines = 0
        statusLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(statusLabel)

        container.addArrangedSubview(makeButton(title: "Store All Credentials", action: #selector(storeAllCredentials)))
        container.addArrangedSubview(makeButton(title: "Retrieve & Check (no store)", action: #selector(retrieveAndCheck)))
        container.addArrangedSubview(makeButton(title: "Arm Locked Retrieval (stores, then lock device)", action: #selector(armLockedRetrieval)))
        container.addArrangedSubview(makeButton(title: "Show Locked-Retrieval Log", action: #selector(showLockedLog)))
        container.addArrangedSubview(makeButton(title: "Clear Locked-Retrieval Log", action: #selector(clearLockedLog)))
        container.addArrangedSubview(makeButton(title: "Clear Test Credentials", action: #selector(clearCredentials)))

        lockedLogLabel.font = UIFont.monospacedSystemFont(ofSize: 9, weight: .regular)
        lockedLogLabel.numberOfLines = 0
        lockedLogLabel.textColor = .secondaryLabel
        container.addArrangedSubview(lockedLogLabel)

        contentView.addSubview(container)
        NSLayoutConstraint.activate([
            container.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 20),
            container.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            container.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),
            container.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -20),
        ])
    }

    private func timestampText() -> String {
        let dateFormatter = DateFormatter()
        dateFormatter.timeStyle = .medium
        return dateFormatter.string(from: Date())
    }

    private func storeAll() {
        let timestamp = timestampText()
        for (index, accessibility) in KeychainTester.AccessibilityClass.allCases.enumerated() {
            let status = KeychainTester.storeCredential(
                value: KeychainTester.syntheticValue(for: accessibility),
                accessibility: accessibility
            )
            let stored = status == errSecSuccess
            resultLabels[index].text = "\(accessibility.displayName): \(stored ? "stored" : "store failed") "
                + "status=\(KeychainTester.statusText(status)) at \(timestamp)"
            resultLabels[index].textColor = stored ? .systemGreen : .systemRed
        }
    }

    @objc
    private func storeAllCredentials() {
        storeAll()
        statusLabel.text = "Stored. Use 'Retrieve & Check' to read without storing."
    }

    @objc
    private func retrieveAndCheck() {
        let timestamp = timestampText()
        for (index, accessibility) in KeychainTester.AccessibilityClass.allCases.enumerated() {
            let retrieved = KeychainTester.retrieveCredential(accessibility: accessibility)
            let matches = retrieved.value == KeychainTester.syntheticValue(for: accessibility)
            resultLabels[index].text = "\(accessibility.displayName): \(matches ? "retrieved" : "not retrieved") "
                + "status=\(KeychainTester.statusText(retrieved.status)) at \(timestamp)"
            resultLabels[index].textColor = matches ? .systemGreen : .systemRed
        }
        statusLabel.text = "Retrieve complete (protectedDataAvailable=\(UIApplication.shared.isProtectedDataAvailable))."
    }

    @objc
    private func armLockedRetrieval() {
        storeAll()
        LockedRetrievalProbe.shared.arm()
        statusLabel.text = "Armed. Lock the device now; wait at least 30 seconds, unlock, return here and tap 'Show Locked-Retrieval Log'."
    }

    @objc
    private func showLockedLog() {
        let log = LockedRetrievalProbe.shared.readLog()
        lockedLogLabel.text = log.isEmpty ? "(locked-retrieval log is empty)" : log
    }

    @objc
    private func clearLockedLog() {
        LockedRetrievalProbe.shared.clearLog()
        lockedLogLabel.text = "(locked-retrieval log cleared)"
    }

    @objc
    private func clearCredentials() {
        KeychainTester.deleteAllTestCredentials()
        for label in resultLabels {
            label.text = "(cleared)"
            label.textColor = .tertiaryLabel
        }
        statusLabel.text = "All probe test credentials removed."
    }

    private func showReadyState() {
        for label in resultLabels {
            label.text = "(tap 'Store All Credentials' to begin)"
            label.textColor = .tertiaryLabel
        }
        statusLabel.text = "Ready. Store and retrieve are separate actions; launching does not touch the Keychain."
    }
}
