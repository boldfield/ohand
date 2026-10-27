import XCTest

/// Checks the upgraded app through its UI, as a person would see it: the seeded captures and the correction are shown,
/// the deleted capture is in neither the recents nor a search, and the reminder still shows the time it was set for. The
/// data itself is checked by `tools/release/verify_upgrade_report.py` from the app's own upgrade report.
final class UpgradeVerifyUITests: UpgradeUITestCase {
    func testUpgradedAppShowsTheSeededData() throws {
        let expectations = try readExpectations()
        launchApp()

        let keptCard = requireStableCard(containing: "alpha")
        XCTAssertEqual(itemID(ofCard: keptCard), expectations.kept.itemID)
        let correctedCard = requireStableCard(containing: "bravo corrected")
        XCTAssertEqual(itemID(ofCard: correctedCard), expectations.corrected.itemID)
        XCTAssertFalse(card(containing: "bravo: water").exists, "the original words of the corrected capture are still shown")
        let reminderCard = requireStableCard(containing: "delta")
        XCTAssertEqual(itemID(ofCard: reminderCard), expectations.reminder.itemID)
        XCTAssertFalse(card(containing: "charlie").exists, "the deleted capture is back in the recents")
        XCTAssertFalse(
            elements(withIdentifierPrefix: "home.recents.card.\(expectations.deleted.itemID)").firstMatch.exists,
            "the deleted item has a card")

        try assertSearch("charlie", findsNothing: true)
        try assertSearch("plumber", findsNothing: false)

        // The reminder shows the time it was set for before the upgrade.
        openItem(card: reminderCard)
        let stored = storedReminderTime
        if !appears(stored, timeout: screenTimeout) { recordFailureScreen("reminder-stored-time") }
        XCTAssertTrue(appears(stored, timeout: 1), "the reminder shows no stored time after the upgrade")
        XCTAssertEqual(stored.label, expectations.reminder.storedTimeLabel, "the reminder's time changed in the upgrade")
    }

    private func assertSearch(_ query: String, findsNothing: Bool) throws {
        dismissKeyboard()
        let queryField = app.textFields["retrieval.search.query"].firstMatch
        openFromHome("home.search", name: "Search", shows: queryField)
        tap(queryField, step: "type the search")
        queryField.typeText(query)
        let searchKey = app.keyboards.buttons["Search"]
        XCTAssertTrue(appears(searchKey, timeout: screenTimeout), "the keyboard Search key did not appear")
        tap(searchKey, step: "run the search")
        let count = element("retrieval.search.count")
        let empty = element("retrieval.search.empty")
        let answered = Date().addingTimeInterval(screenTimeout)
        while !count.exists && !empty.exists && Date() < answered { Thread.sleep(forTimeInterval: 0.5) }
        if findsNothing {
            XCTAssertTrue(empty.exists, "searching for \(query) did not report no matches")
            XCTAssertFalse(count.exists, "searching for \(query) found a result")
        } else {
            XCTAssertTrue(count.exists, "searching for \(query) found nothing")
        }
        goBack(step: "from Search")
    }
}
