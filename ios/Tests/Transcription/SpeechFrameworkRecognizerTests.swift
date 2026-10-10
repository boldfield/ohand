import Foundation
import XCTest
@testable import OhAndServices

private final class FakeRecognitionTask: RecognitionCancelling, @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0

    var cancelCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return count
    }

    func cancel() {
        lock.lock()
        count += 1
        lock.unlock()
    }
}

/// The recognizer's pure decisions, and the session that resumes a recognition exactly once. The speech framework
/// itself cannot be made to produce speech in a simulator test; whether it recognizes offline is device evidence.
final class SpeechFrameworkRecognizerTests: XCTestCase {
    // MARK: error mapping

    func testNoSpeechDetectedIsAnEmptyTranscript() {
        let error = NSError(domain: "kAFAssistantErrorDomain", code: 1110)
        XCTAssertEqual(SpeechFrameworkRecognizer.outcome(for: error), .transcript(text: "", confidence: nil))
    }

    func testAFrameworkRaisedCancellationIsTransientNotAnInterruption() {
        let error = NSError(domain: "kAFAssistantErrorDomain", code: 216)
        XCTAssertEqual(
            SpeechFrameworkRecognizer.outcome(for: error), .unavailableNow(reason: "recognition_ended_by_system"))
    }

    func testOtherErrorsAreRecognitionFailures() {
        XCTAssertEqual(SpeechFrameworkRecognizer.outcome(for: NSError(domain: "kAFAssistantErrorDomain", code: 203)), .failed)
        XCTAssertEqual(SpeechFrameworkRecognizer.outcome(for: NSError(domain: "other", code: 216)), .failed)
        XCTAssertEqual(SpeechFrameworkRecognizer.outcome(for: NSError(domain: "other", code: 1110)), .failed)
    }

    // MARK: confidence

    func testConfidenceIsTheMeanOfTheSegmentScores() throws {
        let mean = try XCTUnwrap(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: [0.5, 1.0]))
        XCTAssertEqual(mean, 0.75, accuracy: 0.0001)
    }

    func testUnscoredOrEmptySegmentsReportNoConfidence() {
        XCTAssertNil(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: []))
        XCTAssertNil(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: [0, 0]))
    }

    func testConfidenceIsClampedToTheUnitInterval() throws {
        let mean = try XCTUnwrap(SpeechFrameworkRecognizer.confidence(ofSegmentConfidences: [1.5, 2.0]))
        XCTAssertEqual(mean, 1.0, accuracy: 0.0001)
    }

    // MARK: session

    func testTheFirstFinishWinsAndLaterOnesAreIgnored() async {
        let session = RecognitionSession(keepingAlive: NSObject())
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            XCTAssertTrue(session.install(continuation))
            session.finish(.transcript(text: "water the plants", confidence: 0.9))
            session.finish(.failed)
            session.cancel()
        }
        XCTAssertEqual(outcome, .transcript(text: "water the plants", confidence: 0.9))
    }

    func testCancellingBeforeInstallResumesCancelledAndRefusesToStart() async {
        let session = RecognitionSession(keepingAlive: NSObject())
        session.cancel()
        var installed = true
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            installed = session.install(continuation)
        }
        XCTAssertFalse(installed)
        XCTAssertEqual(outcome, .cancelled)
    }

    func testCancellingAfterAttachStopsTheTaskAndResumesCancelledOnce() async {
        let session = RecognitionSession(keepingAlive: NSObject())
        let task = FakeRecognitionTask()
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            XCTAssertTrue(session.install(continuation))
            session.attach(task)
            session.cancel()
            session.cancel()
            session.finish(.failed)
        }
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertGreaterThanOrEqual(task.cancelCount, 1)
    }

    func testATaskAttachedAfterCancellationIsStoppedAtOnce() async {
        let session = RecognitionSession(keepingAlive: NSObject())
        let task = FakeRecognitionTask()
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            XCTAssertTrue(session.install(continuation))
            session.cancel()
            session.attach(task)
        }
        XCTAssertEqual(outcome, .cancelled)
        XCTAssertEqual(task.cancelCount, 1)
    }

    func testATaskAttachedAfterTheRecognitionEndedIsStopped() async {
        let session = RecognitionSession(keepingAlive: NSObject())
        let task = FakeRecognitionTask()
        let outcome = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            XCTAssertTrue(session.install(continuation))
            session.finish(.failed)
            session.attach(task)
        }
        XCTAssertEqual(outcome, .failed)
        XCTAssertEqual(task.cancelCount, 1)
    }

    func testTheRecognizerIsHeldUntilTheRecognitionEnds() async {
        weak var weakRecognizer: NSObject?
        let session: RecognitionSession = {
            let recognizer = NSObject()
            weakRecognizer = recognizer
            return RecognitionSession(keepingAlive: recognizer)
        }()
        XCTAssertNotNil(weakRecognizer, "the session keeps the recognizer alive while recognition runs")
        _ = await withCheckedContinuation { (continuation: CheckedContinuation<OnDeviceRecognitionOutcome, Never>) in
            XCTAssertTrue(session.install(continuation))
            XCTAssertNotNil(weakRecognizer)
            session.finish(.failed)
        }
        XCTAssertNil(weakRecognizer, "the recognizer is released once the recognition ends")
    }
}
