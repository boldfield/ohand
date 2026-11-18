import Foundation
import Speech

extension RecognitionResultValue {
    init(_ result: SFSpeechRecognitionResult) {
        let transcription = result.bestTranscription
        let segments = transcription.segments
        self.init(
            text: transcription.formattedString,
            segmentConfidences: segments.map(\.confidence),
            audioStart: segments.first?.timestamp,
            audioEnd: segments.last.map { $0.timestamp + $0.duration },
            isFinal: result.isFinal)
    }
}
