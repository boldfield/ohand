import UIKit
import XCTest
@testable import OhAnd

@MainActor
final class FullScreenLaunchTests: XCTestCase {
    func testAppRunsAtNativeScreenSizeNotLegacyCompatibilityMode() throws {
        let windowScene = try XCTUnwrap(
            UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first,
            "hosted tests must run with a connected window scene"
        )
        let keyWindow = try XCTUnwrap(
            windowScene.windows.first(where: \.isKeyWindow) ?? windowScene.windows.first,
            "hosted app must have a window"
        )
        let screenBounds = UIScreen.main.bounds
        XCTAssertEqual(keyWindow.windowScene?.coordinateSpace.bounds, screenBounds)
        XCTAssertGreaterThan(screenBounds.height, 667, "legacy letterboxed mode reports a 667-point-tall screen")
    }
}
