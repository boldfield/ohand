import AVFoundation
import Foundation

/// Renders the fixed synthetic phrase to an audio file with `AVSpeechSynthesizer`, so the probe has speech audio
/// that contains no personal content. Completion runs on the main thread with nil on success or a failure reason.
final class SyntheticSpeechFixtureGenerator {
    private let synthesizer = AVSpeechSynthesizer()
    private var outputFile: AVAudioFile?
    private var framesWritten: AVAudioFrameCount = 0
    private var failureReason: String?
    private var finished = false

    func generate(to destination: URL, completion: @escaping (String?) -> Void) {
        outputFile = nil
        framesWritten = 0
        failureReason = nil
        finished = false
        try? FileManager.default.removeItem(at: destination)

        let utterance = AVSpeechUtterance(string: TranscriptionFixture.phrase)
        utterance.voice = AVSpeechSynthesisVoice(language: "en-US")
        synthesizer.write(utterance) { [weak self] buffer in
            guard let self = self else { return }
            guard let pcmBuffer = buffer as? AVAudioPCMBuffer else {
                self.finish(completion, extraFailure: "Synthesizer returned a non-PCM buffer")
                return
            }
            if pcmBuffer.frameLength == 0 {
                self.finish(completion, extraFailure: nil)
                return
            }
            do {
                if self.outputFile == nil {
                    self.outputFile = try AVAudioFile(forWriting: destination, settings: pcmBuffer.format.settings)
                }
                try self.outputFile?.write(from: pcmBuffer)
                self.framesWritten += pcmBuffer.frameLength
            } catch {
                self.failureReason = "Writing fixture failed: \(error.localizedDescription)"
            }
        }
    }

    private func finish(_ completion: @escaping (String?) -> Void, extraFailure: String?) {
        if finished { return }
        finished = true
        outputFile = nil
        let reason = failureReason ?? extraFailure ?? (framesWritten == 0 ? "Synthesizer produced no audio" : nil)
        DispatchQueue.main.async {
            completion(reason)
        }
    }
}
