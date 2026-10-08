import UIKit
import Speech

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
    private var resultLabel: UILabel?
    private var statusLabel: UILabel?
    private var transcriptionDurationLabel: UILabel?
    private var recognizer: SFSpeechRecognizer?
    private var recognitionTask: SFSpeechRecognitionTask?
    private let speechRecognitionQueue = DispatchQueue(label: "com.boldfield.transcription.queue")
    private var transcriptionStartTime: Date?
    private var authorizationStatus: SFSpeechRecognizerAuthorizationStatus = .notDetermined
    private var recognizerAvailable: Bool = false
    private var supportsOnDevice: Bool = false

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
        titleLabel.text = "Transcription Probe"
        titleLabel.font = UIFont.systemFont(ofSize: 24, weight: .bold)
        titleLabel.textAlignment = .center
        container.addArrangedSubview(titleLabel)

        let descriptionLabel = UILabel()
        descriptionLabel.text = "On-device transcription availability\nLanguage/model availability and fallback handling"
        descriptionLabel.font = UIFont.systemFont(ofSize: 14, weight: .regular)
        descriptionLabel.numberOfLines = 0
        descriptionLabel.textAlignment = .center
        descriptionLabel.textColor = .secondaryLabel
        container.addArrangedSubview(descriptionLabel)

        statusLabel = UILabel()
        statusLabel?.text = "Initializing..."
        statusLabel?.font = UIFont.systemFont(ofSize: 12, weight: .light)
        statusLabel?.textColor = .tertiaryLabel
        statusLabel?.textAlignment = .center
        container.addArrangedSubview(statusLabel!)

        transcriptionDurationLabel = UILabel()
        transcriptionDurationLabel?.text = "Duration: —"
        transcriptionDurationLabel?.font = UIFont.systemFont(ofSize: 12, weight: .light)
        transcriptionDurationLabel?.textColor = .tertiaryLabel
        transcriptionDurationLabel?.textAlignment = .center
        container.addArrangedSubview(transcriptionDurationLabel!)

        let buttonContainer = UIStackView()
        buttonContainer.axis = .horizontal
        buttonContainer.spacing = 8
        buttonContainer.distribution = .fillEqually
        buttonContainer.translatesAutoresizingMaskIntoConstraints = false
        container.addArrangedSubview(buttonContainer)

        let loadButton = UIButton(type: .system)
        loadButton.setTitle("Load Test Audio", for: .normal)
        loadButton.addTarget(self, action: #selector(loadTestAudio), for: .touchUpInside)
        loadButton.backgroundColor = .systemBlue
        loadButton.setTitleColor(.white, for: .normal)
        loadButton.layer.cornerRadius = 8
        loadButton.translatesAutoresizingMaskIntoConstraints = false
        loadButton.heightAnchor.constraint(equalToConstant: 44).isActive = true
        buttonContainer.addArrangedSubview(loadButton)

        let transcribeButton = UIButton(type: .system)
        transcribeButton.setTitle("Transcribe", for: .normal)
        transcribeButton.addTarget(self, action: #selector(transcribeAudio), for: .touchUpInside)
        transcribeButton.backgroundColor = .systemGreen
        transcribeButton.setTitleColor(.white, for: .normal)
        transcribeButton.layer.cornerRadius = 8
        transcribeButton.translatesAutoresizingMaskIntoConstraints = false
        transcribeButton.heightAnchor.constraint(equalToConstant: 44).isActive = true
        buttonContainer.addArrangedSubview(transcribeButton)

        let cancelButton = UIButton(type: .system)
        cancelButton.setTitle("Cancel", for: .normal)
        cancelButton.addTarget(self, action: #selector(cancelTranscription), for: .touchUpInside)
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

        initializeRecognizer()
        requestSpeechRecognitionPermission()
    }

    private func initializeRecognizer() {
        recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US"))

        guard let recognizer = recognizer else {
            updateStatus("Speech recognizer initialization failed", color: .systemRed)
            recognizerAvailable = false
            supportsOnDevice = false
            return
        }

        recognizerAvailable = recognizer.isAvailable
        supportsOnDevice = recognizer.supportsOnDeviceRecognition

        updateRecognizerStatus()
    }

    private func updateRecognizerStatus() {
        let statusText: String
        let statusColor: UIColor

        if !recognizerAvailable {
            if supportsOnDevice {
                statusText = "Speech recognizer available but not ready (may need model download)"
                statusColor = .systemOrange
            } else {
                statusText = "On-device speech recognition not supported on this device"
                statusColor = .systemRed
            }
        } else if !supportsOnDevice {
            statusText = "On-device recognition unavailable (model may need download)"
            statusColor = .systemOrange
        } else {
            statusText = "Recognizer ready for on-device recognition"
            statusColor = .systemGreen
        }

        updateStatus(statusText, color: statusColor)
    }

    private func updateStatus(_ text: String, color: UIColor) {
        DispatchQueue.main.async { [weak self] in
            self?.statusLabel?.text = text
            self?.statusLabel?.textColor = color
        }
    }

    private func requestSpeechRecognitionPermission() {
        SFSpeechRecognizer.requestAuthorization { [weak self] status in
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.authorizationStatus = status

                guard status == .authorized else {
                    let statusText: String
                    let statusColor: UIColor

                    switch status {
                    case .authorized:
                        statusText = "Speech recognition authorized"
                        statusColor = .systemGreen
                    case .denied:
                        statusText = "Speech recognition permission denied"
                        statusColor = .systemRed
                    case .restricted:
                        statusText = "Speech recognition restricted"
                        statusColor = .systemOrange
                    case .notDetermined:
                        statusText = "Speech recognition not yet determined"
                        statusColor = .systemOrange
                    @unknown default:
                        statusText = "Unknown authorization status"
                        statusColor = .systemOrange
                    }

                    self.updateStatus(statusText, color: statusColor)
                    return
                }

                self.updateRecognizerStatus()
            }
        }
    }

    private var loadedAudioURL: URL?

    @objc private func loadTestAudio() {
        let documentsURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]

        let audioFileNames = try? FileManager.default.contentsOfDirectory(
            at: documentsURL,
            includingPropertiesForKeys: nil
        ).filter { $0.pathExtension.lowercased() == "wav" }

        if let audioFiles = audioFileNames, !audioFiles.isEmpty {
            loadedAudioURL = audioFiles.last
            resultLabel?.text = "Loaded: \(audioFiles.last?.lastPathComponent ?? "unknown")"
            resultLabel?.textColor = .systemGreen
        } else {
            resultLabel?.text = "No WAV files found in Documents"
            resultLabel?.textColor = .systemOrange
        }
    }

    @objc private func transcribeAudio() {
        guard let audioURL = loadedAudioURL else {
            resultLabel?.text = "No audio file loaded"
            resultLabel?.textColor = .systemOrange
            return
        }

        guard authorizationStatus == .authorized else {
            resultLabel?.text = "Speech recognition permission not authorized"
            resultLabel?.textColor = .systemRed
            return
        }

        guard let recognizer = recognizer, recognizerAvailable else {
            resultLabel?.text = "Recognizer not available"
            resultLabel?.textColor = .systemRed
            return
        }

        guard supportsOnDevice else {
            resultLabel?.text = "On-device transcription not supported (model may need download)"
            resultLabel?.textColor = .systemOrange
            return
        }

        resultLabel?.text = "Transcribing..."
        resultLabel?.textColor = .systemBlue
        transcriptionStartTime = Date()

        let request = SFSpeechURLRecognitionRequest(url: audioURL)
        request.shouldReportPartialResults = true
        request.requiresOnDeviceRecognition = true

        var taskAssigned = false
        let task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            guard let self = self else { return }

            DispatchQueue.main.async {
                if let error = error {
                    self.handleTranscriptionError(error)
                } else if let result = result {
                    let isFinal = result.isFinal
                    let transcript = result.bestTranscription.formattedString
                    let confidence = result.bestTranscription.segments.first?.confidence ?? 0

                    if isFinal {
                        let duration = Date().timeIntervalSince(self.transcriptionStartTime ?? Date())
                        self.transcriptionDurationLabel?.text = String(format: "Duration: %.2fs", duration)
                        self.resultLabel?.text = "Transcribed: \(transcript)\n(Confidence: \(String(format: "%.0f%%", confidence * 100)))"
                        self.resultLabel?.textColor = .systemGreen
                    } else {
                        self.resultLabel?.text = "Partial: \(transcript)"
                        self.resultLabel?.textColor = .systemBlue
                    }
                }
            }
        }

        speechRecognitionQueue.async { [weak self] in
            guard let self = self else { return }
            self.recognitionTask = task
        }
    }

    @objc private func cancelTranscription() {
        speechRecognitionQueue.async { [weak self] in
            guard let self = self else { return }
            self.recognitionTask?.cancel()
            self.recognitionTask = nil
        }

        transcriptionDurationLabel?.text = "Duration: —"
        resultLabel?.text = "Transcription cancelled"
        resultLabel?.textColor = .systemOrange
    }

    private func handleTranscriptionError(_ error: Error) {
        let duration = Date().timeIntervalSince(transcriptionStartTime ?? Date())
        transcriptionDurationLabel?.text = String(format: "Duration: %.2fs", duration)

        let errorText: String
        if #available(iOS 17, *) {
            if let sfError = error as? SFSpeechError {
                switch sfError.code {
                case .audioReadFailed:
                    errorText = "Failed to read audio file"
                case .timeout:
                    errorText = "Recognition request timed out"
                case .noModel:
                    errorText = "Speech recognition model not available"
                case .internalServiceError:
                    errorText = "Internal speech recognition error"
                @unknown default:
                    errorText = "Error: \(sfError.localizedDescription)"
                }
            } else {
                errorText = "Error: \(error.localizedDescription)"
            }
        } else {
            errorText = "Error: \(error.localizedDescription)"
        }

        resultLabel?.text = errorText
        resultLabel?.textColor = .systemRed
    }
}
