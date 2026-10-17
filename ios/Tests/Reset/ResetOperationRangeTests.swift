import XCTest
@testable import OhAndServices

final class ResetOperationRangeTests: XCTestCase {
    func testTheResetRangeIsReservedAndDisjointFromTheOthers() throws {
        let ranges = ServiceCompositionRoot.reservedOperationRanges
        let mine = try XCTUnwrap(ranges.first { $0.name == "reset" })
        XCTAssertEqual(mine.start, CoreResetClient.firstOperationID)
        let overlapping = ranges.filter { $0.name != mine.name && $0.start < mine.end && mine.start < $0.end }
        XCTAssertTrue(overlapping.isEmpty, "overlaps \(overlapping.map(\.name))")
    }

    func testTheResetAndReminderClientsStartAtDifferentOperationIDs() {
        XCTAssertNotEqual(CoreResetClient.firstOperationID, CoreReminderReconciler.firstOperationID)
    }
}
