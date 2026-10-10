import Foundation
import XCTest
@testable import OhAndCoreBridge

/// The JSON the bridge sends to and reads from the core's transcription exports.
final class TranscriptionWireFormatTests: XCTestCase {
    private func json(_ outcome: TranscriptionOutcomeRequest) throws -> [String: Any] {
        let data = try JSONEncoder().encode(outcome)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func testATranscriptOutcomeEncodesTheResultAndTheTranscriptObject() throws {
        let outcome = TranscriptionOutcomeRequest(
            jobID: "job-1", leaseAttempt: 2, audioReference: "FinalizedAudio/clip-1.m4a",
            result: .transcript(RecognizedTranscriptRequest(
                text: "hello", confidence: 0.5, detectedLanguage: nil, recognizer: "r/1", audioSHA256: "ab")),
            interpretation: InterpretationJobRequest(jobID: "job-2", profileVersion: nil, requestVersion: "v1"))

        let fields = try json(outcome)

        XCTAssertEqual(fields["job_id"] as? String, "job-1")
        XCTAssertEqual(fields["lease_attempt"] as? Int, 2)
        XCTAssertEqual(fields["audio_reference"] as? String, "FinalizedAudio/clip-1.m4a")
        XCTAssertEqual(fields["result"] as? String, "transcript")
        let transcript = try XCTUnwrap(fields["transcript"] as? [String: Any])
        XCTAssertEqual(transcript["text"] as? String, "hello")
        XCTAssertEqual(transcript["confidence"] as? Double, 0.5)
        XCTAssertEqual(transcript["recognizer"] as? String, "r/1")
        XCTAssertEqual(transcript["audio_sha256"] as? String, "ab")
        XCTAssertNil(transcript["detected_language"])
        let interpretation = try XCTUnwrap(fields["interpretation"] as? [String: Any])
        XCTAssertEqual(interpretation["job_id"] as? String, "job-2")
        XCTAssertEqual(interpretation["request_version"] as? String, "v1")
        XCTAssertEqual(Set(fields.keys), ["job_id", "lease_attempt", "audio_reference", "result", "transcript", "interpretation"])
    }

    func testUnsupportedAndFailedOutcomesCarryNoTranscript() throws {
        for (result, label) in [(TranscriptionResultRequest.unsupported, "unsupported"), (.failed, "failed")] {
            let fields = try json(TranscriptionOutcomeRequest(
                jobID: "job-1", leaseAttempt: 1, audioReference: "FinalizedAudio/clip-1.m4a", result: result))

            XCTAssertEqual(fields["result"] as? String, label)
            XCTAssertEqual(Set(fields.keys), ["job_id", "lease_attempt", "audio_reference", "result"])
        }
    }

    func testAnAcknowledgmentDecodesEveryDisposition() throws {
        for disposition in ["attached", "already_attached", "pending", "discarded"] {
            let body = """
                {"operation_id":7,"item_id":"i","job_id":"j","disposition":"\(disposition)","revision":1,\
                "transcription_state":"transcribed","discard_reason":null,\
                "interpretation":{"status":"queued","job_id":"j2","denial":null}}
                """
            let acknowledgment = try JSONDecoder().decode(TranscriptionApplyAcknowledgment.self, from: Data(body.utf8))

            XCTAssertEqual(acknowledgment.disposition.rawValue, disposition)
            XCTAssertEqual(acknowledgment.interpretation?.status, "queued")
            XCTAssertEqual(acknowledgment.interpretation?.jobID, "j2")
        }
    }
}
