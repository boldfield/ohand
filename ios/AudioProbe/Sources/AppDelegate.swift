import UIKit
import AVFoundation

@main
class AudioProbeDelegateAdapter: UIResponder, UIApplicationDelegate {
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
        configuration.delegateClass = AudioProbeSceneDelegate.self
        return configuration
    }
}

class AudioProbeSceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(
        _ scene: UIScene,
        willConnectTo session: UISceneSession,
        options connectionOptions: UIScene.ConnectionOptions
    ) {
        guard let windowScene = (scene as? UIWindowScene) else { return }
        let window = UIWindow(windowScene: windowScene)
        let rootViewController = AudioProbeViewController()
        window.rootViewController = rootViewController
        self.window = window
        window.makeKeyAndVisible()
    }
}

class AudioProbeViewController: UIViewController {
    private let recorder = AudioRecorder()
    private var resultLabel: UILabel?
    private var recordingDurationLabel: UILabel?
    private var recordingTimer: Timer?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground

        let scrollView = UIScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scrollView)

        let container = UIStackView()
        container.axis = .vertical
        container.spacing = 12
        container.alignment = .fill
        container.distribution = .fillProportionally
        container.translatesAutoresizingMaskIntoConstraints = false
        scrollView.addSubview(container)

        let titleLabel = UILabel()
        titleLabel.text = "Audio Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "Recording interruption and partial-audio recovery"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        recordingDurationLabel = UILabel()
        recordingDurationLabel?.text = "Duration: 0.0s"
        recordingDurationLabel?.font = UIFont.systemFont(ofSize: 12, weight: .light)
        recordingDurationLabel?.textColor = .tertiaryLabel
        recordingDurationLabel?.textAlignment = .center
        container.addArrangedSubview(recordingDurationLabel!)

        let buttonContainer = UIStackView()
        buttonContainer.axis = .horizontal
        buttonContainer.spacing = 8
        buttonContainer.distribution = .fillEqually
        buttonContainer.translatesAutoresizingMaskIntoConstraints = false
        container.addArrangedSubview(buttonContainer)

        let startButton = UIButton(type: .system)
        startButton.setTitle("Start Recording", for: .normal)
        startButton.addTarget(self, action: #selector(startRecording), for: .touchUpInside)
        startButton.backgroundColor = .systemBlue
        startButton.setTitleColor(.white, for: .normal)
        startButton.layer.cornerRadius = 8
        startButton.translatesAutoresizingMaskIntoConstraints = false
        startButton.heightAnchor.constraint(equalToConstant: 44).isActive = true
        buttonContainer.addArrangedSubview(startButton)

        let stopButton = UIButton(type: .system)
        stopButton.setTitle("Stop Recording", for: .normal)
        stopButton.addTarget(self, action: #selector(stopRecording), for: .touchUpInside)
        stopButton.backgroundColor = .systemGreen
        stopButton.setTitleColor(.white, for: .normal)
        stopButton.layer.cornerRadius = 8
        stopButton.translatesAutoresizingMaskIntoConstraints = false
        stopButton.heightAnchor.constraint(equalToConstant: 44).isActive = true
        buttonContainer.addArrangedSubview(stopButton)

        let cancelButton = UIButton(type: .system)
        cancelButton.setTitle("Cancel", for: .normal)
        cancelButton.addTarget(self, action: #selector(cancelRecording), for: .touchUpInside)
        cancelButton.backgroundColor = .systemRed
        cancelButton.setTitleColor(.white, for: .normal)
        cancelButton.layer.cornerRadius = 8
        cancelButton.translatesAutoresizingMaskIntoConstraints = false
        cancelButton.heightAnchor.constraint(equalToConstant: 44).isActive = true
        buttonContainer.addArrangedSubview(cancelButton)

        resultLabel = UILabel()
        resultLabel?.text = "Ready"
        resultLabel?.font = UIFont.systemFont(ofSize: 12, weight: .regular)
        resultLabel?.numberOfLines = 0
        resultLabel?.textColor = .label
        resultLabel?.textAlignment = .center
        container.addArrangedSubview(resultLabel!)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: view.topAnchor),
            scrollView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: view.bottomAnchor),

            container.topAnchor.constraint(equalTo: scrollView.topAnchor, constant: 20),
            container.leadingAnchor.constraint(equalTo: scrollView.leadingAnchor, constant: 20),
            container.trailingAnchor.constraint(equalTo: scrollView.trailingAnchor, constant: -20),
            container.bottomAnchor.constraint(lessThanOrEqualTo: scrollView.bottomAnchor, constant: -20),
            container.widthAnchor.constraint(equalTo: scrollView.widthAnchor, constant: -40)
        ])

        requestMicrophonePermission()
    }

    private func requestMicrophonePermission() {
        if #available(iOS 17, *) {
            AVAudioApplication.requestRecordPermission { granted in
                DispatchQueue.main.async {
                    if !granted {
                        self.resultLabel?.text = "Microphone permission denied"
                        self.resultLabel?.textColor = .systemRed
                    }
                }
            }
        } else {
            AVAudioSession.sharedInstance().requestRecordPermission { granted in
                DispatchQueue.main.async {
                    if !granted {
                        self.resultLabel?.text = "Microphone permission denied"
                        self.resultLabel?.textColor = .systemRed
                    }
                }
            }
        }
    }

    @objc private func startRecording() {
        let documentsPath = NSSearchPathForDirectoriesInDomains(.documentDirectory, .userDomainMask, true)[0]
        let recordingFilePath = documentsPath + "/recording-" + UUID().uuidString + ".wav"
        let recordingURL = URL(fileURLWithPath: recordingFilePath)

        if recorder.startRecording(to: recordingURL) {
            resultLabel?.text = "Recording..."
            resultLabel?.textColor = .systemBlue
            recordingDurationLabel?.textColor = .systemBlue

            recordingTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
                guard let self = self, let startTime = self.recorder.recordingStartTime else { return }
                let duration = Date().timeIntervalSince(startTime)
                self.recordingDurationLabel?.text = String(format: "Duration: %.1fs", duration)
            }
        } else {
            resultLabel?.text = "Failed to start recording"
            resultLabel?.textColor = .systemRed
        }
    }

    @objc private func stopRecording() {
        recordingTimer?.invalidate()
        recordingTimer = nil

        let result = recorder.stopRecording()
        recordingDurationLabel?.textColor = .tertiaryLabel
        resultLabel?.text = result.description
        resultLabel?.textColor = result.success ? .systemGreen : .systemOrange
    }

    @objc private func cancelRecording() {
        recordingTimer?.invalidate()
        recordingTimer = nil

        let result = recorder.cancelRecording()
        recordingDurationLabel?.textColor = .tertiaryLabel
        resultLabel?.text = result.description
        resultLabel?.textColor = .systemOrange
    }
}
