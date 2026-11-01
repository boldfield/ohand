import XCTest

final class ThrowawayFailureDemoTests: XCTestCase {
    func testDeliberateFailureWithAttachment() {
        let attachment = XCTAttachment(string: "ci03 throwaway failure attachment")
        attachment.name = "ci03-demo"
        attachment.lifetime = .keepAlways
        add(attachment)
        XCTFail("CI03 deliberate failure to show the failure export")
    }
}
