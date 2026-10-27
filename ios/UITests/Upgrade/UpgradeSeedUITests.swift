import XCTest

/// Seeds the installed (prior) app with synthetic data by driving its UI, so every write goes through the app's own
/// services: four text captures, one correction, one deletion, one explicitly scheduled reminder and a configured
/// provider that processing routes to. Nothing here touches a database, a Keychain or a file of the app. It then writes
/// `seed-expectations.json` for the verifying run.
final class UpgradeSeedUITests: UpgradeUITestCase {
    func testSeedSyntheticData() throws {
        launchApp()
        for text in [keptText, correctedOriginalText, deletedText, reminderText] {
            saveCapture(text)
        }
        dismissKeyboard()

        let keptCard = requireStableCard(containing: "alpha")
        let correctedCard = requireStableCard(containing: "bravo")
        let deletedCard = requireStableCard(containing: "charlie")
        let reminderCard = requireStableCard(containing: "delta")
        let keptID = itemID(ofCard: keptCard)
        let correctedID = itemID(ofCard: correctedCard)
        let deletedID = itemID(ofCard: deletedCard)
        let reminderID = itemID(ofCard: reminderCard)
        XCTAssertEqual(Set([keptID, correctedID, deletedID, reminderID]).count, 4, "each capture is its own item")

        try correct(card: correctedCard)
        try delete(card: deletedCard)
        let storedTimeLabel = try scheduleReminder(card: reminderCard)
        try configureProvider()

        try write(
            UpgradeExpectations(
                kept: .init(itemID: keptID, textLength: keptText.count),
                corrected: .init(
                    itemID: correctedID, originalTextLength: correctedOriginalText.count,
                    correctedTextLength: correctedFinalText.count),
                deleted: .init(itemID: deletedID),
                reminder: .init(itemID: reminderID, textLength: reminderText.count, storedTimeLabel: storedTimeLabel),
                provider: .init(profileName: providerName)))
    }

    // MARK: Captures

    private func saveCapture(_ text: String) {
        let field = element("home.composer.field")
        XCTAssertTrue(appears(field, timeout: screenTimeout), "the composer did not appear")
        let save = app.buttons["home.composer.save"]
        let saveIsReady = NSPredicate(format: "exists == true AND isEnabled == true")
        var typed = false
        for _ in 1...3 where !typed {
            tap(field, step: "type capture")
            XCTAssertTrue(appears(app.keyboards.firstMatch, timeout: screenTimeout), "the keyboard did not appear")
            field.typeText(text)
            let enabled = XCTNSPredicateExpectation(predicate: saveIsReady, object: save)
            typed = XCTWaiter().wait(for: [enabled], timeout: 30) == .completed
        }
        if !typed { recordFailureScreen("capture-not-ready") }
        XCTAssertTrue(typed, "Save did not appear enabled after typing a capture")
        tap(save, step: "save capture")
        XCTAssertTrue(disappears(save, timeout: 30), "the Save button was still showing 30 s after it was tapped")
    }

    // MARK: Correction

