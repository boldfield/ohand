import Foundation
import XCTest

class HandoffValidatorTests: XCTestCase {
    func testValidCaptureHandoff() {
        let url = URL(string: "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)

        switch result {
        case .success(let request):
            XCTAssertEqual(request.captureId, "550e8400-e29b-41d4-a716-446655440000")
            XCTAssertEqual(request.route, .capture)
        case .failure:
            XCTFail("Valid handoff should not fail")
        }
    }

    func testMissingCaptureId() {
        let url = URL(string: "ohand-tauri://capture")!
        let result = HandoffValidator.validate(url: url)
        XCTAssertEqual(result, .failure(.missingCaptureId))
    }

    func testEmptyCaptureId() {
        let url = URL(string: "ohand-tauri://capture?captureId=")!
        let result = HandoffValidator.validate(url: url)
        XCTAssertEqual(result, .failure(.missingCaptureId))
    }

    func testInvalidRoute() {
        let url = URL(string: "ohand-tauri://invalid?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)
        XCTAssertEqual(result, .failure(.invalidRoute))
    }

    func testMissingRoute() {
        let url = URL(string: "ohand-tauri://?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)
        XCTAssertEqual(result, .failure(.missingRoute))
    }

    func testWrongScheme() {
        let url = URL(string: "ohand://capture?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)
        XCTAssertEqual(result, .failure(.invalidURL))
    }

    func testSchemeIsCaseInsensitive() {
        let url = URL(string: "OHAND-TAURI://capture?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)

        switch result {
        case .success(let request):
            XCTAssertEqual(request.route, .capture)
        case .failure:
            XCTFail("Case-insensitive scheme should work")
        }
    }

    func testMaliciousRouteInjection() {
        let maliciousUrl = URL(string: "ohand-tauri://../../settings?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: maliciousUrl)
        XCTAssertEqual(result, .failure(.invalidRoute))
    }

    func testMultipleCaptureIdParameters() {
        let url = URL(string: "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000&captureId=evil")!
        let result = HandoffValidator.validate(url: url)

        switch result {
        case .success(let request):
            XCTAssertEqual(request.captureId, "550e8400-e29b-41d4-a716-446655440000")
        case .failure:
            XCTFail("Should use first captureId parameter")
        }
    }

    func testTimestampIsSet() {
        let url = URL(string: "ohand-tauri://capture?captureId=550e8400-e29b-41d4-a716-446655440000")!
        let result = HandoffValidator.validate(url: url)

        switch result {
        case .success(let request):
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let parsed = formatter.date(from: request.timestamp)
            XCTAssertNotNil(parsed, "Timestamp should be a valid ISO8601 date")
        case .failure:
            XCTFail("Valid handoff should not fail")
        }
    }
}
