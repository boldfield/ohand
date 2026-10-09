import UIKit

@main
class TranscriptionProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
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
        configuration.delegateClass = TranscriptionProbeSceneDelegate.self
        return configuration
    }
}

class TranscriptionProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = TranscriptionProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class TranscriptionProbeViewController: UIViewController {
    // "zz-ZZ" is deliberately not a real locale so the unsupported-language path can be exercised on any device.
    private static let probeLocales = ["en-US", "fr-FR", "zz-ZZ"]

    private let engine = SpeechFrameworkEngine()
    private lazy var coordinator = TranscriptionCoordinator(
        engine: engine,
        localeIdentifier: TranscriptionProbeViewController.probeLocales[0]
    )
    private let fixtureGenerator = SyntheticSpeechFixtureGenerator()
    private let statusLabel = UILabel()
    private let audioLabel = UILabel()
    private var notificationObservers: [NSObjectProtocol] = []

    private var documentsDirectory: URL {
        return FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 12
        container.alignment = .fill
        container.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = UILabel()
        titleLabel.text = "Transcription Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "On-device only. No cloud fallback: when on-device recognition is unavailable the audio is kept and the transcript stays pending."
        descriptionLabel.font = UIFont.systemFont(ofSize: 13, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        let localeControl = UISegmentedControl(items: TranscriptionProbeViewController.probeLocales)
        localeControl.selectedSegmentIndex = 0
        localeControl.accessibilityIdentifier = "localeControl"
        localeControl.addAction(UIAction { [weak self, weak localeControl] _ in
            guard let self = self, let localeControl = localeControl else { return }
            let selected = TranscriptionProbeViewController.probeLocales[localeControl.selectedSegmentIndex]
            if !self.coordinator.setLocale(selected) {
                localeControl.selectedSegmentIndex = TranscriptionProbeViewController.probeLocales
                    .firstIndex(of: self.coordinator.localeIdentifier) ?? 0
            }
        }, for: .valueChanged)
        container.addArrangedSubview(localeControl)

        audioLabel.font = UIFont.systemFont(ofSize: 12, weight: .regular)
        audioLabel.numberOfLines = 0
        audioLabel.textAlignment = .center
        audioLabel.textColor = .secondaryLabel
        audioLabel.accessibilityIdentifier = "audioLabel"
        container.addArrangedSubview(audioLabel)

        statusLabel.font = UIFont.systemFont(ofSize: 14, weight: .medium)
        statusLabel.numberOfLines = 0
        statusLabel.textAlignment = .center
        statusLabel.accessibilityIdentifier = "statusLabel"
        container.addArrangedSubview(statusLabel)

        let actions: [(title: String, identifier: String, action: () -> Void)] = [
            ("Generate Synthetic Fixture", "generateFixtureButton", { [weak self] in self?.generateFixture() }),
            ("Load Fixture", "loadFixtureButton", { [weak self] in self?.loadFixture() }),
            ("Request Permission", "requestPermissionButton", { [weak self] in self?.coordinator.requestPermission() }),
            ("Transcribe On Device", "transcribeButton", { [weak self] in self?.coordinator.startTranscription() }),
            ("Cancel", "cancelButton", { [weak self] in self?.coordinator.cancelTranscription() })
        ]
        for entry in actions {
            let button = UIButton(type: .system)
            button.setTitle(entry.title, for: .normal)
            button.accessibilityIdentifier = entry.identifier
            button.backgroundColor = .systemBlue
            button.setTitleColor(.white, for: .normal)
            button.layer.cornerRadius = 8
            button.heightAnchor.constraint(equalToConstant: 44).isActive = true
            let handler = entry.action
            button.addAction(UIAction { _ in handler() }, for: .touchUpInside)
            container.addArrangedSubview(button)
        }

        let scrollView = UIScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.addSubview(container)
        view.addSubview(scrollView)
        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            container.topAnchor.constraint(equalTo: scrollView.topAnchor, constant: 20),
            container.leadingAnchor.constraint(equalTo: scrollView.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(equalTo: scrollView.trailingAnchor, constant: -20),
            container.bottomAnchor.constraint(equalTo: scrollView.bottomAnchor, constant: -20),
            container.widthAnchor.constraint(equalTo: scrollView.widthAnchor, constant: -40)
        ])

        coordinator.onChange = { [weak self] in self?.render() }
        engine.onAvailabilityChange = { [weak self] in self?.coordinator.refreshAfterEnvironmentChange() }
        observeInterruptions()
        render()
    }

    private func observeInterruptions() {
        let center = NotificationCenter.default
        notificationObservers.append(center.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.coordinator.noteInterruption(.applicationBackgrounded)
        })
        notificationObservers.append(center.addObserver(
            forName: UIApplication.protectedDataWillBecomeUnavailableNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.coordinator.noteInterruption(.protectedDataUnavailable)
        })
        notificationObservers.append(center.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
            // Permission can be revoked in Settings while the app is backgrounded.
            self?.coordinator.refreshAfterEnvironmentChange()
        })
    }

    private func render() {
        statusLabel.text = coordinator.displayText
        switch coordinator.status {
        case .transcribed:
            statusLabel.textColor = .systemGreen
        case .pending, .failed:
            statusLabel.textColor = .systemOrange
        case .cancelled:
            statusLabel.textColor = .secondaryLabel
        case .noAudio, .ready, .inProgress:
            statusLabel.textColor = .label
        }
        if let audioURL = coordinator.audioURL {
            audioLabel.text = "Audio: \(audioURL.lastPathComponent) | locale \(coordinator.localeIdentifier)"
        } else {
            audioLabel.text = "Audio: none | locale \(coordinator.localeIdentifier)"
        }
    }

    private func generateFixture() {
        statusLabel.text = "Generating synthetic speech fixture..."
        let destination = TranscriptionFixture.generatedURL(in: documentsDirectory)
        fixtureGenerator.generate(to: destination) { [weak self] failure in
            guard let self = self else { return }
            if let failure = failure {
                self.statusLabel.text = "Fixture generation failed: \(failure)"
                self.statusLabel.textColor = .systemOrange
            } else {
                self.loadFixture()
            }
        }
    }

    private func loadFixture() {
        guard let fixtureURL = TranscriptionFixture.locate(in: documentsDirectory) else {
            statusLabel.text = "No fixture found. Generate one, or place \(TranscriptionFixture.baseName).wav in Documents."
            statusLabel.textColor = .systemOrange
            return
        }
        if let rejection = coordinator.loadAudio(at: fixtureURL) {
            statusLabel.text = "Load rejected: \(rejection)"
            statusLabel.textColor = .systemOrange
        }
    }

    deinit {
        notificationObservers.forEach { NotificationCenter.default.removeObserver($0) }
    }
}
