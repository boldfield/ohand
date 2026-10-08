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
    var hadInterruptionGap: Bool = false

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
            hadInterruptionGap = false

            if recorder?.record() ?? false {
                return true
            } else {
                recorder = nil
                recordingStartTime = nil
                lastInterruptionReason = "Recording start failed"
                return false
            }
        } catch {
            lastInterruptionReason = "Record start failed: \(error)"
            recorder = nil
            recordingStartTime = nil
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

        if let interruptionReason = lastInterruptionReason {
            defer {
                self.recorder = nil
                recordingStartTime = nil
            }
            let filePath = recorder.url.path
            let fileSize = getFileSize(at: recorder.url)

            var isRecoverable = false
            if fileSize > 0 {
                if let audioFile = try? AVAudioFile(forReading: recorder.url) {
                    isRecoverable = audioFile.length > 0
                }
            }

            if isRecoverable {
                return AudioRecordingResult(
                    success: false,
                    durationSeconds: duration,
                    filePath: filePath,
                    fileSize: fileSize,
                    interruption: interruptionReason
                )
            } else {
                return AudioRecordingResult(
                    success: false,
                    durationSeconds: duration,
                    filePath: nil,
                    fileSize: 0,
                    interruption: interruptionReason
                )
            }
        }

        let filePath = recorder.url.path
        let fileSize = getFileSize(at: recorder.url)

        var isRecoverable = false
        if fileSize > 0 {
            if let audioFile = try? AVAudioFile(forReading: recorder.url) {
                isRecoverable = audioFile.length > 0
            }
        }

        defer {
            self.recorder = nil
            recordingStartTime = nil
        }

        if hadInterruptionGap {
            if isRecoverable {
                return AudioRecordingResult(
                    success: false,
                    durationSeconds: duration,
                    filePath: filePath,
                    fileSize: fileSize,
                    interruption: "Interrupted and resumed"
                )
            } else {
                return AudioRecordingResult(
                    success: false,
                    durationSeconds: duration,
                    filePath: nil,
                    fileSize: 0,
                    interruption: "Interrupted and resumed"
                )
            }
        }

        if isRecoverable {
            return AudioRecordingResult(
                success: true,
                durationSeconds: duration,
                filePath: filePath,
                fileSize: fileSize,
                interruption: nil
            )
        } else {
            return AudioRecordingResult(
                success: false,
                durationSeconds: duration,
                filePath: nil,
                fileSize: 0,
                interruption: "File not recoverable"
            )
        }
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

        recorder.stop()

        let duration = Date().timeIntervalSince(startTime)
        let filePath = recorder.url.path
        let fileSize = getFileSize(at: recorder.url)

        var isRecoverable = false
        if fileSize > 0 {
            if let audioFile = try? AVAudioFile(forReading: recorder.url) {
                isRecoverable = audioFile.length > 0
            }
        }

        defer {
            self.recorder = nil
            recordingStartTime = nil
        }

        if isRecoverable {
            return AudioRecordingResult(
                success: false,
                durationSeconds: duration,
                filePath: filePath,
                fileSize: fileSize,
                interruption: "Cancelled"
            )
        } else {
            return AudioRecordingResult(
                success: false,
                durationSeconds: duration,
                filePath: nil,
                fileSize: 0,
                interruption: "Cancelled"
            )
        }
    }

    private func getFileSize(at url: URL) -> Int {
        return (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? Int ?? 0
    }

    @objc private func handleAudioInterruption(_ notification: Notification) {
        guard let userInfo = notification.userInfo,
              let typeValue = userInfo[AVAudioSession.interruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: typeValue) else {
            return
        }

        if type == .began {
            lastInterruptionReason = "Audio interrupted"
        } else if type == .ended {
            if let optionsValue = userInfo[AVAudioSession.interruptionOptionKey] as? UInt {
                let options = AVAudioSession.InterruptionOptions(rawValue: optionsValue)
                if options.contains(.shouldResume) {
                    hadInterruptionGap = true
                    if recorder?.record() ?? false {
                        lastInterruptionReason = nil
                    }
                }
            }
        }
    }

    @objc private func handleRouteChange(_ notification: Notification) {
    }

    func audioRecorderDidFinishRecording(_ recorder: AVAudioRecorder, successfully flag: Bool) {
        if !flag {
            lastInterruptionReason = "Recording finished with error"
        }
    }
}
