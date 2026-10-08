import UIKit
import UserNotifications
import os

@main
class NotificationProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = ForegroundPresentationRecorder.shared
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
        configuration.delegateClass = NotificationProbeSceneDelegate.self
        return configuration
    }
}

class NotificationProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = NotificationProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class NotificationProbeViewController: UIViewController {
    static let reportFileName = "notification-probe-report.jsonl"

    private let scenarios = NotificationProbeScenarios()
    private let resultLabel = UILabel()
    private let logger = Logger(subsystem: "com.boldfield.ohand.probes.notification", category: "report")
    private var runCounter = 0

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 6
        container.alignment = .center
        container.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = UILabel()
        titleLabel.text = "Notification Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 20, weight: .bold)
        container.addArrangedSubview(titleLabel)

        for scenario in ProbeScenario.allCases {
            let button = UIButton(type: .system)
            button.setTitle(scenario.rawValue, for: .normal)
            button.accessibilityIdentifier = "probe.run.\(scenario.rawValue)"
            button.addAction(UIAction { [weak self] _ in self?.run(scenario) }, for: .touchUpInside)
            container.addArrangedSubview(button)
        }

        resultLabel.text = "no scenario run yet"
        resultLabel.accessibilityLabel = "no scenario run yet"
        resultLabel.accessibilityIdentifier = "probe.lastResult"
        resultLabel.font = UIFont.monospacedSystemFont(ofSize: 9, weight: .regular)
        resultLabel.numberOfLines = 6
        resultLabel.lineBreakMode = .byTruncatingTail
        resultLabel.textColor = .secondaryLabel
        container.addArrangedSubview(resultLabel)

        view.addSubview(container)
        NSLayoutConstraint.activate([
            container.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            container.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            container.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            container.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12)
        ])
    }

    private func run(_ scenario: ProbeScenario) {
        Task { @MainActor in
            var result: [String: String] = [:]
            do {
                result = try await scenarios.run(scenario)
            } catch {
                let nsError = error as NSError
                result["error"] = "\(nsError.domain)#\(nsError.code)"
            }
            runCounter += 1
            result["scenario"] = scenario.rawValue
            result["run"] = String(runCounter)
            publish(result)
        }
    }

    private func publish(_ result: [String: String]) {
        guard
            let data = try? JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .withoutEscapingSlashes]),
            let json = String(data: data, encoding: .utf8)
        else { return }
        resultLabel.text = json
        resultLabel.accessibilityLabel = json
        logger.info("\(json, privacy: .public)")
        appendToReportFile(json)
    }

    private func appendToReportFile(_ json: String) {
        guard let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let reportURL = documents.appendingPathComponent(Self.reportFileName)
        let line = Data((json + "\n").utf8)
        if let handle = try? FileHandle(forWritingTo: reportURL) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: line)
        } else {
            try? line.write(to: reportURL)
        }
    }
}
