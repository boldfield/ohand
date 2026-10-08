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
    private let captureId: String

    override init(nibName nibNameOrNil: String?, bundle nibBundleOrNil: Bundle?) {
        captureId = Self.loadOrCreateCaptureId()
        super.init(nibName: nibNameOrNil, bundle: nibBundleOrNil)
    }

    required init?(coder: NSCoder) {
        captureId = Self.loadOrCreateCaptureId()
        super.init(coder: coder)
    }

    private static let captureIdKey = "com.boldfield.ohand.probes.capture.id"

    private static func loadOrCreateCaptureId() -> String {
        if let existing = UserDefaults.standard.string(forKey: captureIdKey) {
            return existing
        }
        let newId = UUID().uuidString
        UserDefaults.standard.set(newId, forKey: captureIdKey)
        return newId
    }

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
        Task {
            do {
                let store = try ProbeStore()

                let now = ISO8601DateFormatter().string(from: Date())
                let calendar = Calendar.current
                let timeZone = TimeZone.current
                let locale = Locale.current
                let isLocked = !UIApplication.shared.isProtectedDataAvailable
                let calendarString = "\(calendar.identifier)"

                let record = CaptureRecord(
                    captureId: captureId,
                    text: "Capture probe ingress entry",
                    audioReference: nil,
                    captureInstant: now,
                    timezoneId: timeZone.identifier,
                    utcOffsetMinutes: Int32(timeZone.secondsFromGMT() / 60),
                    locale: locale.identifier,
                    calendar: calendarString,
                    itemScope: "personal",
                    routeId: "route-local",
                    entryLocked: isLocked,
                    createdAt: now,
                    sessionTopic: nil
                )

                let result = try store.save(record)

                DispatchQueue.main.async {
                    self.ingressLabel.text = """
                    Capture ID: \(self.captureId)
                    Entry Time: \(now)
                    Locked: \(isLocked)
                    Idempotent: \(result.idempotentReplay)
                    """
                    self.statusLabel.text = "Ingress record persisted"
                }
            } catch let error as BoundaryFailure {
                DispatchQueue.main.async {
                    self.ingressLabel.text = "Error: \(error.code)"
                    self.statusLabel.text = "Failed to persist"
                }
            } catch {
                DispatchQueue.main.async {
                    self.ingressLabel.text = "Unexpected error"
                    self.statusLabel.text = "Failed to persist"
                }
            }
        }
    }
}