    private func correct(card cardElement: XCUIElement) throws {
        openItem(card: cardElement)
        let field = element("item-controls.text.field")
        XCTAssertTrue(appears(field, timeout: screenTimeout), "the correction field did not appear")
        tap(field, step: "edit the words")
        // The caret goes to the end of the words, then everything is deleted and the correction typed.
        field.coordinate(withNormalizedOffset: CGVector(dx: 0.98, dy: 0.5)).tap()
        XCTAssertTrue(appears(app.keyboards.firstMatch, timeout: screenTimeout), "the keyboard did not appear")
        let existing = (field.value as? String) ?? correctedOriginalText
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: existing.count + 4))
        field.typeText(correctedFinalText)
        let save = element("item-controls.text.save")
        tap(save, step: "save the corrected words")
        let outcome = element("item-controls.outcome")
        XCTAssertTrue(appears(outcome, timeout: screenTimeout), "no outcome was shown for the correction")
        XCTAssertTrue(outcome.label.hasPrefix("Saved"), "the correction was not saved: \(outcome.label)")
        dismissKeyboard()
        goBack(step: "from the corrected item")
        requireStableCard(containing: "bravo corrected")
    }

    // MARK: Deletion

    private func delete(card cardElement: XCUIElement) throws {
        openItem(card: cardElement)
        tap(element("data-settings.delete-item.delete"), step: "ask to delete the item")
        tap(element("data-settings.delete-item.confirm"), step: "confirm the deletion")
        let state = element("data-settings.delete-item.state")
        if !appears(state, timeout: screenTimeout) { recordFailureScreen("deletion-state") }
        XCTAssertTrue(appears(state, timeout: 1), "the deletion showed no result")
        goBack(step: "from the deleted item")
        XCTAssertTrue(disappears(card(containing: "charlie"), timeout: screenTimeout), "the deleted item is still in the recents")
    }

    // MARK: Reminder

    private func scheduleReminder(card cardElement: XCUIElement) throws -> String {
        openItem(card: cardElement)
        tap(element("item-controls.type.picker"), step: "open the type menu")
        let actionChoice = app.buttons["Action"].firstMatch
        XCTAssertTrue(appears(actionChoice, timeout: screenTimeout), "the type menu did not offer Action")
        actionChoice.tap()
        tap(element("item-controls.type.apply"), step: "apply the type")
        let outcome = element("item-controls.outcome")
        XCTAssertTrue(appears(outcome, timeout: screenTimeout), "no outcome was shown for the type change")
        XCTAssertTrue(outcome.label.hasPrefix("Saved"), "the type change was not saved: \(outcome.label)")
        dismissKeyboard()

        // The reminder section belongs to an action; open the item again if it did not appear on its own.
        var saveTime = element("reminder.save")
        if !appears(saveTime, timeout: 20) {
            goBack(step: "to reopen the action")
            openItem(card: requireStableCard(containing: "delta"))
            saveTime = element("reminder.save")
        }
        tap(saveTime, step: "save the reminder time")
        allowNotificationsIfAsked()
        let stored = element("reminder.stored-time")
        if !appears(stored, timeout: screenTimeout) { recordFailureScreen("reminder-stored-time") }
        XCTAssertTrue(appears(stored, timeout: 1), "the reminder showed no stored time")
        let label = stored.label
        XCTAssertFalse(label.isEmpty, "the stored time was empty")
        // Give the reminder pass a moment to hand the request to the system.
        Thread.sleep(forTimeInterval: 5)
        goBack(step: "from the reminder item")
        return label
    }

    // MARK: Provider

    private func configureProvider() throws {
        openFromHome("home.settings", name: "Settings", shows: app.staticTexts["Settings"])
        openSettingsRow("Processing", screen: "Processing")
        tap(app.buttons["provider-settings.add-provider"], step: "open Add provider")
        let nameField = element("provider-settings.field.name")
        XCTAssertTrue(appears(nameField, timeout: screenTimeout), "the Add provider form did not open")
        tap(nameField, step: "type the profile name")
        nameField.typeText(providerName)
        let modelField = element("provider-settings.field.model")
        tap(modelField, step: "type the model")
        modelField.typeText(providerModel)
        let secretField = element("provider-settings.field.secret")
        tap(secretField, step: "type the secret")
        secretField.typeText(providerSecret)
        tap(element("provider-settings.store-secret"), step: "store the secret")
        let credentialState = element("provider-settings.credential-state")
        if !appears(credentialState, timeout: screenTimeout) { recordFailureScreen("credential-state") }
        XCTAssertTrue(appears(credentialState, timeout: 1), "storing the secret showed no result")
        tap(element("provider-settings.save"), step: "save the profile")
        let saveState = element("provider-settings.save-state")
        if !appears(saveState, timeout: screenTimeout) { recordFailureScreen("save-state") }
        XCTAssertTrue(appears(saveState, timeout: 1), "saving the profile showed no result")
        goBack(step: "from Add provider")

        let cloud = element("provider-settings.choice.cloud")
        tap(cloud, step: "choose the cloud provider")
        let allowanceNote = element("provider-settings.privacy-allowance")
        if !appears(allowanceNote, timeout: screenTimeout) { recordFailureScreen("privacy-allowance-note") }
        XCTAssertTrue(appears(allowanceNote, timeout: 1), "choosing the provider did not ask for the privacy allowance")
        goBack(step: "from Processing")

        openSettingsRow("Privacy", screen: "Privacy")
        let offer = elements(withIdentifierPrefix: "privacy-settings.offer.").firstMatch
        if !appears(offer, timeout: screenTimeout) { recordFailureScreen("privacy-offer") }
        XCTAssertTrue(appears(offer, timeout: 1), "Privacy offered no allowance for the provider")
        tap(offer, step: "ask to allow processing through the provider")
        tap(element("privacy-settings.allow"), step: "allow processing")
        let actionState = element("privacy-settings.action-state")
        if !appears(actionState, timeout: screenTimeout) { recordFailureScreen("privacy-action-state") }
        XCTAssertTrue(appears(actionState, timeout: 1), "allowing processing showed no result")
        goBack(step: "from Privacy")
        goBack(step: "from Settings")
    }

    private func openSettingsRow(_ label: String, screen: String) {
        let row = app.buttons.matching(
            NSPredicate(format: "label == %@ OR label BEGINSWITH %@", label, "\(label), ")
        ).firstMatch
        XCTAssertTrue(appears(row, timeout: screenTimeout), "settings row \(label) not found")
        tap(row, step: "open \(label)")
        XCTAssertTrue(appears(app.staticTexts[screen].firstMatch, timeout: screenTimeout), "\(screen) did not open")
    }
}
