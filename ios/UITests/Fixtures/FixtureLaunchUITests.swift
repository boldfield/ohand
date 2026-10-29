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
        XCTAssertTrue(app.waitForFixtureReady(timeout: seedingTimeout))
        XCTAssertFalse(app.element(identifier: "home.recents.locked").exists)
        XCTAssertFalse(app.anyHomeCard.exists)
    }

    func testLockedShowsTheLockedCardAndUnlocksOnTheUnlockAction() {
        XCTAssertTrue(app.launch(fixture: .locked))
        // The fixture is unlocked while it fills the store and locked again before it reports ready, so the unlock
        // below cannot be undone by seeding.
        XCTAssertTrue(app.waitForFixtureReady())
        XCTAssertTrue(app.element(identifier: "home.recents.locked").waitForExistence(timeout: seedingTimeout))
        let unlock = app.element(identifier: "home.recents.unlock")
        var tapsLeft = 4
        while tapsLeft > 0, !app.homeCard(containing: "Synthetic").exists {
            if unlock.exists { unlock.tap() }
            tapsLeft -= 1
            _ = app.homeCard(containing: "Synthetic").waitForExistence(timeout: 20)
        }
        XCTAssertTrue(app.homeCard(containing: "Synthetic").exists, "the unlock action showed the seeded notes (taps left: \(tapsLeft))")
    }

    func testPopulatedShowsTypedAndSpokenNotes() {
        XCTAssertTrue(app.launch(fixture: .populated))
        XCTAssertTrue(app.waitForFixtureReady())
        XCTAssertTrue(app.homeCard(containing: "Synthetic typed note about the garden hose").waitForExistence(timeout: seedingTimeout))
        XCTAssertTrue(app.homeCard(containing: "Synthetic spoken action to water the ferns").waitForExistence(timeout: seedingTimeout))
    }

    func testNeedsCorrectionShowsTheReminderNote() {
        XCTAssertTrue(app.launch(fixture: .needsCorrection))
        XCTAssertTrue(app.waitForFixtureReady())
        XCTAssertTrue(app.homeCard(containing: "call the plumber").waitForExistence(timeout: seedingTimeout))
    }

    func testFailedSortingShowsTheRejectedNote() {
        XCTAssertTrue(app.launch(fixture: .failedSorting))
        XCTAssertTrue(app.waitForFixtureReady())
        XCTAssertTrue(app.homeCard(containing: "synthetic rejected quotes").waitForExistence(timeout: seedingTimeout))
    }

    func testOfflineShowsTheWaitingNotes() {
        XCTAssertTrue(app.launch(fixture: .offline))
        XCTAssertTrue(app.waitForFixtureReady())
        XCTAssertTrue(app.homeCard(containing: "Synthetic waiting note about the garden gate").waitForExistence(timeout: seedingTimeout))
    }

    // MARK: Each seeded note is saved once

    /// Once the fixture is ready, each seeded note's text is on exactly one home card. The cards are counted by distinct
    /// identifier, which is the item ID, so a note saved under a second capture ID shows as a second identifier.
    private func assertEachNoteHasOneCard(_ texts: [String], file: StaticString = #filePath, line: UInt = #line) {
        for text in texts {
            XCTAssertTrue(
                app.homeCard(containing: text).waitForExistence(timeout: seedingTimeout), "no card for '\(text)'",
                file: file, line: line)
            let identifiers = app.homeCardIdentifiers(containing: text)
            XCTAssertEqual(
                identifiers.count, 1, "'\(text)' is on \(identifiers.sorted()); \(app.fixtureDiagnostics)",
                file: file, line: line)
        }
    }

    func testFailedSortingSavesItsNoteOnce() {
        XCTAssertTrue(app.launch(fixture: .failedSorting))
        XCTAssertTrue(app.waitForFixtureReady())
        assertEachNoteHasOneCard(["Compare the synthetic rejected quotes for the shed roof"])
    }

    func testPopulatedSavesEachNoteOnce() {
        XCTAssertTrue(app.launch(fixture: .populated))
        XCTAssertTrue(app.waitForFixtureReady())
        assertEachNoteHasOneCard([
            "Synthetic typed note about the garden hose",
            "Synthetic typed idea about a herb planter",
            "Synthetic typed action to sweep the porch",
            "to call mom",
            "Synthetic spoken note about the bookshelf",
            "Synthetic spoken idea about a window box",
            "Synthetic spoken action to water the ferns",
        ])
    }
}
