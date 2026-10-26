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
        return element(identifier: "capture-entry.composer").waitForExistence(timeout: timeout)
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
