import XCTest
import OhAndCoreBridge
import OhAndServices
@testable import OhAnd

final class ProjectSmokeTests: XCTestCase {
    func testHostApplicationIsProductionApp() {
        XCTAssertEqual(Bundle.main.bundleIdentifier, "com.boldfield.ohand.app")
        XCTAssertEqual(ContentView().bundleIdentifier, "com.boldfield.ohand.app")
    }

    func testServicesAndBridgeFrameworksLink() {
        XCTAssertEqual(OhAndServices.version, "0.1.0")
        XCTAssertEqual(OhAndCore.version, "0.1.0")
    }
}
