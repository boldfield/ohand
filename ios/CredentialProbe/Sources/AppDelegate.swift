import UIKit
import Security

@main
class CredentialProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
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
}

class KeychainTester {
    static let syntheticServiceName = "com.boldfield.ohand.probes.credential.test"
    static let syntheticAccountName = "test-credential"

    enum AccessibilityClass {
        case whenUnlocked
        case afterFirstUnlock
        case afterFirstUnlockThisDeviceOnly
        case whenUnlockedThisDeviceOnly
        case whenPasscodeSetThisDeviceOnly

        var keychainValue: CFString {
            switch self {
            case .whenUnlocked:
                return kSecAttrAccessibleWhenUnlocked
            case .afterFirstUnlock:
                return kSecAttrAccessibleAfterFirstUnlock
            case .afterFirstUnlockThisDeviceOnly:
                return kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            case .whenUnlockedThisDeviceOnly:
                return kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            case .whenPasscodeSetThisDeviceOnly:
                return kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly
            }
        }

        var displayName: String {
            switch self {
            case .whenUnlocked:
                return "WhenUnlocked"
            case .afterFirstUnlock:
                return "AfterFirstUnlock"
            case .afterFirstUnlockThisDeviceOnly:
                return "AfterFirstUnlockThisDeviceOnly"
            case .whenUnlockedThisDeviceOnly:
                return "WhenUnlockedThisDeviceOnly"
            case .whenPasscodeSetThisDeviceOnly:
                return "WhenPasscodeSetThisDeviceOnly"
            }
        }
    }

    struct TestResult {
        let accessibilityClass: AccessibilityClass
        let stored: Bool
        let retrieved: Bool
        let timestamp: String

        var summary: String {
            if stored && retrieved {
                return "✓ \(accessibilityClass.displayName): Stored & Retrieved"
            } else if stored && !retrieved {
                return "⚠ \(accessibilityClass.displayName): Stored but not retrieved"
            } else {
                return "✗ \(accessibilityClass.displayName): Failed to store"
            }
        }
    }

    static func storeCredential(value: String, accessibility: AccessibilityClass) -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: syntheticServiceName,
            kSecAttrAccount as String: syntheticAccountName + "-\(accessibility.displayName)",
            kSecValueData as String: value.data(using: .utf8)!,
            kSecAttrAccessible as String: accessibility.keychainValue,
        ]

        SecItemDelete(query as CFDictionary)
        let status = SecItemAdd(query as CFDictionary, nil)
        return status == errSecSuccess
    }

    static func retrieveCredential(accessibility: AccessibilityClass) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: syntheticServiceName,
            kSecAttrAccount as String: syntheticAccountName + "-\(accessibility.displayName)",
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        guard status == errSecSuccess, let data = result as? Data else {
            return nil
        }

        return String(data: data, encoding: .utf8)
    }

    static func deleteAllTestCredentials() {
        let accessibilityClasses: [AccessibilityClass] = [
            .whenUnlocked,
            .afterFirstUnlock,
            .afterFirstUnlockThisDeviceOnly,
            .whenUnlockedThisDeviceOnly,
            .whenPasscodeSetThisDeviceOnly,
        ]

        for accessClass in accessibilityClasses {
            let query: [String: Any] = [
                kSecClass as String: kSecClassGenericPassword,
                kSecAttrService as String: syntheticServiceName,
                kSecAttrAccount as String: syntheticAccountName + "-\(accessClass.displayName)",
            ]
            SecItemDelete(query as CFDictionary)
        }
    }

    static func runAllTests() -> [TestResult] {
        let accessibilityClasses: [AccessibilityClass] = [
            .whenUnlocked,
            .afterFirstUnlock,
            .afterFirstUnlockThisDeviceOnly,
            .whenUnlockedThisDeviceOnly,
            .whenPasscodeSetThisDeviceOnly,
        ]

        let dateFormatter = DateFormatter()
        dateFormatter.timeStyle = .medium
        let timestamp = dateFormatter.string(from: Date())

        return accessibilityClasses.map { accessClass in
            let syntheticValue = "synthetic-credential-\(accessClass.displayName)"
            let stored = storeCredential(value: syntheticValue, accessibility: accessClass)
            let retrieved = retrieveCredential(accessibility: accessClass) == syntheticValue
            return TestResult(
                accessibilityClass: accessClass,
                stored: stored,
                retrieved: retrieved,
                timestamp: timestamp
            )
        }
    }
}

class CredentialProbeViewController: UIViewController {
    private let scrollView = UIScrollView()
    private let contentView = UIView()
    private var resultLabels: [UILabel] = []

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        setupScrollView()
        setupUI()
        runTests()
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
        descriptionLabel.text = "Keychain accessibility and lock behavior\nCredential protection and device lock integration"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        let sectionLabel = UILabel()
        sectionLabel.text = "Accessibility Class Tests"
        sectionLabel.font = UIFont.systemFont(ofSize: 16, weight: .semibold)
        sectionLabel.textColor = .label
        container.addArrangedSubview(sectionLabel)

        for _ in 0..<5 {
            let resultLabel = UILabel()
            resultLabel.font = UIFont.monospacedSystemFont(ofSize: 12, weight: .regular)
            resultLabel.numberOfLines = 0
            resultLabel.textColor = .secondaryLabel
            container.addArrangedSubview(resultLabel)
            resultLabels.append(resultLabel)
        }

        let noteLabel = UILabel()
        noteLabel.text = "Note: Synthetic credentials only. Lock/unlock and relaunch tests require manual verification on physical device."
        noteLabel.font = UIFont.systemFont(ofSize: 12, weight: .light)
        noteLabel.numberOfLines = 0
        noteLabel.textAlignment = .center
        noteLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(noteLabel)

        let refreshButton = UIButton(type: .system)
        refreshButton.setTitle("Refresh Tests", for: .normal)
        refreshButton.addTarget(self, action: #selector(runTests), for: .touchUpInside)
        container.addArrangedSubview(refreshButton)

        let clearButton = UIButton(type: .system)
        clearButton.setTitle("Clear Test Credentials", for: .normal)
        clearButton.addTarget(self, action: #selector(clearCredentials), for: .touchUpInside)
        container.addArrangedSubview(clearButton)

        contentView.addSubview(container)
        NSLayoutConstraint.activate([
            container.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 20),
            container.leadingAnchor.constraint(equalTo: contentView.leadingAnchor, constant: 16),
            container.trailingAnchor.constraint(equalTo: contentView.trailingAnchor, constant: -16),
            container.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -20),
        ])
    }

    @objc
    private func runTests() {
        let results = KeychainTester.runAllTests()
        for (index, result) in results.enumerated() {
            if index < resultLabels.count {
                resultLabels[index].text = result.summary + "\n\(result.timestamp)"
                resultLabels[index].textColor = result.retrieved ? .systemGreen : .systemRed
            }
        }
    }

    @objc
    private func clearCredentials() {
        KeychainTester.deleteAllTestCredentials()
        for label in resultLabels {
            label.text = "Cleared"
            label.textColor = .tertiaryLabel
        }
    }
}
