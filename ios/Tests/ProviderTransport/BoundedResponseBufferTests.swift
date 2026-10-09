import XCTest
@testable import OhAndServices

final class BoundedResponseBufferTests: XCTestCase {
    func testChunkLargerThanTheLimitIsRefusedWithoutBeingBuffered() {
        var buffer = BoundedResponseBuffer(limit: 8)
        XCTAssertFalse(buffer.append(Data(count: 1_000_000)))
        XCTAssertEqual(buffer.count, 0)
    }

    func testBufferNeverExceedsTheLimitAcrossChunks() {
        var buffer = BoundedResponseBuffer(limit: 10)
        XCTAssertTrue(buffer.append(Data(count: 6)))
        XCTAssertFalse(buffer.append(Data(count: 5)))
        XCTAssertEqual(buffer.count, 6)
        XCTAssertTrue(buffer.append(Data(count: 4)))
        XCTAssertEqual(buffer.count, 10)
        XCTAssertFalse(buffer.append(Data(count: 1)))
        XCTAssertEqual(buffer.count, 10)
    }

    func testExactLimitIsAcceptedAndDiscardEmptiesTheBuffer() {
        var buffer = BoundedResponseBuffer(limit: 4)
        XCTAssertTrue(buffer.append(Data([1, 2, 3, 4])))
        XCTAssertEqual(buffer.contents, Data([1, 2, 3, 4]))
        buffer.discard()
        XCTAssertEqual(buffer.count, 0)
    }

    func testNegativeLimitAcceptsNothing() {
        var buffer = BoundedResponseBuffer(limit: -5)
        XCTAssertFalse(buffer.append(Data([1])))
        XCTAssertEqual(buffer.count, 0)
    }
}
