import AVFoundation

struct AudioRecordingResult {
    let success: Bool
    let durationSeconds: Double
    let filePath: String?
    let fileSize: Int
    let interruption: String?

    var description: String {
        if success {
            return "Saved: \(durationSeconds)s, \(fileSize) bytes"
        } else if let partial = filePath {
            return "Partial: \(durationSeconds)s, \(fileSize) bytes, \(interruption ?? "unknown")"
        } else {
            return "Failed: \(interruption ?? "no file")"
        }
    }
}

class AudioRecorder: NSObject, AVAudioRecorderDelegate {
    var recorder: AVAudioRecorder?
    var recordingStartTime: Date?
    var recordingDestination: URL?
    var lastInterruptionReason: String?

    override init() {
        super.init()
        setupAudioSession()
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleAudioInterruption(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleRouteChange(_:)),
            name: AVAudioSession.routeChangeNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    private func setupAudioSession() {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(
                .record,
                mode: .default,
                options: [.duckOthers]
            )
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            lastInterruptionReason = "Session setup failed: \(error)"
        }
    }

    func startRecording(to destination: URL) -> Bool {
        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatLinearPCM),
            AVSampleRateKey: 16000.0,
            AVNumberOfChannelsKey: 1,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsFloatKey: false,
            AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue
        ]

        do {
            let session = AVAudioSession.sharedInstance()
            if session.recordPermission == .denied {
                lastInterruptionReason = "Microphone permission denied"
                return false
            }

            recorder = try AVAudioRecorder(url: destination, settings: settings)
            recorder?.delegate = self
            recordingStartTime = Date()
            recordingDestination = destination
            lastInterruptionReason = nil

            return recorder?.record() ?? false
        } catch {
            lastInterruptionReason = "Record start failed: \(error)"
            return false
        }
    }

    func stopRecording() -> AudioRecordingResult {
        guard let recorder = recorder, let startTime = recordingStartTime else {
            return AudioRecordingResult(
                success: false,
                durationSeconds: 0,
                filePath: nil,
                fileSize: 0,
                interruption: lastInterruptionReason ?? "No active recording"
            )
        }

        recorder.stop()

        let duration = Date().timeIntervalSince(startTime)
        let fileSize = (try? FileManager.default.attributesOfItem(atPath: recorder.url.path))?[.size] as? Int ?? 0

        return AudioRecordingResult(
            success: true,
            durationSeconds: duration,
            filePath: recorder.url.path,
            fileSize: fileSize,
            interruption: nil
        )
    }

    func cancelRecording() -> AudioRecordingResult {
        guard let recorder = recorder, let startTime = recordingStartTime else {
            return AudioRecordingResult(
                success: false,
                durationSeconds: 0,
                filePath: nil,
                fileSize: 0,
                interruption: lastInterruptionReason ?? "No active recording"
            )
        }

        let duration = Date().timeIntervalSince(startTime)
        let fileSize = (try? FileManager.default.attributesOfItem(atPath: recorder.url.path))?[.size] as? Int ?? 0

        recorder.stop()

        // Keep the partial file for recovery
        let result = AudioRecordingResult(
            success: false,
            durationSeconds: duration,
            filePath: recorder.url.path,
            fileSize: fileSize,
            interruption: lastInterruptionReason ?? "Cancelled"
        )

        // Clean up the recorder
        self.recorder = nil
        recordingStartTime = nil

        return result
    }

    @objc private func handleAudioInterruption(_ notification: Notification) {
        guard let userInfo = notification.userInfo,
              let typeValue = userInfo[AVAudioSession.interruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: typeValue) else {
            return
        }

        if type == .began {
            if let optionsValue = userInfo[AVAudioSession.interruptionOptionKey] as? UInt {
                let options = AVAudioSession.InterruptionOptions(rawValue: optionsValue)
                lastInterruptionReason = "Audio interrupted: \(options)"
            } else {
                lastInterruptionReason = "Audio interrupted"
            }
        } else if type == .ended {
            if let optionsValue = userInfo[AVAudioSession.interruptionOptionKey] as? UInt {
                let options = AVAudioSession.InterruptionOptions(rawValue: optionsValue)
                if options.contains(.shouldResume) {
                    _ = recorder?.record()
                }
            }
        }
    }

    @objc private func handleRouteChange(_ notification: Notification) {
        guard let userInfo = notification.userInfo,
              let reasonValue = userInfo[AVAudioSession.routeChangeReasonKey] as? UInt,
              let reason = AVAudioSession.RouteChangeReason(rawValue: reasonValue) else {
            return
        }

        switch reason {
        case .unknown:
            lastInterruptionReason = "Route changed: unknown"
        case .newDeviceAvailable:
            lastInterruptionReason = "Route changed: new device"
        case .oldDeviceUnavailable:
            lastInterruptionReason = "Route changed: device unavailable"
        case .categoryChange:
            lastInterruptionReason = "Route changed: category"
        case .override:
            lastInterruptionReason = "Route changed: override"
        case .wakeFromSleep:
            lastInterruptionReason = "Route changed: wake from sleep"
        case .noSuitableRouteForCategory:
            lastInterruptionReason = "Route changed: no suitable route"
        case .routeConfigurationChange:
            lastInterruptionReason = "Route changed: configuration"
        @unknown default:
            lastInterruptionReason = "Route changed: unknown reason"
        }
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        if !flag {
            lastInterruptionReason = "Recording finished with error"
        }
    }
}
