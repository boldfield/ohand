import UIKit
import OhAndCoreBridge

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

        let testButton = UIButton(type: .system)
        testButton.setTitle("Run Tests", for: .normal)
        testButton.titleLabel?.font = UIFont.systemFont(ofSize: 16, weight: .semibold)
        testButton.addTarget(self, action: #selector(runTests), for: .touchUpInside)
        container.addArrangedSubview(testButton)

        let statusLabel = UILabel()
        statusLabel.text = "Ready to test"
        statusLabel.font = UIFont.systemFont(ofSize: 12, weight: .light)
        statusLabel.textColor = .tertiaryLabel
        statusLabel.numberOfLines = 0
        statusLabel.textAlignment = .center
        container.addArrangedSubview(statusLabel)
        self.statusLabel = statusLabel

        view.addSubview(container)
        NSLayoutConstraint.activate([
            container.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            container.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            container.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -20),
            container.widthAnchor.constraint(lessThanOrEqualToConstant: 300)
        ])
    }

    var statusLabel: UILabel?

    @objc private func runTests() {
        var results: [String] = []

        // Test 1: ASCII round-trip
        do {
            let capture = try createCapture(
                captureId: "test-1",
                text: "Hello, World!",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.text == "Hello, World!" {
                results.append("✓ ASCII round-trip")
            } else {
                results.append("✗ ASCII round-trip: text mismatch")
            }
        } catch {
            results.append("✗ ASCII round-trip: \(error)")
        }

        // Test 2: Unicode round-trip
        do {
            let capture = try createCapture(
                captureId: "test-2",
                text: "Héllo, 世界! 🌍",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.text == "Héllo, 世界! 🌍" {
                results.append("✓ Unicode round-trip")
            } else {
                results.append("✗ Unicode round-trip: text mismatch")
            }
        } catch {
            results.append("✗ Unicode round-trip: \(error)")
        }

        // Test 3: Audio reference without text
        do {
            let capture = try createCapture(
                captureId: "test-3",
                audioReference: "audio-ref-123.m4a",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.audioReference == "audio-ref-123.m4a" && capture.text == nil {
                results.append("✓ Audio-only capture")
            } else {
                results.append("✗ Audio-only capture: mismatch")
            }
        } catch {
            results.append("✗ Audio-only capture: \(error)")
        }

        // Test 4: Entry locked flag
        do {
            let capture = try createCapture(
                captureId: "test-4",
                text: "Secret",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "private",
                routeId: "local",
                entryLocked: true,
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.entryLocked && capture.text == "Secret" {
                results.append("✓ Locked entry flag")
            } else {
                results.append("✗ Locked entry flag: mismatch")
            }
        } catch {
            results.append("✗ Locked entry flag: \(error)")
        }

        // Test 5: Session topic
        do {
            let capture = try createCapture(
                captureId: "test-5",
                text: "Therapy note",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z",
                sessionTopic: "therapy"
            )

            if capture.sessionTopic == "therapy" {
                results.append("✓ Session topic")
            } else {
                results.append("✗ Session topic: mismatch")
            }
        } catch {
            results.append("✗ Session topic: \(error)")
        }

        // Test 6: Negative offset
        do {
            let capture = try createCapture(
                captureId: "test-6",
                text: "EST note",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "America/New_York",
                utcOffsetMinutes: -300,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.utcOffsetMinutes == -300 {
                results.append("✓ Negative UTC offset")
            } else {
                results.append("✗ Negative UTC offset: mismatch")
            }
        } catch {
            results.append("✗ Negative UTC offset: \(error)")
        }

        // Test 7: Large text input (1MB)
        do {
            let largeText = String(repeating: "A", count: 1024 * 1024)
            let capture = try createCapture(
                captureId: "test-7",
                text: largeText,
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )

            if capture.text?.count == 1024 * 1024 {
                results.append("✓ Large text (1MB)")
            } else {
                results.append("✗ Large text (1MB): size mismatch")
            }
        } catch {
            results.append("✗ Large text (1MB): \(error)")
        }

        // Test 8: Missing text and audio should fail
        do {
            _ = try createCapture(
                captureId: "test-8",
                captureInstant: "2024-01-01T12:00:00Z",
                timezoneId: "UTC",
                utcOffsetMinutes: 0,
                locale: "en-US",
                calendar: "gregorian",
                itemScope: "personal",
                routeId: "local",
                createdAt: "2024-01-01T12:00:00Z"
            )
            results.append("✗ Validation: should reject missing text/audio")
        } catch {
            results.append("✓ Validation: correctly rejects missing text/audio")
        }

        statusLabel?.text = results.joined(separator: "\n")
    }
}
