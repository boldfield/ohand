import XCTest

/// Opens the app in each named fixture and checks one element that identifies it.
final class FixtureLaunchUITests: XCTestCase {
    private let app = XCUIApplication()
    /// Seeding runs the production save, provider setup and interpretation paths, so the first card can take a while.
    private let seedingTimeout: TimeInterval = 90

    override func setUp() {
        continueAfterFailure = false
    }

    func testEmptyFirstRunShowsAnEmptyComposerAndNoCards() {
        XCTAssertTrue(app.launch(fixture: .emptyFirstRun))
        XCTAssertFalse(app.element(identifier: "home.recents.locked").exists)
        XCTAssertFalse(app.anyHomeCard.exists)
    }

    func testLockedShowsTheLockedCardAndUnlocksOnTheUnlockAction() {
        XCTAssertTrue(app.launch(fixture: .locked))
        XCTAssertTrue(app.element(identifier: "home.recents.locked").waitForExistence(timeout: seedingTimeout))
        app.element(identifier: "home.recents.unlock").tap()
        XCTAssertTrue(app.homeCard(containing: "Synthetic").waitForExistence(timeout: seedingTimeout))
    }

    func testPopulatedShowsTypedAndSpokenNotes() {
        XCTAssertTrue(app.launch(fixture: .populated))
        XCTAssertTrue(app.homeCard(containing: "Synthetic typed note about the garden hose").waitForExistence(timeout: seedingTimeout))
        XCTAssertTrue(app.homeCard(containing: "Synthetic spoken action to water the ferns").waitForExistence(timeout: seedingTimeout))
    }

    func testNeedsCorrectionShowsTheReminderNote() {
        XCTAssertTrue(app.launch(fixture: .needsCorrection))
        XCTAssertTrue(app.homeCard(containing: "call the plumber").waitForExistence(timeout: seedingTimeout))
    }

    func testFailedSortingShowsTheRejectedNote() {
        XCTAssertTrue(app.launch(fixture: .failedSorting))
        XCTAssertTrue(app.homeCard(containing: "synthetic rejected quotes").waitForExistence(timeout: seedingTimeout))
    }

    func testOfflineShowsTheWaitingNotes() {
        XCTAssertTrue(app.launch(fixture: .offline))
        XCTAssertTrue(app.homeCard(containing: "Synthetic waiting note about the garden gate").waitForExistence(timeout: seedingTimeout))
    }
}
