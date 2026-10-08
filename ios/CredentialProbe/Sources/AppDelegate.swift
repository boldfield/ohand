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


    static func storeCredential(value: String, accessibility: AccessibilityClass) -> (success: Bool, status: OSStatus) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: syntheticServiceName,
            kSecAttrAccount as String: syntheticAccountName + "-\(accessibility.displayName)",
            kSecValueData as String: value.data(using: .utf8)!,
            kSecAttrAccessible as String: accessibility.keychainValue,
        ]

        SecItemDelete(query as CFDictionary)
        let status = SecItemAdd(query as CFDictionary, nil)
        return (success: status == errSecSuccess, status: status)
    }

    static func retrieveCredential(accessibility: AccessibilityClass) -> (value: String?, status: OSStatus) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: syntheticServiceName,
            kSecAttrAccount as String: syntheticAccountName + "-\(accessibility.displayName)",
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        if status == errSecSuccess, let data = result as? Data {
            let value = String(data: data, encoding: .utf8)
            return (value: value, status: status)
        }
        return (value: nil, status: status)
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

}

class CredentialProbeViewController: UIViewController {
    private let scrollView = UIScrollView()
    private let contentView = UIView()
    private var resultLabels: [UILabel] = []
    private var statusLabel: UILabel?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        setupScrollView()
        setupUI()
        displayStoredStatus()
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

        let statusLbl = UILabel()
        statusLbl.font = UIFont.systemFont(ofSize: 11, weight: .regular)
        statusLbl.numberOfLines = 0
        statusLbl.textColor = .tertiaryLabel
        container.addArrangedSubview(statusLbl)
        self.statusLabel = statusLbl

        let noteLabel = UILabel()
        noteLabel.text = "Note: Synthetic credentials only. Lock/unlock and relaunch tests require manual verification on physical device."
        noteLabel.font = UIFont.systemFont(ofSize: 12, weight: .light)
        noteLabel.numberOfLines = 0
        noteLabel.textAlignment = .center
        noteLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(noteLabel)

        let storeButton = UIButton(type: .system)
        storeButton.setTitle("Store All Credentials", for: .normal)
        storeButton.addTarget(self, action: #selector(storeAllCredentials), for: .touchUpInside)
        container.addArrangedSubview(storeButton)

        let retrieveButton = UIButton(type: .system)
        retrieveButton.setTitle("Retrieve & Check", for: .normal)
        retrieveButton.addTarget(self, action: #selector(retrieveAndCheck), for: .touchUpInside)
        container.addArrangedSubview(retrieveButton)

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
    private func storeAllCredentials() {
        let accessibilityClasses: [KeychainTester.AccessibilityClass] = [
            .whenUnlocked,
            .afterFirstUnlock,
            .afterFirstUnlockThisDeviceOnly,
            .whenUnlockedThisDeviceOnly,
            .whenPasscodeSetThisDeviceOnly,
        ]

        let dateFormatter = DateFormatter()
        dateFormatter.timeStyle = .medium
        let timestamp = dateFormatter.string(from: Date())

        for (index, accessClass) in accessibilityClasses.enumerated() {
            let syntheticValue = "synthetic-credential-\(accessClass.displayName)"
            let result = KeychainTester.storeCredential(value: syntheticValue, accessibility: accessClass)
            if index < resultLabels.count {
                let statusStr = result.status != errSecSuccess ? " (status: \(result.status))" : ""
                let text = result.success ? "✓ Stored" : "✗ Failed to store\(statusStr)"
                resultLabels[index].text = "\(accessClass.displayName): \(text)\n\(timestamp)"
                resultLabels[index].textColor = result.success ? .systemGreen : .systemRed
            }
        }

        statusLabel?.text = "Credentials stored. Tap 'Retrieve & Check' to test access."
    }

    @objc
    private func retrieveAndCheck() {
        let accessibilityClasses: [KeychainTester.AccessibilityClass] = [
            .whenUnlocked,
            .afterFirstUnlock,
            .afterFirstUnlockThisDeviceOnly,
            .whenUnlockedThisDeviceOnly,
            .whenPasscodeSetThisDeviceOnly,
        ]

        let dateFormatter = DateFormatter()
        dateFormatter.timeStyle = .medium
        let timestamp = dateFormatter.string(from: Date())

        for (index, accessClass) in accessibilityClasses.enumerated() {
            let syntheticValue = "synthetic-credential-\(accessClass.displayName)"
            let result = KeychainTester.retrieveCredential(accessibility: accessClass)
            let retrieved = result.value == syntheticValue
            if index < resultLabels.count {
                let statusStr = result.status != errSecSuccess ? " (status: \(result.status))" : ""
                let text = retrieved ? "✓ Retrieved" : "✗ Not found\(statusStr)"
                resultLabels[index].text = "\(accessClass.displayName): \(text)\n\(timestamp)"
                resultLabels[index].textColor = retrieved ? .systemGreen : .systemRed
            }
        }

        statusLabel?.text = "Retrieve complete. Store again to reset tests."
    }

    @objc
    private func clearCredentials() {
        KeychainTester.deleteAllTestCredentials()
        for label in resultLabels {
            label.text = "(Cleared)"
            label.textColor = .tertiaryLabel
        }
        statusLabel?.text = "All test credentials removed."
    }

    private func displayStoredStatus() {
        for label in resultLabels {
            label.text = "(Tap 'Store All Credentials' to begin)"
            label.textColor = .tertiaryLabel
        }
        statusLabel?.text = "Ready. Tap buttons to store and retrieve credentials separately."
    }
}
