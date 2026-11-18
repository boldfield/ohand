import Foundation

/// One recognizer callback result reduced to plain values, so accumulation needs no speech framework.
///
/// `audioStart` and `audioEnd` come from the first and last `SFTranscriptionSegment` (timestamp, and timestamp plus
/// duration); both are nil when the result has no segments.
struct RecognitionResultValue: Equatable {
    var text: String
    var segmentConfidences: [Float]
    var audioStart: TimeInterval?
    var audioEnd: TimeInterval?
    var isFinal: Bool
}

/// Joins the per-segment results of one recognition into a transcript.
///
/// On-device recognition reports one result per segment (a pause of a few seconds, or about a minute of speech,
/// ends a segment) and only the last is final, so keeping the final result alone drops everything said earlier. A
/// result that starts before the end of an accepted result restates it, as a recognizer that reports cumulatively
/// does, and replaces every accepted result it overlaps.
struct RecognitionAccumulator {
    private struct AcceptedResult {
        var text: String
        var segmentConfidences: [Float]
        var audioStart: TimeInterval
        var audioEnd: TimeInterval
    }

    private var acceptedResults: [AcceptedResult] = []
    /// Every result passed to `add`, including those that contributed nothing.
    private(set) var receivedCount = 0

    var hasAcceptedResults: Bool { !acceptedResults.isEmpty }

    /// The accepted texts in audio order, each trimmed, joined by a single space.
    var transcript: String {
        acceptedResults.map(\.text).joined(separator: " ")
    }

    /// The scores of the segments of every accepted result, in audio order.
    var segmentConfidences: [Float] {
        acceptedResults.flatMap(\.segmentConfidences)
    }

    mutating func add(_ result: RecognitionResultValue) {
        receivedCount += 1
        let trimmedText = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedText.isEmpty, let audioStart = result.audioStart, let audioEnd = result.audioEnd else {
            return
        }
        // Strict comparisons, as specified: a result that starts before an accepted one ends restates it. Results that
        // only touch, or that lie wholly before or after the new one, stay.
        acceptedResults.removeAll { $0.audioEnd > audioStart && $0.audioStart < audioEnd }
        let accepted = AcceptedResult(
            text: trimmedText,
            segmentConfidences: result.segmentConfidences,
            audioStart: audioStart,
            audioEnd: audioEnd)
        let insertionIndex = acceptedResults.firstIndex { $0.audioStart > audioStart } ?? acceptedResults.endIndex
        acceptedResults.insert(accepted, at: insertionIndex)
    }
}
