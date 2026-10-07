import UIKit
import UserNotifications

@main
class NotificationProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
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
        let rootViewController = UINavigationController(rootViewController: NotificationProbeViewController())
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class NotificationProbeViewController: UIViewController {
    private let notificationCenter = UNUserNotificationCenter.current()
    private var statusLabel = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        title = "Notification Probe"
        navigationController?.navigationBar.prefersLargeTitles = true

        let scrollView = UIScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scrollView)

        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 12
        container.alignment = .fill
        container.distribution = .fillEqually
        container.translatesAutoresizingMaskIntoConstraints = false
        scrollView.addSubview(container)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            container.topAnchor.constraint(equalTo: scrollView.topAnchor, constant: 16),
            container.bottomAnchor.constraint(equalTo: scrollView.bottomAnchor, constant: -16),
            container.leadingAnchor.constraint(equalTo: scrollView.leadingAnchor, constant: 16),
            container.trailingAnchor.constraint(equalTo: scrollView.trailingAnchor, constant: -16),
            container.widthAnchor.constraint(equalTo: scrollView.widthAnchor, constant: -32)
        ])

        // Title and description
        let titleLabel = UILabel()
        titleLabel.text = "Notification Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 20, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "Test native notification scheduling, permissions, limits, and platform behavior"
        descriptionLabel.font = UIFont.systemFont(ofSize: 12, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        // Status label
        statusLabel.text = "No test executed yet"
        statusLabel.font = UIFont.systemFont(ofSize: 11, weight: .light)
        statusLabel.numberOfLines = 0
        statusLabel.textAlignment = .center
        statusLabel.textColor = .tertiaryLabel
        container.addArrangedSubview(statusLabel)

        // Permission test
        container.addArrangedSubview(createButton(title: "Check/Request Permissions", action: #selector(checkPermissions)))

        // Schedule immediate notification
        container.addArrangedSubview(createButton(title: "Schedule Immediate (5s)", action: #selector(scheduleImmediate)))

        // Schedule future notification
        container.addArrangedSubview(createButton(title: "Schedule Future (1 min)", action: #selector(scheduleFuture)))

        // Test duplicate identifiers
        container.addArrangedSubview(createButton(title: "Test Duplicate ID", action: #selector(testDuplicateId)))

        // Test timezone behavior
        container.addArrangedSubview(createButton(title: "Test Timezone Behavior", action: #selector(testTimezoneBehavior)))

        // List pending notifications
        container.addArrangedSubview(createButton(title: "List Pending", action: #selector(listPending)))

        // Check capacity
        container.addArrangedSubview(createButton(title: "Test Capacity (Schedule 100)", action: #selector(testCapacity)))

        // Cancel all
        container.addArrangedSubview(createButton(title: "Cancel All", action: #selector(cancelAll)))

        // Schedule batch to measure limits
        container.addArrangedSubview(createButton(title: "Schedule Batch (Max Limit)", action: #selector(scheduleBatch)))
    }

    private func createButton(title: String, action: Selector) -> UIButton {
        let button = UIButton(type: .system)
        button.setTitle(title, for: .normal)
        button.addTarget(self, action: action, for: .touchUpInside)
        button.backgroundColor = .systemBlue
        button.setTitleColor(.white, for: .normal)
        button.layer.cornerRadius = 8
        button.translatesAutoresizingMaskIntoConstraints = false
        button.heightAnchor.constraint(equalToConstant: 44).isActive = true
        return button
    }

    @objc private func checkPermissions() {
        notificationCenter.getNotificationSettings { [weak self] settings in
            DispatchQueue.main.async {
                let status = self?.formatSettings(settings) ?? "Error"
                self?.statusLabel.text = "Permission Status:\n\(status)"

                if settings.authorizationStatus == .notDetermined {
                    self?.notificationCenter.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
                        DispatchQueue.main.async {
                            let result = granted ? "Authorized" : "Denied"
                            self?.statusLabel.text = "Auth request result: \(result)\nError: \(error?.localizedDescription ?? "none")"
                        }
                    }
                }
            }
        }
    }

    @objc private func scheduleImmediate() {
        let content = UNMutableNotificationContent()
        content.title = "Immediate Test"
        content.body = "Scheduled immediately, should appear in 5 seconds"
        content.sound = .default

        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 5, repeats: false)
        let request = UNNotificationRequest(identifier: "immediate-\(Date().timeIntervalSince1970)", content: content, trigger: trigger)

        notificationCenter.add(request) { [weak self] error in
            DispatchQueue.main.async {
                if let error = error {
                    self?.statusLabel.text = "Error scheduling immediate: \(error.localizedDescription)"
                } else {
                    self?.statusLabel.text = "Scheduled immediate notification (5s delay)"
                }
            }
        }
    }

    @objc private func scheduleFuture() {
        let content = UNMutableNotificationContent()
        content.title = "Future Test"
        content.body = "Scheduled for 1 minute from now"
        content.sound = .default

        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60, repeats: false)
        let request = UNNotificationRequest(identifier: "future-\(Date().timeIntervalSince1970)", content: content, trigger: trigger)

        notificationCenter.add(request) { [weak self] error in
            DispatchQueue.main.async {
                if let error = error {
                    self?.statusLabel.text = "Error scheduling future: \(error.localizedDescription)"
                } else {
                    self?.statusLabel.text = "Scheduled notification for 1 minute from now"
                }
            }
        }
    }

    @objc private func testDuplicateId() {
        let content1 = UNMutableNotificationContent()
        content1.title = "Duplicate ID Test (First)"
        content1.body = "First request with duplicate ID"

        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 5, repeats: false)
        let request1 = UNNotificationRequest(identifier: "duplicate-test", content: content1, trigger: trigger)

        notificationCenter.add(request1) { [weak self] error in
            if error != nil {
                DispatchQueue.main.async {
                    self?.statusLabel.text = "First request failed"
                }
                return
            }

            let content2 = UNMutableNotificationContent()
            content2.title = "Duplicate ID Test (Second)"
            content2.body = "Second request with same ID (should replace first)"

            let request2 = UNNotificationRequest(identifier: "duplicate-test", content: content2, trigger: trigger)

            self?.notificationCenter.add(request2) { error in
                DispatchQueue.main.async {
                    if let error = error {
                        self?.statusLabel.text = "Duplicate ID causes error: \(error.localizedDescription)"
                    } else {
                        self?.statusLabel.text = "Duplicate ID: Second request accepted (replaces first)"
                    }
                }
            }
        }
    }

    @objc private func testTimezoneBehavior() {
        var calendar = Calendar.current
        let timezone = TimeZone.current

        var components = DateComponents()
        components.minute = 1
        if let date = calendar.date(byAdding: components, to: Date()) {
            let trigger = UNCalendarNotificationTrigger(dateMatching: calendar.dateComponents([.year, .month, .day, .hour, .minute], from: date), repeats: false)

            let content = UNMutableNotificationContent()
            content.title = "Timezone Test"
            content.body = "TZ: \(timezone.abbreviation() ?? "unknown"), current offset: \(timezone.secondsFromGMT() / 3600)h"

            let request = UNNotificationRequest(identifier: "tz-test-\(Date().timeIntervalSince1970)", content: content, trigger: trigger)

            notificationCenter.add(request) { [weak self] error in
                DispatchQueue.main.async {
                    if let error = error {
                        self?.statusLabel.text = "Timezone test error: \(error.localizedDescription)"
                    } else {
                        self?.statusLabel.text = "Scheduled timezone-aware notification"
                    }
                }
            }
        }
    }

    @objc private func listPending() {
        notificationCenter.getPendingNotificationRequests { [weak self] requests in
            DispatchQueue.main.async {
                let summary = "Pending: \(requests.count) notifications\n"
                let details = requests.prefix(5).map { r in
                    "\(r.identifier): \(r.content.title)"
                }.joined(separator: "\n")

                self?.statusLabel.text = summary + (requests.count > 5 ? "...\n" : "") + details
            }
        }
    }

    @objc private func testCapacity() {
        var added = 0

        for i in 0..<100 {
            let content = UNMutableNotificationContent()
            content.title = "Capacity Test \(i)"
            content.body = "Testing pending request capacity"

            let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 60 + Double(i), repeats: false)
            let request = UNNotificationRequest(identifier: "capacity-\(i)", content: content, trigger: trigger)

            notificationCenter.add(request) { error in
                if error == nil {
                    added += 1
                }
            }
        }

        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            self?.notificationCenter.getPendingNotificationRequests { requests in
                DispatchQueue.main.async {
                    self?.statusLabel.text = "Attempted 100, got \(requests.count) total pending"
                }
            }
        }
    }

    @objc private func cancelAll() {
        notificationCenter.removeAllPendingNotificationRequests()
        statusLabel.text = "Cancelled all pending notifications"
    }

    @objc private func scheduleBatch() {
        notificationCenter.getPendingNotificationRequests { [weak self] existing in
            let slots = max(0, 64 - existing.count)

            for i in 0..<slots {
                let content = UNMutableNotificationContent()
                content.title = "Batch \(i)"
                content.body = "Testing max capacity"

                let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 300 + Double(i * 10), repeats: false)
                let request = UNNotificationRequest(identifier: "batch-\(Date().timeIntervalSince1970)-\(i)", content: content, trigger: trigger)

                self?.notificationCenter.add(request) { _ in }
            }

            DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
                self?.notificationCenter.getPendingNotificationRequests { requests in
                    DispatchQueue.main.async {
                        self?.statusLabel.text = "Scheduled \(slots) notifications\nTotal pending: \(requests.count)"
                    }
                }
            }
        }
    }

    private func formatSettings(_ settings: UNNotificationSettings) -> String {
        let authStatus = String(describing: settings.authorizationStatus).split(separator: ".").last ?? "unknown"
        let alertStatus = String(describing: settings.alertSetting).split(separator: ".").last ?? "unknown"
        let soundStatus = String(describing: settings.soundSetting).split(separator: ".").last ?? "unknown"
        let badgeStatus = String(describing: settings.badgeSetting).split(separator: ".").last ?? "unknown"

        return "Auth: \(authStatus)\nAlert: \(alertStatus)\nSound: \(soundStatus)\nBadge: \(badgeStatus)"
    }
}
