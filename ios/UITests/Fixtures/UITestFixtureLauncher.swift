import XCTest

/// Launches the app in one of its debug-only fixture states. The names are the ones `UITestFixture` in the app accepts
/// after `-OhAndUITestFixture`; each launch runs on a fresh temporary store with scripted adapters.
enum OhAndFixture: String, CaseIterable {
    case emptyFirstRun = "empty-first-run"
    case locked
    case populated
    case needsCorrection = "needs-correction"
    case failedSorting = "failed-sorting"
    case offline
}

extension XCUIApplication {
    /// The launch argument that selects a fixture.
    static let fixtureLaunchArgument = "-OhAndUITestFixture"

    /// Launches the app with `fixture` and waits for the capture composer, which every fixture shows.
    @discardableResult
    func launch(fixture: OhAndFixture, timeout: TimeInterval = 90) -> Bool {
        launchArguments += [Self.fixtureLaunchArgument, fixture.rawValue]
        launch()
        return element(identifier: "home.composer.field").waitForExistence(timeout: timeout)
    }

    /// Waits until the fixture is fully seeded: the app's `ui-fixture.status` marker reads `ready`. Fails with the marker's
    /// text (`failed: <reason>`) when seeding broke, so a half-filled store never reaches the test body.
    @discardableResult
    func waitForFixtureReady(timeout: TimeInterval = 150) -> Bool {
        let marker = element(identifier: "ui-fixture.status")
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if marker.exists {
                let status = marker.label
                if status == "ready" { return true }
                if status.hasPrefix("failed") {
                    XCTFail("The fixture did not seed: \(status)")
                    return false
                }
            }
            Thread.sleep(forTimeInterval: 0.5)
        }
        XCTFail("The fixture was not ready within \(timeout) seconds (status: \(marker.exists ? marker.label : "no marker"))")
        return false
    }

    /// Any element carrying an accessibility identifier.
    func element(identifier: String) -> XCUIElement {
        descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Any home card.
    var anyHomeCard: XCUIElement {
        descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'home.recents.card.'")).firstMatch
    }

    /// The first home card whose text contains `phrase`.
    func homeCard(containing phrase: String) -> XCUIElement {
        descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'home.recents.card.' AND label CONTAINS %@", phrase))
            .firstMatch
    }
}
