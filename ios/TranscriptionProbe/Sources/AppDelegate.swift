import UIKit
import Speech
import AVFoundation

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
    private var audioEngine = AVAudioEngine()
    private let speechRecognitionQueue = DispatchQueue(label: "com.boldfield.transcription.queue")
    private var transcriptionStartTime: Date?

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

        if recognizer == nil {
            statusLabel?.text = "Speech recognizer not available"
            statusLabel?.textColor = .systemRed
        } else if !SFSpeechRecognizer.authorizationStatus().isAvailable {
            statusLabel?.text = "Speech recognition not available on this device"
            statusLabel?.textColor = .systemOrange
        } else {
            statusLabel?.text = "Recognizer ready"
            statusLabel?.textColor = .systemGreen
        }
    }

    private func requestSpeechRecognitionPermission() {
        SFSpeechRecognizer.requestAuthorization { [weak self] status in
            DispatchQueue.main.async {
                switch status {
                case .authorized:
                    self?.statusLabel?.text = "Speech recognition authorized"
                    self?.statusLabel?.textColor = .systemGreen
                case .denied:
                    self?.statusLabel?.text = "Speech recognition permission denied"
                    self?.statusLabel?.textColor = .systemRed
                case .restricted:
                    self?.statusLabel?.text = "Speech recognition restricted"
                    self?.statusLabel?.textColor = .systemOrange
                case .notDetermined:
                    self?.statusLabel?.text = "Speech recognition not yet determined"
                    self?.statusLabel?.textColor = .systemOrange
                @unknown default:
                    self?.statusLabel?.text = "Unknown authorization status"
                    self?.statusLabel?.textColor = .systemOrange
                }
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

        guard let recognizer = recognizer, recognizer.isAvailable else {
            resultLabel?.text = "Recognizer not available"
            resultLabel?.textColor = .systemRed
            return
        }

        resultLabel?.text = "Transcribing..."
        resultLabel?.textColor = .systemBlue
        transcriptionStartTime = Date()

        speechRecognitionQueue.async { [weak self] in
            let request = SFSpeechURLRecognitionRequest(url: audioURL)
            request.shouldReportPartialResults = true

            self?.recognitionTask = recognizer.recognitionTask(with: request) { [weak self] result, error in
                guard let self = self else { return }

                let isFinal = result?.isFinal ?? false

                DispatchQueue.main.async {
                    if let error = error {
                        self.handleTranscriptionError(error)
                    } else if let result = result {
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
        }
    }

    @objc private func cancelTranscription() {
        recognitionTask?.cancel()
        recognitionTask = nil
        transcriptionDurationLabel?.text = "Duration: —"
        resultLabel?.text = "Transcription cancelled"
        resultLabel?.textColor = .systemOrange
    }

    private func handleTranscriptionError(_ error: Error) {
        let duration = Date().timeIntervalSince(transcriptionStartTime ?? Date())
        transcriptionDurationLabel?.text = String(format: "Duration: %.2fs", duration)

        let errorText: String
        if let sfError = error as? NSError {
            switch sfError.code {
            case SFSpeechRecognizer.Error.availabilityNotDetermined.rawValue:
                errorText = "Speech recognition availability not determined"
            case SFSpeechRecognizer.Error.requestTimedOut.rawValue:
                errorText = "Request timed out"
            case SFSpeechRecognizer.Error.audioEngineFailed.rawValue:
                errorText = "Audio engine failed"
            case SFSpeechRecognizer.Error.speechRecognizerNotAvailable.rawValue:
                errorText = "Speech recognizer not available (may need model download)"
            default:
                errorText = "Error: \(sfError.localizedDescription)"
            }
        } else {
            errorText = "Error: \(error.localizedDescription)"
        }

        resultLabel?.text = errorText
        resultLabel?.textColor = .systemRed
    }
}
