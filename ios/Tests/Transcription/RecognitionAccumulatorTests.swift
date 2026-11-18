import Foundation
import XCTest
@testable import OhAndServices

/// The accumulator over value fixtures that follow the on-device results observed on iOS 27.2.1 (see F69 in
/// docs/features/m1-task-refinement.md). They are synthetic values; no test here recognizes audio.
final class RecognitionAccumulatorTests: XCTestCase {
    private func result(
        _ text: String, from start: TimeInterval?, to end: TimeInterval?, isFinal: Bool = false,
        confidences: [Float] = [0.9]
    ) -> RecognitionResultValue {
        RecognitionResultValue(
            text: text, segmentConfidences: confidences, audioStart: start, audioEnd: end, isFinal: isFinal)
    }

    private func transcript(of results: [RecognitionResultValue]) -> String {
        var accumulator = RecognitionAccumulator()
        results.forEach { accumulator.add($0) }
        return accumulator.transcript
    }

    private func assertOnce(_ sentence: String, in text: String, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(text.components(separatedBy: sentence).count - 1, 1, "\(sentence) in \(text)", file: file, line: line)
    }

    // MARK: observed sequences

    func testPausedTwentyOneSecondFixtureKeepsBothSegments() {
        let text = transcript(of: [
            result("First sentence before the pause.", from: 0.00, to: 8.55),
            result("Last sentence after the pause.", from: 11.61, to: 20.88, isFinal: true),
        ])
        assertOnce("First sentence before the pause.", in: text)
        assertOnce("Last sentence after the pause.", in: text)
        XCTAssertEqual(text, "First sentence before the pause. Last sentence after the pause.")
    }

    func testContinuousNinetyFiveSecondFixtureKeepsBothSegments() {
        let text = transcript(of: [
            result("Sentence one of twenty. Sentence ten.", from: 0.00, to: 60.06),
            result("Sentence eleven. Sentence twenty.", from: 60.27, to: 95.01, isFinal: true),
        ])
        assertOnce("Sentence one of twenty.", in: text)
        assertOnce("Sentence twenty.", in: text)
    }

    func testFinalResultAloneIsTheWholeTranscript() {
        XCTAssertEqual(
            transcript(of: [result("Call the dentist", from: 0.2, to: 2.5, isFinal: true)]), "Call the dentist")
    }

    // MARK: ordering, restatement and empties

    func testThreeOrMoreSegmentsJoinWithASingleSpaceInOrder() {
        XCTAssertEqual(
            transcript(of: [
                result("  one ", from: 0, to: 3),
                result("two", from: 5, to: 8),
                result("three\n", from: 10, to: 12),
                result("four", from: 15, to: 17, isFinal: true),
            ]),
            "one two three four")
    }

    func testACumulativeRecognizerYieldsItsLastTextOnce() {
        XCTAssertEqual(
            transcript(of: [
                result("buy", from: 0, to: 0.5),
                result("buy milk", from: 0, to: 1.0),
                result("buy milk and eggs", from: 0, to: 2.0, isFinal: true),
            ]),
            "buy milk and eggs")
    }

    func testARestatementReplacesOnlyTheResultsItOverlaps() {
        XCTAssertEqual(
            transcript(of: [
                result("alpha", from: 0, to: 3),
                result("beta", from: 5, to: 8),
                result("beta gamma", from: 5, to: 11, isFinal: true),
            ]),
            "alpha beta gamma")
    }

    func testARestatementLeavesALaterNonOverlappingResultInPlace() {
        XCTAssertEqual(
            transcript(of: [
                result("alpha", from: 0, to: 3),
                result("beta", from: 5, to: 8),
                result("gamma", from: 2, to: 4, isFinal: true),
            ]),
            "gamma beta")
    }

    func testEmptyAndSegmentlessResultsContributeNothing() {
        var accumulator = RecognitionAccumulator()
        accumulator.add(result("start", from: 0, to: 2))
        accumulator.add(result("   ", from: 3, to: 4))
        accumulator.add(result("ghost", from: nil, to: nil))
        accumulator.add(result("end", from: 6, to: 8, isFinal: true))
        XCTAssertEqual(accumulator.transcript, "start end")
        XCTAssertEqual(accumulator.receivedCount, 4)
        XCTAssertTrue(accumulator.hasAcceptedResults)
    }

    func testNothingAcceptedMeansAnEmptyTranscript() {
        var accumulator = RecognitionAccumulator()
        accumulator.add(result("", from: nil, to: nil, isFinal: true))
        XCTAssertEqual(accumulator.transcript, "")
        XCTAssertFalse(accumulator.hasAcceptedResults)
    }

    // MARK: confidence

    func testConfidenceIsTheMeanOverEveryAcceptedSegment() throws {
        var accumulator = RecognitionAccumulator()
        accumulator.add(result("a", from: 0, to: 1, confidences: [0.2, 0.4]))
        accumulator.add(result("b", from: 3, to: 4, confidences: [1.0]))
        accumulator.add(result("dropped", from: nil, to: nil, confidences: [0.0]))
        XCTAssertEqual(accumulator.segmentConfidences, [0.2, 0.4, 1.0])
        let mean = try XCTUnwrap(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: accumulator.segmentConfidences))
        XCTAssertEqual(mean, 0.5333, accuracy: 0.001)
    }

    func testReplacedResultsDoNotCountTowardsConfidence() {
        var accumulator = RecognitionAccumulator()
        accumulator.add(result("a", from: 0, to: 1, confidences: [0.1]))
        accumulator.add(result("a b", from: 0, to: 2, confidences: [0.8, 0.8]))
        XCTAssertEqual(accumulator.segmentConfidences, [0.8, 0.8])
    }

    func testNoScoredSegmentsMeansNoConfidence() {
        var accumulator = RecognitionAccumulator()
        accumulator.add(result("a", from: 0, to: 1, confidences: [0, 0]))
        XCTAssertNil(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: accumulator.segmentConfidences))
    }
}
