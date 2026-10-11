import XCTest
@testable import OhAndServices

final class NotificationIdentifierTests: XCTestCase {
    func testIdentifierRoundTripsThroughItsRawValue() throws {
        let reminderID = "3f2b8c1e-5d4a-4e0b-9a77-0c1d2e3f4a5b"
        let identifier = try XCTUnwrap(NotificationIdentifier(reminderID: reminderID, scheduleGeneration: 3))

        XCTAssertEqual(identifier.rawValue, "\(reminderID)#3", "the form the core derives: reminder id, '#', generation")
        let parsed = try XCTUnwrap(NotificationIdentifier(rawValue: identifier.rawValue))
        XCTAssertEqual(parsed, identifier)
        XCTAssertEqual(parsed.reminderID.rawValue, reminderID)
        XCTAssertEqual(parsed.scheduleGeneration, 3)
    }

    func testGenerationZeroAndLargestGenerationRoundTrip() throws {
        for generation in [Int64(0), 1, Int64.max] {
            let identifier = try XCTUnwrap(NotificationIdentifier(reminderID: "reminder-1", scheduleGeneration: generation))
            XCTAssertEqual(NotificationIdentifier(rawValue: identifier.rawValue), identifier)
        }
    }

    func testRawValuesOutsideTheCanonicalFormAreRejected() {
        let rejected = [
            "",
            "#1",
            "reminder-1",
            "reminder-1#",
            "reminder-1#-1",
            "reminder-1#+1",
            "reminder-1#01",
            "reminder-1#1.0",
            "reminder-1#1#2",
            "reminder-1# 1",
            "reminder-1#9223372036854775808",
            "reminder 1#1",
            "reminder/1#1",
            "r\u{e9}minder#1",
            String(repeating: "a", count: OpaqueIdentifier.maximumLength + 1) + "#1",
        ]
        for rawValue in rejected {
            XCTAssertNil(NotificationIdentifier(rawValue: rawValue), "\(rawValue.debugDescription) must be rejected")
        }
    }

    func testNegativeGenerationIsRejected() {
        XCTAssertNil(NotificationIdentifier(reminderID: "reminder-1", scheduleGeneration: -1))
    }

    func testOpaqueIdentifierAcceptsOnlyTheBoundedAsciiAlphabet() {
        XCTAssertNotNil(OpaqueIdentifier("7d1c5a1e-0000-4000-8000-000000000001"))
        XCTAssertNotNil(OpaqueIdentifier("action_snooze.v1"))
        XCTAssertNotNil(OpaqueIdentifier(String(repeating: "x", count: OpaqueIdentifier.maximumLength)))

        let rejected = ["", " ", "has space", "semi;colon", "at@sign", "colon:separated", "line\nbreak",
                        "nul\0byte", "\u{1F4A1}", String(repeating: "x", count: OpaqueIdentifier.maximumLength + 1)]
        for candidate in rejected {
            XCTAssertNil(OpaqueIdentifier(candidate), "\(candidate.debugDescription) must be rejected")
        }
    }
}
