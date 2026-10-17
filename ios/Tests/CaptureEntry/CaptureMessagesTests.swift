import XCTest
@testable import OhAnd

final class CaptureMessagesTests: XCTestCase {
    private func message(
        _ source: CaptureAcknowledgmentSource, _ save: CaptureSaveState, earlyEnd: VoiceRecordingEnd? = nil
    ) -> String? {
        CaptureMessages.message(for: CaptureAcknowledgment(source: source, save: save, progress: nil, earlyEnd: earlyEnd))
    }

    func testAConfirmedSaveAndASavingStateHaveNoMessage() {
        XCTAssertNil(message(.text, .saving))
        XCTAssertNil(message(.text, .saved(itemID: "item-1", alreadySaved: false)))
        XCTAssertNil(message(.audio, .saved(itemID: "item-1", alreadySaved: true)))
    }

    func testEveryNonSaveOutcomeIsStillReported() {
        let states: [CaptureSaveState] = [
            .keptOnDevice, .notSaved, .itemDeleted, .cancelled, .unreadable(retained: true), .unreadable(retained: false),
        ]
        for source in [CaptureAcknowledgmentSource.text, .audio] {
            for state in states {
                XCTAssertNotNil(message(source, state), "\(source) \(state) is reported")
            }
        }
    }

    func testKeptForRetryAndUnrecoverableAreStatedHonestly() {
        XCTAssertEqual(message(.audio, .notSaved), "Not saved. The recording is kept on this phone.")
        XCTAssertEqual(message(.text, .notSaved), "Not saved. Your text is still here.")
        XCTAssertEqual(message(.audio, .keptOnDevice), "Kept on this phone. Not confirmed saved yet.")
        XCTAssertEqual(message(.audio, .unreadable(retained: false)), "Nothing was recorded.")
        XCTAssertEqual(
            message(.audio, .unreadable(retained: true)), "Couldn't read the recording. What was captured is kept.")
    }

    func testAnEarlyEndIsNotedAndAUserStopIsNot() {
        XCTAssertEqual(
            message(.audio, .saved(itemID: "item-1", alreadySaved: false), earlyEnd: .durationLimit),
            "Stopped at the time limit. The recording is saved.")
        XCTAssertEqual(
            message(.audio, .notSaved, earlyEnd: .deviceLocking),
            "Stopped when the phone locked. Not saved. The recording is kept on this phone.")
        XCTAssertNil(message(.audio, .saved(itemID: "item-1", alreadySaved: false), earlyEnd: .stoppedByUser))
    }

    func testTheTimerCountsWholeSeconds() {
        let start = Date(timeIntervalSince1970: 1_000)
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start), "0:00")
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start.addingTimeInterval(75.9)), "1:15")
        XCTAssertEqual(CaptureMessages.elapsedText(since: start, now: start.addingTimeInterval(-5)), "0:00")
    }
}
